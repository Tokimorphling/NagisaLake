use super::{authentication::authorize_current, shared::*, *};
use axum::{
    body::Body,
    http::header::{
        ACCEPT_RANGES, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
        CONTENT_TYPE, ETAG, RANGE,
    },
};
use tokio_util::io::ReaderStream;

/// Same-origin streaming endpoint for authenticated artifact consumers.
pub(super) async fn artifact_content(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(artifact_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    let auth = match authorize_current(&state, &headers, Permission::ArtifactsRead).await {
        Ok(value) => value,
        Err(error) => return product_error(error, &request_id),
    };
    let artifact =
        match readable_artifact_for_principal(&state, &auth.principal, &artifact_id).await {
            Ok(value) => value,
            Err(error) => return product_error(error, &request_id),
        };
    stream_object_content(
        &state,
        &headers,
        &request_id,
        &artifact.object_key,
        &artifact.view.name,
        &artifact.view.content_type,
        artifact.view.size_bytes,
        &artifact.view.sha256,
    )
    .await
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ByteRange {
    request:       String,
    content_range: String,
}

pub(super) fn normalize_range(value: &str, size_bytes: u64) -> Result<ByteRange, ()> {
    if size_bytes == 0 {
        return Err(());
    }
    let value = value.trim().strip_prefix("bytes=").ok_or(())?;
    if value.contains(',') {
        return Err(());
    }
    let (start, end) = value.split_once('-').ok_or(())?;
    let (start, end) = if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        (
            size_bytes.saturating_sub(suffix.min(size_bytes)),
            size_bytes - 1,
        )
    } else {
        let start = start.parse::<u64>().map_err(|_| ())?;
        if start >= size_bytes {
            return Err(());
        }
        let end = if end.is_empty() {
            size_bytes - 1
        } else {
            end.parse::<u64>().map_err(|_| ())?.min(size_bytes - 1)
        };
        if end < start {
            return Err(());
        }
        (start, end)
    };
    Ok(ByteRange {
        request:       format!("bytes={start}-{end}"),
        content_range: format!("bytes {start}-{end}/{size_bytes}"),
    })
}

pub(super) fn normalized_media_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

pub(super) fn safe_inline_media_type(content_type: &str) -> bool {
    let value = normalized_media_type(content_type);
    matches!(
        value.as_str(),
        "image/png"
            | "image/jpeg"
            | "image/webp"
            | "image/gif"
            | "image/avif"
            | "image/bmp"
            | "image/x-icon"
    ) || value.starts_with("audio/")
        || value.starts_with("video/")
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn stream_object_content(
    state: &AppState,
    request_headers: &HeaderMap,
    request_id: &str,
    object_key: &str,
    artifact_name: &str,
    declared_content_type: &str,
    size_bytes: u64,
    sha256: &str,
) -> Response {
    let range = match request_headers.get(RANGE) {
        None => None,
        Some(value) => match value
            .to_str()
            .map_err(|_| ())
            .and_then(|value| normalize_range(value, size_bytes))
        {
            Ok(range) => Some(range),
            Err(()) => {
                let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
                if let Ok(value) = HeaderValue::from_str(&format!("bytes */{size_bytes}")) {
                    response.headers_mut().insert(CONTENT_RANGE, value);
                }
                return response;
            }
        },
    };
    let object = match state
        .objects
        .get(
            object_key,
            range.as_ref().map(|range| range.request.as_str()),
        )
        .await
    {
        Ok(value) => value,
        Err(error) => {
            return product_error(HubError::ObjectStore(error.to_string()), request_id);
        }
    };
    let inline = safe_inline_media_type(declared_content_type);
    let response_content_type = if inline {
        declared_content_type
    } else {
        "application/octet-stream"
    };
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    if let Ok(value) = HeaderValue::from_str(response_content_type) {
        headers.insert(CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&object.size_bytes.to_string()) {
        headers.insert(CONTENT_LENGTH, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!("\"{sha256}\"")) {
        headers.insert(ETAG, value);
    }
    if !inline {
        let filename = artifact_name
            .chars()
            .map(|character| {
                if character.is_ascii_graphic() && character != '"' && character != '\\' {
                    character
                } else {
                    '_'
                }
            })
            .take(180)
            .collect::<String>();
        if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
            headers.insert(CONTENT_DISPOSITION, value);
        }
    }
    let status = if let Some(range) = range {
        let content_range = object
            .content_range
            .as_deref()
            .unwrap_or(&range.content_range);
        if let Ok(value) = HeaderValue::from_str(content_range) {
            headers.insert(CONTENT_RANGE, value);
        }
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let body = Body::from_stream(ReaderStream::new(object.body.into_async_read()));
    (status, headers, body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_ranges_are_normalized_and_bounded() {
        assert_eq!(
            normalize_range("bytes=10-19", 100).unwrap().request,
            "bytes=10-19"
        );
        assert_eq!(
            normalize_range("bytes=90-999", 100).unwrap().request,
            "bytes=90-99"
        );
        assert_eq!(
            normalize_range("bytes=-10", 100).unwrap().request,
            "bytes=90-99"
        );
        assert!(normalize_range("bytes=100-", 100).is_err());
        assert!(normalize_range("bytes=0-1,4-5", 100).is_err());
    }

    #[test]
    fn active_content_is_never_rendered_inline() {
        assert!(safe_inline_media_type(" Image/PNG; charset=binary "));
        assert!(safe_inline_media_type("video/mp4"));
        assert!(!safe_inline_media_type("image/svg+xml"));
        assert!(!safe_inline_media_type("text/html"));
    }
}
