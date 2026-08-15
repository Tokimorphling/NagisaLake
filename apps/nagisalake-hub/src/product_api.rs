//! Browser and programmatic control-plane HTTP API.

use super::{
    AppState, CompleteUploadRequest, CreateUploadRequest, HubError, Permission, Principal,
    PrincipalKind, Role, SubmitJobRequest, accessible_workers_for, aggregate_workflows,
    bearer_token, cancel_job_for_principal, complete_upload_for_principal,
    create_upload_for_principal, download_for_principal, job_for_principal,
    jobs_page_for_principal, now_unix_ms, readable_artifact_for_principal,
    submit_job_for_principal, truncate,
};
use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, Path, Query, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{LOCATION, RETRY_AFTER, SET_COOKIE},
    },
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{delete, get, patch, post},
};
use futures_util::stream;
use nagisalake_hub_auth::{
    GeneratedSecret, generate_secret, hash_password_async, hash_secret, verify_password_async,
    verify_secret,
};
use nagisalake_hub_store::{
    ApiKey, AuditInsert, DeviceWorkflowRule, NewApiKey, NewDeviceInvite, NewOrganizationInvite,
    NewSession, NewWorkerCredential, PgStore, QuotaPolicyUpdate, RotateSession, StoreError,
    WorkerCredential, device_workflow_allowed,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    convert::Infallible,
    net::SocketAddr,
    time::Duration,
};
use tracing::warn;
use uuid::Uuid;

mod authentication;
mod batches;
mod devices_workflows;
mod gallery;
mod jobs_artifacts;
mod media;
mod organizations;
mod shared;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) fn is_same_origin(origin: &str, headers: &HeaderMap) -> bool {
    authentication::is_same_origin(origin, headers)
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/settings/public", get(public_settings))
        .route("/api/v1/openapi.yaml", get(openapi_spec))
        .route("/api/v1/auth/register", post(authentication::register))
        .route("/api/v1/auth/login", post(authentication::login))
        .route("/api/v1/auth/refresh", post(authentication::refresh))
        .route("/api/v1/auth/logout", post(authentication::logout))
        .route(
            "/api/v1/auth/revoke-all-sessions",
            post(authentication::revoke_all_sessions),
        )
        .route(
            "/api/v1/auth/me",
            get(authentication::me).delete(authentication::delete_account),
        )
        .route(
            "/api/v1/auth/oauth/providers",
            get(authentication::list_oauth_providers),
        )
        .route(
            "/api/v1/auth/oauth/{provider}/start",
            get(authentication::start_oauth),
        )
        .route(
            "/api/v1/auth/oauth/{provider}/callback",
            get(authentication::oauth_callback),
        )
        .route(
            "/api/v1/auth/identities",
            get(authentication::list_linked_identities),
        )
        .route(
            "/api/v1/organizations",
            get(organizations::list_organizations).post(organizations::create_organization),
        )
        .route(
            "/api/v1/organizations/{org_id}/members",
            get(organizations::list_members),
        )
        .route(
            "/api/v1/organizations/{org_id}/members/{user_id}",
            patch(organizations::change_member_role).delete(organizations::remove_member),
        )
        .route(
            "/api/v1/organizations/{org_id}/member-invites",
            get(organizations::list_organization_invites)
                .post(organizations::create_organization_invite),
        )
        .route(
            "/api/v1/organizations/{org_id}/member-invites/{invite_id}",
            delete(organizations::revoke_organization_invite),
        )
        .route(
            "/api/v1/organization-invitations/accept",
            post(organizations::accept_organization_invite),
        )
        .route(
            "/api/v1/organizations/{org_id}/owner-transfer",
            post(organizations::transfer_organization_owner),
        )
        .route(
            "/api/v1/organizations/{org_id}/export",
            get(organizations::export_organization),
        )
        .route(
            "/api/v1/organizations/{org_id}",
            delete(organizations::delete_organization),
        )
        .route(
            "/api/v1/organizations/{org_id}/api-keys",
            get(organizations::list_api_keys).post(organizations::create_api_key),
        )
        .route(
            "/api/v1/organizations/{org_id}/api-keys/{key_id}",
            delete(organizations::revoke_api_key),
        )
        .route(
            "/api/v1/organizations/{org_id}/quota",
            get(organizations::get_quota).patch(organizations::update_quota),
        )
        .route(
            "/api/v1/organizations/{org_id}/audit-logs",
            get(organizations::list_audit_logs),
        )
        .route(
            "/api/v1/organizations/{org_id}/worker-credentials",
            get(organizations::list_worker_credentials)
                .post(organizations::create_worker_credential),
        )
        .route(
            "/api/v1/organizations/{org_id}/worker-credentials/{credential_id}",
            delete(organizations::revoke_worker_credential),
        )
        .route("/api/v1/devices", get(devices_workflows::list_devices))
        .route(
            "/api/v1/device-invites",
            post(devices_workflows::create_device_invite),
        )
        .route(
            "/api/v1/device-invites/{invite_id}",
            delete(devices_workflows::revoke_device_invite),
        )
        .route(
            "/api/v1/device-invitations/accept",
            post(devices_workflows::accept_device_invite),
        )
        .route(
            "/api/v1/devices/shares/revoke",
            post(devices_workflows::revoke_device_share),
        )
        .route("/api/v1/workflows", get(devices_workflows::list_workflows))
        .route(
            "/api/v1/artifacts/uploads",
            post(jobs_artifacts::create_artifact_upload),
        )
        .route(
            "/api/v1/artifacts/uploads/{artifact_id}/complete",
            post(jobs_artifacts::complete_artifact_upload),
        )
        .route(
            "/api/v1/artifacts/{artifact_id}/download",
            get(jobs_artifacts::download_artifact),
        )
        .route(
            "/api/v1/artifacts/{artifact_id}/content",
            get(media::artifact_content),
        )
        .route(
            "/api/v1/gallery/items",
            get(gallery::list_gallery_items).post(gallery::publish_gallery_item),
        )
        .route(
            "/api/v1/gallery/items/{item_id}",
            get(gallery::get_gallery_item).delete(gallery::unpublish_gallery_item),
        )
        .route(
            "/api/v1/gallery/items/{item_id}/content",
            get(gallery::gallery_item_content),
        )
        .route(
            "/api/v1/gallery/items/{item_id}/download",
            get(gallery::gallery_item_download),
        )
        .route(
            "/api/v1/jobs",
            get(jobs_artifacts::list_jobs).post(jobs_artifacts::submit_job),
        )
        .route(
            "/api/v1/jobs/{job_id}/events",
            get(jobs_artifacts::stream_job_events),
        )
        .route(
            "/api/v1/jobs/{job_id}",
            get(jobs_artifacts::get_job).delete(jobs_artifacts::cancel_job),
        )
        .route(
            "/api/v1/job-batches",
            get(batches::list_batches).post(batches::create_batch),
        )
        .route(
            "/api/v1/job-batches/{batch_id}",
            get(batches::get_batch).delete(batches::cancel_batch),
        )
        .route(
            "/api/v1/job-batches/{batch_id}/jobs",
            get(batches::list_batch_jobs),
        )
}

#[derive(Debug, Serialize)]
struct PublicSettings {
    registration_enabled:  bool,
    password_auth_enabled: bool,
    max_artifact_bytes:    u64,
    authentication:        [&'static str; 3],
    oauth_providers:       Vec<authentication::PublicProvider>,
}

async fn public_settings(State(state): State<AppState>) -> Json<PublicSettings> {
    Json(PublicSettings {
        registration_enabled:  state.store.is_some() && state.config.browser.registration_enabled,
        password_auth_enabled: state.config.browser.password_auth_enabled,
        max_artifact_bytes:    state.config.transport.max_artifact_bytes,
        authentication:        ["browser_session", "api_key", "oauth"],
        oauth_providers:       state
            .oauth_providers
            .iter()
            .map(|(name, provider)| authentication::PublicProvider {
                name: name.clone(),
                kind: provider.kind,
            })
            .collect(),
    })
}

async fn openapi_spec() -> Response {
    (
        StatusCode::OK,
        [("content-type", "application/yaml; charset=utf-8")],
        include_str!("../../../docs/openapi.yaml"),
    )
        .into_response()
}
