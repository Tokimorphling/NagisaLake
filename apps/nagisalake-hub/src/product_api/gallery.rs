use super::{
    authentication::require_browser,
    media::{normalized_media_type, safe_inline_media_type, stream_object_content},
    shared::*,
    *,
};
use nagisalake_hub_store::{PublishGalleryItem, StoredGalleryItem};
use nagisalake_protocol::{WorkflowInputKind, WorkflowManifest};
use serde_json::Map;

const GALLERY_PAGE_DEFAULT: i64 = 24;
const GALLERY_PAGE_MAX: i64 = 50;
const MAX_PUBLIC_PARAMETERS: usize = 32;
const MAX_PUBLIC_PARAMETER_BYTES: usize = 16 * 1024;
const MAX_PUBLIC_STRING_CHARS: usize = 8 * 1024;

#[derive(Debug, Deserialize)]
pub(super) struct PublishGalleryRequest {
    artifact_id: String,
}

#[derive(Debug, Serialize)]
pub(super) struct GalleryArtifactView {
    id:           String,
    name:         String,
    content_type: String,
    size_bytes:   u64,
    sha256:       String,
}

/// Public projection: tenant, owner, and object-store coordinates are omitted.
#[derive(Debug, Serialize)]
pub(super) struct GalleryItemView {
    id:                   String,
    artifact:             GalleryArtifactView,
    job_id:               String,
    workflow_id:          String,
    workflow_version:     String,
    display_name:         String,
    parameters:           JsonValue,
    media_kind:           &'static str,
    content_url:          String,
    can_unpublish:        bool,
    published_at_unix_ms: i64,
}

#[derive(Debug, Serialize)]
pub(super) struct GalleryDownloadResponse {
    download: nagisalake_protocol::PresignedRequest,
}

pub(super) async fn publish_gallery_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PublishGalleryRequest>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    if !auth.principal.allows(Permission::ArtifactsWrite) {
        return product_error(
            HubError::Forbidden("missing permission artifacts:write".into()),
            &request_id,
        );
    }
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
    let candidate = match store
        .gallery_publish_candidate(
            &auth.principal.organization_id,
            request.artifact_id.trim(),
            user_id,
        )
        .await
    {
        Ok(Some(value)) => value,
        Ok(None) => {
            return product_error(
                HubError::Conflict(
                    "artifact is not a ready completed output owned by the current user".into(),
                ),
                &request_id,
            );
        }
        Err(error) => return product_error(map_store(error), &request_id),
    };
    if !safe_inline_media_type(&candidate.content_type) {
        return product_error(
            HubError::InvalidRequest(
                "only safe image, audio, or video outputs can be published".into(),
            ),
            &request_id,
        );
    }
    let parameters = sanitize_gallery_parameters(
        &candidate.parameters_json,
        candidate.manifest_json.as_deref(),
    );
    let parameters_json = serde_json::to_string(&parameters).unwrap_or_else(|_| "{}".into());
    let display_name = candidate
        .manifest_json
        .as_deref()
        .and_then(|value| serde_json::from_str::<WorkflowManifest>(value).ok())
        .map(|manifest| truncate(manifest.display_name.trim(), 200))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| truncate(&candidate.workflow_id, 200));
    let gallery_id = Uuid::new_v4().to_string();
    let item = match store
        .publish_gallery_item(PublishGalleryItem {
            id:              &gallery_id,
            organization_id: &auth.principal.organization_id,
            artifact_id:     &candidate.artifact_id,
            owner_user_id:   user_id,
            display_name:    &display_name,
            parameters_json: &parameters_json,
            published_at:    now_unix_ms(),
        })
        .await
    {
        Ok(value) => value,
        Err(error) => return product_error(map_store(error), &request_id),
    };
    audit(
        &state,
        Some(&auth.principal.organization_id),
        Some(user_id),
        auth_kind(auth.principal.kind),
        &request_id,
        "gallery.publish",
        "gallery_item",
        Some(&item.id),
        "success",
        json!({"artifact_id": item.artifact_id}),
    )
    .await;
    match gallery_view(item, user_id) {
        Ok(value) => Json(value).into_response(),
        Err(error) => product_error(error, &request_id),
    }
}

pub(super) async fn unpublish_gallery_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let Some(user_id) = auth.principal.user_id.as_deref() else {
        return product_error(
            HubError::Forbidden("a user-owned credential is required".into()),
            &request_id,
        );
    };
    let publication_organization_id = match store(&state) {
        Ok(store) => match store.unpublish_gallery_item(&item_id, user_id).await {
            Ok(value) => value,
            Err(error) => return product_error(map_store(error), &request_id),
        },
        Err(error) => return product_error(error, &request_id),
    };
    let Some(publication_organization_id) = publication_organization_id else {
        return product_error(HubError::NotFound("gallery item".into()), &request_id);
    };
    audit(
        &state,
        Some(&publication_organization_id),
        Some(user_id),
        auth_kind(auth.principal.kind),
        &request_id,
        "gallery.unpublish",
        "gallery_item",
        Some(&item_id),
        "success",
        json!({}),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

/// Lists gallery items newest-first across **all** organizations.
///
/// Visibility is intentionally cross-organization: any authenticated user
/// receives the same page. This is the "public gallery" semantic: only
/// publish/unpublish are tenant-scoped; reads are not filtered by org.
pub(super) async fn list_gallery_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CursorQuery>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let user_id = auth.principal.user_id.as_deref().unwrap_or_default();
    let limit = query
        .limit
        .unwrap_or(GALLERY_PAGE_DEFAULT)
        .clamp(1, GALLERY_PAGE_MAX);
    let after = match query.cursor.as_deref().map(decode_created_id_cursor) {
        Some(Ok(value)) => Some(value),
        Some(Err(error)) => return product_error(error, &request_id),
        None => None,
    };
    let rows = match store(&state) {
        Ok(store) => match store
            .gallery_items_page(
                limit.saturating_add(1),
                after
                    .as_ref()
                    .map(|(published_at, id)| (*published_at, id.as_str())),
            )
            .await
        {
            Ok(value) => value,
            Err(error) => return product_error(map_store(error), &request_id),
        },
        Err(error) => return product_error(error, &request_id),
    };
    let mut items = rows;
    let has_more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let next_cursor = has_more
        .then(|| {
            items
                .last()
                .map(|item| encode_created_id_cursor(item.published_at, &item.id))
        })
        .flatten();
    let items = match items
        .into_iter()
        .map(|item| gallery_view(item, user_id))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    Json(ListPage { items, next_cursor }).into_response()
}

pub(super) async fn get_gallery_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match require_browser(&state, &headers, None).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let user_id = auth.principal.user_id.as_deref().unwrap_or_default();
    let item = match store(&state) {
        Ok(store) => match store.gallery_item(&item_id).await {
            Ok(Some(value)) => value,
            Ok(None) => {
                return product_error(HubError::NotFound("gallery item".into()), &request_id);
            }
            Err(error) => return product_error(map_store(error), &request_id),
        },
        Err(error) => return product_error(error, &request_id),
    };
    match gallery_view(item, user_id) {
        Ok(value) => Json(value).into_response(),
        Err(error) => product_error(error, &request_id),
    }
}

pub(super) async fn gallery_item_content(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    if let Err(error) = require_browser(&state, &headers, None).await {
        return product_error(error, &request_id);
    }
    let content = match store(&state) {
        Ok(store) => match store.gallery_content(&item_id).await {
            Ok(Some(value)) => value,
            Ok(None) => {
                return product_error(HubError::NotFound("gallery item".into()), &request_id);
            }
            Err(error) => return product_error(map_store(error), &request_id),
        },
        Err(error) => return product_error(error, &request_id),
    };
    if !safe_inline_media_type(&content.content_type) {
        return product_error(HubError::NotFound("gallery item".into()), &request_id);
    }
    let size_bytes = match u64::try_from(content.size_bytes) {
        Ok(value) => value,
        Err(_) => {
            return product_error(
                HubError::InvalidConfig("persisted artifact size is negative".into()),
                &request_id,
            );
        }
    };
    stream_object_content(
        &state,
        &headers,
        &request_id,
        &content.object_key,
        &content.artifact_name,
        &content.content_type,
        size_bytes,
        &content.sha256,
    )
    .await
}

/// Native media elements cannot attach the access token kept in browser
/// memory, so an authenticated API call exchanges the publication for a fresh
/// short-lived object-store GET. The stable content route remains available to
/// authenticated fetches that need a same-origin Blob (for example Canvas).
pub(super) async fn gallery_item_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    if let Err(error) = require_browser(&state, &headers, None).await {
        return product_error(error, &request_id);
    }
    let content = match store(&state) {
        Ok(store) => match store.gallery_content(&item_id).await {
            Ok(Some(value)) => value,
            Ok(None) => {
                return product_error(HubError::NotFound("gallery item".into()), &request_id);
            }
            Err(error) => return product_error(map_store(error), &request_id),
        },
        Err(error) => return product_error(error, &request_id),
    };
    if !safe_inline_media_type(&content.content_type) {
        return product_error(HubError::NotFound("gallery item".into()), &request_id);
    }
    match state.objects.presign_get(&content.object_key).await {
        Ok(download) => (
            [("cache-control", "no-store")],
            Json(GalleryDownloadResponse { download }),
        )
            .into_response(),
        Err(error) => product_error(HubError::ObjectStore(error.to_string()), &request_id),
    }
}

fn gallery_view(
    item: StoredGalleryItem,
    current_user_id: &str,
) -> Result<GalleryItemView, HubError> {
    let size_bytes = u64::try_from(item.size_bytes)
        .map_err(|_| HubError::InvalidConfig("persisted artifact size is negative".into()))?;
    let parameters = serde_json::from_str(&item.parameters_json)
        .unwrap_or_else(|_| JsonValue::Object(Map::new()));
    let normalized_content_type = normalized_media_type(&item.content_type);
    let media_kind = if normalized_content_type.starts_with("image/") {
        "image"
    } else if normalized_content_type.starts_with("video/") {
        "video"
    } else {
        "audio"
    };
    let can_unpublish = item.owner_user_id == current_user_id;
    Ok(GalleryItemView {
        content_url: format!("/api/v1/gallery/items/{}/content", item.id),
        id: item.id,
        artifact: GalleryArtifactView {
            id: item.artifact_id,
            name: item.artifact_name,
            content_type: item.content_type,
            size_bytes,
            sha256: item.sha256,
        },
        job_id: item.job_id,
        workflow_id: item.workflow_id,
        workflow_version: item.workflow_version,
        display_name: item.display_name,
        parameters,
        media_kind,
        can_unpublish,
        published_at_unix_ms: item.published_at,
    })
}

fn sanitize_gallery_parameters(parameters_json: &str, manifest_json: Option<&str>) -> JsonValue {
    let parameters = serde_json::from_str::<JsonValue>(parameters_json)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let manifest =
        manifest_json.and_then(|value| serde_json::from_str::<WorkflowManifest>(value).ok());
    let Some(manifest) = manifest else {
        return JsonValue::Object(Map::new());
    };
    let mut public = Map::new();
    for input in manifest
        .inputs
        .iter()
        .filter(|input| {
            input.kind == WorkflowInputKind::Parameter && gallery_parameter_is_safe(&input.name)
        })
        .take(MAX_PUBLIC_PARAMETERS)
    {
        let Some(value) = parameters.get(&input.name).and_then(safe_parameter_value) else {
            continue;
        };
        public.insert(input.name.clone(), value);
        if serde_json::to_vec(&public).is_ok_and(|value| value.len() > MAX_PUBLIC_PARAMETER_BYTES) {
            public.remove(&input.name);
            break;
        }
    }
    JsonValue::Object(public)
}

/// Public cards expose only familiar generation controls. The manifest is the
/// first allowlist, and this conservative name allowlist prevents a workflow
/// author from labelling credentials or internal switches as public parameters.
fn gallery_parameter_is_safe(name: &str) -> bool {
    let normalized = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "prompt"
            | "positive"
            | "positive_prompt"
            | "text"
            | "text_prompt"
            | "negative"
            | "negative_prompt"
            | "model"
            | "model_name"
            | "ckpt"
            | "ckpt_name"
            | "checkpoint"
            | "checkpoint_name"
            | "seed"
            | "random_seed"
            | "noise_seed"
            | "steps"
            | "num_steps"
            | "sampling_steps"
            | "cfg"
            | "cfg_scale"
            | "guidance"
            | "guidance_scale"
            | "sampler"
            | "sampler_name"
            | "scheduler"
            | "scheduler_name"
            | "resolution"
            | "size"
            | "width"
            | "height"
            | "aspect_ratio"
    )
}

fn safe_parameter_value(value: &JsonValue) -> Option<JsonValue> {
    match value {
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) => Some(value.clone()),
        JsonValue::String(value) if value.chars().count() <= MAX_PUBLIC_STRING_CHARS => {
            Some(JsonValue::String(value.clone()))
        }
        JsonValue::Array(values) if values.len() <= 64 => values
            .iter()
            .map(safe_parameter_value)
            .collect::<Option<Vec<_>>>()
            .map(JsonValue::Array),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gallery_parameters_are_manifest_allowlisted() {
        let manifest = json!({
            "display_name": "Safe workflow",
            "inputs": [
                {"name":"prompt","kind":"parameter","type":"string","pointer":"/1/prompt"},
                {"name":"image","kind":"artifact","type":"image","pointer":"/2/image"},
                {"name":"seed","kind":"parameter","type":"integer","pointer":"/3/seed"},
                {"name":"api_token","kind":"parameter","type":"string","pointer":"/4/token"},
                {"name":"password","kind":"parameter","type":"string","pointer":"/5/password"}
            ]
        });
        let parameters = json!({
            "prompt": "a lake",
            "image": "private-artifact-id",
            "seed": 42,
            "api_token": "must not escape",
            "password": "must not escape",
            "internal_token": "must not escape",
            "nested": {"secret": true}
        });
        let sanitized = sanitize_gallery_parameters(
            &serde_json::to_string(&parameters).unwrap(),
            Some(&serde_json::to_string(&manifest).unwrap()),
        );
        assert_eq!(sanitized, json!({"prompt":"a lake","seed":42}));
    }

    #[test]
    fn missing_manifest_exposes_no_raw_parameters() {
        assert_eq!(
            sanitize_gallery_parameters(r#"{"prompt":"not allowlisted"}"#, None),
            json!({})
        );
    }

    #[test]
    fn media_kind_uses_the_normalized_content_type() {
        let item = StoredGalleryItem {
            id:               "gallery".into(),
            organization_id:  "private-org".into(),
            artifact_id:      "artifact".into(),
            job_id:           "job".into(),
            owner_user_id:    "private-owner".into(),
            workflow_id:      "workflow".into(),
            workflow_version: "v1".into(),
            display_name:     "Workflow".into(),
            parameters_json:  "{}".into(),
            published_at:     1,
            artifact_name:    "output.png".into(),
            content_type:     "Image/PNG; charset=binary".into(),
            size_bytes:       1,
            sha256:           "0".repeat(64),
        };
        let other_view = gallery_view(item.clone(), "another-user").unwrap();
        assert!(!other_view.can_unpublish);
        let view = gallery_view(item, "private-owner").unwrap();
        assert_eq!(view.media_kind, "image");
        assert!(view.can_unpublish);
        let encoded = serde_json::to_value(&view).unwrap();
        assert!(encoded.get("organization_id").is_none());
        assert!(encoded.get("owner_user_id").is_none());
        assert!(encoded.get("object_key").is_none());
    }

    #[tokio::test]
    async fn gallery_reads_require_login() {
        let app = crate::router(crate::HubConfig {
            server:       crate::ServerConfig::default(),
            auth:         crate::AuthConfig {
                worker_token: Some("test-worker-token".into()),
                consumer_token: Some("test-consumer-token".into()),
                ..crate::AuthConfig::default()
            },
            browser:      crate::BrowserConfig::default(),
            database:     None,
            transport:    crate::TransportConfig::default(),
            object_store: None,
            oauth:        None,
            rate_limit:   crate::RateLimitConfig::default(),
            log:          crate::LogConfig::default(),
        })
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        for path in [
            "/api/v1/gallery/items",
            "/api/v1/gallery/items/missing",
            "/api/v1/gallery/items/missing/content",
            "/api/v1/gallery/items/missing/download",
        ] {
            let response = client
                .get(format!("http://{address}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
        server.abort();
    }
}
