//! Workflow Catalog RPC Service
//!
//! Provides versioned workflow metadata queries over `nagisalake-rpc`.
//! This is the first internal service extracted from the Hub monolith.
//!
//! ## Design Principles
//!
//! 1. **Read-only**: No mutations, all writes stay in the Hub
//! 2. **Stateless**: Only queries PostgreSQL, no in-memory state
//! 3. **Permission-aware**: Respects user device access grants
//! 4. **Paginated**: Uses cursor-based pagination for large catalogs
//!
//! ## RPC Protocol
//!
//! Wire format: 4-byte BE length prefix + Bincode 2 payload
//! Transport: TCP with `SO_NODELAY` (localhost or LAN)
//!
//! Future optimization: Zero-copy via CapnProto + Unix domain sockets

use nagisalake_hub_store::{PgStore, StoredWorkflow};
use nagisalake_rpc::{Code, Principal, ServerContext, Service, Status};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Request to list workflows visible to a user.
///
/// The caller's identity is taken from [`ServerContext::principal`], not from
/// this request. The `user_id` field is retained only to keep the wire shape
/// stable for clients built before the fix; it is ignored on the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListWorkflowsRequest {
    /// Deprecated: ignored. Identity comes from the authenticated principal.
    pub user_id: String,
    /// Maximum items per page (clamped to 1-200).
    pub limit:   Option<i64>,
    /// Opaque cursor from previous response.
    pub cursor:  Option<String>,
}

/// Response containing one page of workflows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListWorkflowsResponse {
    /// Workflows visible to the requesting user.
    pub items:       Vec<WorkflowMetadata>,
    /// Pass this back as `cursor` for the next page. `None` = end.
    pub next_cursor: Option<String>,
}

/// Workflow version metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowMetadata {
    pub organization_id:   String,
    pub workflow_id:       String,
    pub version:           String,
    /// ComfyUI JSON manifest, if available.
    pub manifest_json:     Option<String>,
    /// Output artifact types this workflow produces.
    pub output_types_json: String,
    /// SHA-256 of the manifest for drift detection.
    pub content_hash:      Option<String>,
}

impl From<StoredWorkflow> for WorkflowMetadata {
    fn from(stored: StoredWorkflow) -> Self {
        Self {
            organization_id:   stored.organization_id,
            workflow_id:       stored.workflow_id,
            version:           stored.version,
            manifest_json:     stored.manifest_json,
            output_types_json: stored.output_types_json,
            content_hash:      stored.content_hash,
        }
    }
}

/// Request to get a specific workflow version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetWorkflowRequest {
    pub organization_id: String,
    pub workflow_id:     String,
    pub version:         String,
}

/// The workflow catalog service.
#[derive(Clone)]
pub struct WorkflowCatalog {
    store: Arc<PgStore>,
}

impl WorkflowCatalog {
    /// Creates a new catalog service backed by PostgreSQL.
    pub fn new(store: PgStore) -> Self {
        Self {
            store: Arc::new(store),
        }
    }
}

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn decode_cursor(cursor: &str) -> Result<(String, String), Status> {
    let invalid = || Status::new(Code::InvalidArgument, "cursor is not valid");
    let raw = data_encoding::BASE64URL_NOPAD
        .decode(cursor.as_bytes())
        .map_err(|_| invalid())?;
    let decoded = String::from_utf8(raw).map_err(|_| invalid())?;
    let parts: Vec<&str> = decoded.split('\0').collect();
    if parts.len() != 2 || parts.iter().any(|p| p.is_empty()) {
        return Err(invalid());
    }
    Ok((parts[0].to_owned(), parts[1].to_owned()))
}

fn encode_cursor(workflow_id: &str, version: &str) -> String {
    let payload = format!("{}\0{}", workflow_id, version);
    data_encoding::BASE64URL_NOPAD.encode(payload.as_bytes())
}

impl Service<ServerContext, ListWorkflowsRequest> for WorkflowCatalog {
    type Response = ListWorkflowsResponse;
    type Error = Status;

    async fn call(
        &self,
        cx: &mut ServerContext,
        request: ListWorkflowsRequest,
    ) -> Result<Self::Response, Self::Error> {
        // Identity is never taken from the request payload. A missing principal
        // means no server-side layer authenticated the caller, so the request
        // is refused rather than falling back to the client-supplied field.
        let principal: &Principal = cx
            .principal()
            .ok_or_else(|| Status::new(Code::Unauthenticated, "no authenticated principal"))?;
        let organization_id = principal.organization_id().ok_or_else(|| {
            Status::new(Code::PermissionDenied, "organization context is required")
        })?;

        let limit = clamp_limit(request.limit);
        let after = request.cursor.as_deref().map(decode_cursor).transpose()?;

        let mut items = self
            .store
            .workflows_for_user_devices_page(
                principal.user_id(),
                organization_id,
                limit + 1,
                after
                    .as_ref()
                    .map(|(wid, ver)| (wid.as_str(), ver.as_str())),
            )
            .await
            .map_err(|error| Status::new(Code::Internal, error.to_string()))?;

        let has_more = items.len() > limit as usize;
        if has_more {
            items.pop();
        }

        let next_cursor = has_more
            .then(|| items.last())
            .flatten()
            .map(|w| encode_cursor(&w.workflow_id, &w.version));

        Ok(ListWorkflowsResponse {
            items: items.into_iter().map(Into::into).collect(),
            next_cursor,
        })
    }
}
