//! Tokio Hub for reverse-connected Nagisalake workers.
//!
//! The Hub owns the public job API and the single-instance session directory.
//! Workers always connect outbound over WebSocket/SMUX. Control frames carry
//! JSON metadata; input and output media use S3-compatible presigned URLs.
//!
//! ## Key Components
//!
//! - [`HubConfig`]: authentication, listener, transport, and object-store settings.
//! - [`SessionRegistry`]: current `worker_id -> session` mapping with command ACKs.
//! - [`router`]: public and worker control routes, useful for embedding/tests.
//! - [`serve`]: Tokio listener and graceful shutdown wiring.

mod oauth;
mod product_api;
mod ratelimit;
mod web_ui;

use axum::{
    Json, Router,
    extract::{MatchedPath, Path as AxumPath, State, WebSocketUpgrade},
    http::{
        HeaderMap, Method, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    },
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use faststr::FastStr;
use nagisalake_core::JobState;
use nagisalake_hub_auth::{Permission, Principal, PrincipalKind, Role, hash_secret};
use nagisalake_hub_store::{
    ArtifactUpsert, CommitJobResult, CompleteJobOutputUpload, ConditionalJobUpdate,
    DeviceUseAdmission, DispatchOutbox, EventInsert, IdempotencyInsert, JobEventUpdate, JobUpsert,
    PgStore, StoreConfig, StoreError, UploadRequestUpsert, WorkerUpsert, WorkflowUpsert,
    device_workflow_allowed,
};
use nagisalake_object_store::{ObjectMetadata, ObjectStore, S3ObjectStoreConfig};
use nagisalake_protocol::{
    ArtifactReady, ArtifactUpload, ArtifactUploaded, ArtifactUploadedAck, CancelJob, CommandAck,
    DispatchJob, Heartbeat, HubMessage, JobEvent, JobEventAck, JobEventKind, JobInput,
    ProtocolError, Registered, Validate, WorkerCapabilities, WorkerMessage, WorkflowCapability,
    WorkflowManifest,
};
use nagisalake_transport::{
    DEFAULT_MAX_CONTROL_FRAME_BYTES, HubTransport, TOKILAKE_SUBPROTOCOL, TransportError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

#[path = "hub/api.rs"]
mod api;
#[path = "hub/api_helpers.rs"]
mod api_helpers;
#[path = "hub/config.rs"]
mod config;
#[path = "hub/dispatch.rs"]
mod dispatch;
#[path = "hub/jobs.rs"]
mod jobs;
#[path = "hub/maintenance.rs"]
mod maintenance;
#[path = "hub/metrics.rs"]
mod metrics;
#[path = "hub/runtime.rs"]
mod runtime;
#[path = "hub/scheduler.rs"]
mod scheduler;
#[path = "hub/sessions.rs"]
mod sessions;
#[path = "hub/state.rs"]
mod state;
#[path = "hub/worker.rs"]
mod worker;

pub use self::{api::*, api_helpers::*, config::*, runtime::*, sessions::*, state::*};
use self::{dispatch::*, jobs::*, maintenance::*, metrics::*, scheduler::*, worker::*};

#[cfg(test)]
#[path = "hub/tests/mod.rs"]
mod tests;
