use super::{authentication::authorize_current, shared::*, *};
use nagisalake_hub_store::{
    BatchChildJob, BatchIdempotencyInsert, BatchInsert, BatchJobCounts, CommitBatchResult,
    DeviceUseAdmission,
};

const BATCH_PAGE_DEFAULT: i64 = 20;
const BATCH_PAGE_MAX: i64 = 100;

fn batch_status(counts: &BatchJobCounts) -> &'static str {
    let active = counts.received + counts.accepted + counts.running + counts.uploading;
    if active > 0 {
        return "running";
    }
    if counts.queued > 0 {
        return "queued";
    }
    if counts.completed > 0 && counts.failed == 0 && counts.cancelled == 0 {
        return "completed";
    }
    if counts.cancelled > 0 && counts.completed == 0 && counts.failed == 0 {
        return "cancelled";
    }
    if counts.completed == 0 && (counts.failed > 0 || counts.cancelled > 0) {
        return "failed";
    }
    if counts.completed > 0 || counts.failed > 0 || counts.cancelled > 0 {
        return "partial";
    }
    // A just-created batch can be observed between its parent and child reads.
    "queued"
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct CreateBatchApiRequest {
    workflow_id:               String,
    workflow_version:          String,
    count:                     i64,
    base_parameters:           JsonValue,
    #[serde(default)]
    items:                     Vec<BatchItemSpec>,
    #[serde(default)]
    shared_input_artifact_ids: Vec<String>,
    device_organization_id:    Option<String>,
    device_id:                 Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct BatchItemSpec {
    index:               i64,
    client_item_id:      Option<String>,
    parameter_overrides: Option<JsonValue>,
}

#[derive(Debug, Serialize)]
pub(super) struct BatchView {
    id:               String,
    workflow_id:      String,
    workflow_version: String,
    total:            i64,
    status:           String,
    counts:           BatchJobCounts,
    created_at:       i64,
}

#[derive(Debug, Serialize)]
pub(super) struct BatchJobSummary {
    id:               String,
    batch_index:      Option<i64>,
    state:            String,
    progress:         Option<f32>,
    workflow_id:      String,
    workflow_version: String,
    created_at:       i64,
}

pub(super) async fn create_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateBatchApiRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::JobsWrite).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let Some(user_id) = auth.principal.user_id.as_deref() else {
        return product_error(
            HubError::Forbidden("a user-owned credential is required".into()),
            &request_id,
        );
    };
    let store = match store(&state) {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let (Some(device_organization_id), Some(device_id)) = (
        request.device_organization_id.as_deref(),
        request.device_id.as_deref(),
    ) else {
        return product_error(
            HubError::InvalidRequest(
                "device_organization_id and device_id are required for batch jobs".into(),
            ),
            &request_id,
        );
    };
    if !auth.principal.allows(Permission::DevicesUse) {
        return product_error(
            HubError::Forbidden("missing permission devices:use".into()),
            &request_id,
        );
    }
    match store
        .can_use_device(
            user_id,
            &auth.principal.organization_id,
            device_organization_id,
            device_id,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return product_error(
                HubError::Forbidden("the selected device is not accessible".into()),
                &request_id,
            );
        }
        Err(error) => return product_error(HubError::Store(error), &request_id),
    }
    let device = match store
        .devices_for_user(user_id, &auth.principal.organization_id)
        .await
    {
        Ok(devices) => devices.into_iter().find(|device| {
            device.device_organization_id == device_organization_id && device.device_id == device_id
        }),
        Err(error) => return product_error(HubError::Store(error), &request_id),
    };
    let Some(device) = device else {
        return product_error(
            HubError::Forbidden("the selected device is not accessible".into()),
            &request_id,
        );
    };
    let offers_workflow =
        serde_json::from_str::<nagisalake_protocol::WorkerCapabilities>(&device.capabilities_json)
            .is_ok_and(|capabilities| {
                capabilities.workflows.iter().any(|workflow| {
                    workflow.id == request.workflow_id
                        && workflow.version == request.workflow_version
                })
            });
    if !offers_workflow {
        return product_error(
            HubError::InvalidRequest(
                "the selected device does not offer the requested workflow version".into(),
            ),
            &request_id,
        );
    }
    if request.count < 1 || request.count > 100 {
        return product_error(
            HubError::InvalidRequest("count must be between 1 and 100".into()),
            &request_id,
        );
    }
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let batch_id = Uuid::new_v4().to_string();
    let now = now_unix_ms();
    let base_params_json = serde_json::to_string(&request.base_parameters).unwrap_or_default();
    let job_ids: Vec<String> = (0..request.count)
        .map(|_| Uuid::new_v4().to_string())
        .collect();
    let params_jsons: Vec<String> = (0..request.count)
        .map(|index| {
            let item = request.items.iter().find(|item| item.index == index);
            let params = if let Some(item) = item {
                if let Some(overrides) = &item.parameter_overrides {
                    merge_parameters(&request.base_parameters, overrides)
                } else {
                    request.base_parameters.clone()
                }
            } else {
                request.base_parameters.clone()
            };
            serde_json::to_string(&params).unwrap_or_default()
        })
        .collect();
    let client_item_ids: Vec<Option<String>> = (0..request.count)
        .map(|index| {
            request
                .items
                .iter()
                .find(|item| item.index == index)
                .and_then(|item| item.client_item_id.clone())
        })
        .collect();
    let children: Vec<BatchChildJob> = (0..request.count as usize)
        .map(|index| BatchChildJob {
            job_id:             &job_ids[index],
            batch_index:        index as i64,
            client_item_id:     client_item_ids[index].as_deref(),
            parameters_json:    &params_jsons[index],
            input_artifact_ids: &request.shared_input_artifact_ids,
        })
        .collect();
    let request_hash = hash_secret(&serde_json::to_string(&request).unwrap_or_default());
    let idempotency = idempotency_key
        .as_deref()
        .map(|key| BatchIdempotencyInsert {
            organization_id: &auth.principal.organization_id,
            actor_kind: auth_kind(auth.principal.kind),
            actor_id: &auth.principal.actor_id,
            endpoint: "/api/v1/job-batches",
            key,
            request_hash: &request_hash,
            batch_id: &batch_id,
        });
    let _quota_guard = state.quota_guard(&auth.principal.organization_id).await;
    let result = store
        .commit_new_batch(
            BatchInsert {
                batch_id:                &batch_id,
                organization_id:         &auth.principal.organization_id,
                actor_id:                &auth.principal.actor_id,
                actor_kind:              auth_kind(auth.principal.kind),
                actor_user_id:           Some(user_id),
                workflow_id:             &request.workflow_id,
                workflow_version:        &request.workflow_version,
                workflow_content_digest: None,
                base_parameters_json:    &base_params_json,
                variation_spec_json:     "{}",
                device_organization_id:  Some(device_organization_id),
                device_id:               Some(device_id),
                total_jobs:              request.count,
                retry_of_batch_id:       None,
            },
            &children,
            &request.shared_input_artifact_ids,
            idempotency,
            Some(DeviceUseAdmission {
                organization_id: &auth.principal.organization_id,
                user_id,
                device_organization_id,
                device_id,
                workflow_id: &request.workflow_id,
                workflow_version: &request.workflow_version,
                requested_jobs: request.count,
                now,
            }),
            now,
        )
        .await;
    drop(_quota_guard);
    match result {
        Ok(CommitBatchResult::Created) => {
            audit(
                &state,
                Some(&auth.principal.organization_id),
                Some(user_id),
                auth_kind(auth.principal.kind),
                &request_id,
                "job_batch.create",
                "job_batch",
                Some(&batch_id),
                "success",
                json!({"count": request.count, "workflow_id": request.workflow_id}),
            )
            .await;
            let counts = store
                .batch_job_counts(&auth.principal.organization_id, &batch_id)
                .await
                .unwrap_or_default();
            (
                StatusCode::ACCEPTED,
                Json(BatchView {
                    id: batch_id,
                    workflow_id: request.workflow_id,
                    workflow_version: request.workflow_version,
                    total: request.count,
                    status: batch_status(&counts).into(),
                    counts,
                    created_at: now,
                }),
            )
                .into_response()
        }
        Ok(CommitBatchResult::Existing { batch_id: existing }) => {
            match store
                .job_batch(&auth.principal.organization_id, &existing)
                .await
            {
                Ok(Some(batch)) => {
                    let counts = store
                        .batch_job_counts(&auth.principal.organization_id, &existing)
                        .await
                        .unwrap_or_default();
                    Json(BatchView {
                        id: batch.id,
                        workflow_id: batch.workflow_id,
                        workflow_version: batch.workflow_version,
                        total: batch.total_jobs,
                        status: batch_status(&counts).into(),
                        counts,
                        created_at: batch.created_at,
                    })
                    .into_response()
                }
                _ => product_error(HubError::NotFound("batch".into()), &request_id),
            }
        }
        Err(error) => product_error(map_store(error), &request_id),
    }
}

pub(super) async fn list_batches(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CursorQuery>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::JobsReadOrganization).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let store = match store(&state) {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let limit = query
        .limit
        .unwrap_or(BATCH_PAGE_DEFAULT)
        .clamp(1, BATCH_PAGE_MAX);
    let after = match query.cursor.as_deref().map(decode_created_id_cursor) {
        Some(Ok(value)) => Some(value),
        Some(Err(error)) => return product_error(error, &request_id),
        None => None,
    };
    let items = match store
        .job_batches_page(
            &auth.principal.organization_id,
            limit.saturating_add(1),
            after
                .as_ref()
                .map(|(created_at, id)| (*created_at, id.as_str())),
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return product_error(map_store(error), &request_id),
    };
    let has_more = items.len() > limit as usize;
    let mut views = Vec::with_capacity(items.len().min(limit as usize));
    for batch in items.into_iter().take(limit as usize) {
        let counts = store
            .batch_job_counts(&auth.principal.organization_id, &batch.id)
            .await
            .unwrap_or_default();
        views.push(BatchView {
            id: batch.id,
            workflow_id: batch.workflow_id,
            workflow_version: batch.workflow_version,
            total: batch.total_jobs,
            status: batch_status(&counts).into(),
            counts,
            created_at: batch.created_at,
        });
    }
    let next_cursor = has_more
        .then(|| {
            views
                .last()
                .map(|b| encode_created_id_cursor(b.created_at, &b.id))
        })
        .flatten();
    Json(ListPage {
        items: views,
        next_cursor,
    })
    .into_response()
}

pub(super) async fn get_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(batch_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::JobsReadOrganization).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let store = match store(&state) {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let batch = match store
        .job_batch(&auth.principal.organization_id, &batch_id)
        .await
    {
        Ok(Some(value)) => value,
        Ok(None) => return product_error(HubError::NotFound("batch".into()), &request_id),
        Err(error) => return product_error(map_store(error), &request_id),
    };
    let counts = store
        .batch_job_counts(&auth.principal.organization_id, &batch_id)
        .await
        .unwrap_or_default();
    Json(BatchView {
        id: batch.id,
        workflow_id: batch.workflow_id,
        workflow_version: batch.workflow_version,
        total: batch.total_jobs,
        status: batch_status(&counts).into(),
        counts,
        created_at: batch.created_at,
    })
    .into_response()
}

pub(super) async fn list_batch_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(batch_id): Path<String>,
    Query(query): Query<CursorQuery>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::JobsReadOrganization).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let store = match store(&state) {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let limit = query
        .limit
        .unwrap_or(BATCH_PAGE_DEFAULT)
        .clamp(1, BATCH_PAGE_MAX);
    let jobs = match store
        .batch_jobs_page(
            &auth.principal.organization_id,
            &batch_id,
            limit.saturating_add(1),
            None,
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return product_error(map_store(error), &request_id),
    };
    let has_more = jobs.len() > limit as usize;
    let items: Vec<BatchJobSummary> = jobs
        .into_iter()
        .take(limit as usize)
        .map(|job| BatchJobSummary {
            id:               job.id,
            batch_index:      job.batch_index,
            state:            job.state,
            progress:         job.progress,
            workflow_id:      job.workflow_id,
            workflow_version: job.workflow_version,
            created_at:       job.created_at,
        })
        .collect();
    let next_cursor = has_more
        .then(|| items.last().map(|j| j.batch_index.unwrap_or(0).to_string()))
        .flatten();
    Json(ListPage { items, next_cursor }).into_response()
}

fn merge_parameters(base: &JsonValue, overrides: &JsonValue) -> JsonValue {
    let mut result = base.clone();
    if let (Some(base_obj), Some(over_obj)) = (result.as_object_mut(), overrides.as_object()) {
        for (key, value) in over_obj {
            base_obj.insert(key.clone(), value.clone());
        }
    }
    result
}

pub(super) async fn cancel_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(batch_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::JobsCancelOwn).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let store = match store(&state) {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let batch = match store
        .job_batch(&auth.principal.organization_id, &batch_id)
        .await
    {
        Ok(Some(batch)) => batch,
        Ok(None) => return product_error(HubError::NotFound("batch".into()), &request_id),
        Err(error) => return product_error(map_store(error), &request_id),
    };
    let owns_batch = auth.principal.user_id.as_deref() == batch.actor_user_id.as_deref();
    if !(auth.principal.allows(Permission::JobsCancelAny)
        || auth.principal.allows(Permission::JobsCancelOwn) && owns_batch)
    {
        return product_error(
            HubError::Forbidden("role can only cancel batches created by the same user".into()),
            &request_id,
        );
    }
    let jobs = match store
        .batch_jobs_page(
            &auth.principal.organization_id,
            &batch_id,
            BATCH_PAGE_MAX.saturating_add(1),
            None,
        )
        .await
    {
        Ok(jobs) if jobs.len() <= BATCH_PAGE_MAX as usize => jobs,
        Ok(_) => {
            return product_error(
                HubError::Conflict("batch is too large to cancel atomically".into()),
                &request_id,
            );
        }
        Err(error) => return product_error(map_store(error), &request_id),
    };
    // Reuse the single-job path for every child. It owns the permission check,
    // exact-once quota release, durable queued removal, and CancelJob delivery
    // for work that has already reached a Worker.
    for job in jobs {
        if matches!(job.state.as_str(), "completed" | "failed" | "cancelled") {
            continue;
        }
        if let Err(error) = cancel_job_for_principal(&state, &auth.principal, &job.id).await {
            return product_error(error, &request_id);
        }
    }
    audit(
        &state,
        Some(&auth.principal.organization_id),
        auth.principal.user_id.as_deref(),
        auth_kind(auth.principal.kind),
        &request_id,
        "job_batch.cancel",
        "job_batch",
        Some(&batch_id),
        "success",
        json!({}),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod status_tests {
    use super::*;

    fn counts(
        queued: i64,
        received: i64,
        completed: i64,
        failed: i64,
        cancelled: i64,
    ) -> BatchJobCounts {
        BatchJobCounts {
            queued,
            received,
            completed,
            failed,
            cancelled,
            ..BatchJobCounts::default()
        }
    }

    #[test]
    fn derives_every_batch_status_from_child_counts() {
        assert_eq!(batch_status(&counts(2, 0, 0, 0, 0)), "queued");
        assert_eq!(batch_status(&counts(1, 1, 0, 0, 0)), "running");
        assert_eq!(batch_status(&counts(0, 0, 2, 0, 0)), "completed");
        assert_eq!(batch_status(&counts(0, 0, 0, 0, 2)), "cancelled");
        assert_eq!(batch_status(&counts(0, 0, 0, 1, 1)), "failed");
        assert_eq!(batch_status(&counts(0, 0, 1, 1, 0)), "partial");
    }
}
