//! Serves the browser console compiled into the binary.
//!
//! Enabled by the `embed-web` feature. Without it the Hub stays API-only and
//! unknown paths keep returning the structured JSON 404, so deployments that
//! serve the frontend from a CDN or reverse proxy are unaffected.
//!
//! Two rules matter for correctness:
//!
//! - API prefixes must never fall back to `index.html`. A client asking for a
//!   missing endpoint needs a JSON error, not an HTML page it cannot parse.
//! - Unknown paths that are *not* API routes fall back to `index.html` so the
//!   SPA router can handle deep links like `/jobs/{id}` on a hard reload.

use axum::{
    Json,
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
};
use serde_json::json;

/// Structured 404 matching the documented error envelope.
fn json_not_found(uri: &Uri) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": {
                "code": "not_found",
                "message": format!("no route for {}", uri.path()),
                "request_id": null,
            }
        })),
    )
        .into_response()
}

#[cfg(not(feature = "embed-web"))]
pub(crate) async fn fallback(uri: Uri) -> Response {
    json_not_found(&uri)
}

#[cfg(feature = "embed-web")]
pub(crate) use embedded::fallback;

#[cfg(feature = "embed-web")]
mod embedded {
    use super::{IntoResponse, Response, StatusCode, Uri, json_not_found};
    use axum::http::{
        HeaderMap, HeaderValue, Method,
        header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH},
    };
    use rust_embed::Embed;

    // Paths are resolved relative to CARGO_MANIFEST_DIR, so this points at
    // web/dist in the repository root. build.rs verifies it exists.
    #[derive(Embed)]
    #[folder = "../../web/dist"]
    struct Assets;

    const INDEX: &str = "index.html";

    /// Request prefixes owned by the API. These return JSON errors rather than
    /// the SPA shell, so a client calling a missing endpoint is never handed
    /// HTML it cannot parse.
    const API_PREFIXES: [&str; 3] = ["/api/", "/v1/", "/healthz"];

    fn is_api_path(path: &str) -> bool {
        API_PREFIXES
            .iter()
            .any(|prefix| path == prefix.trim_end_matches('/') || path.starts_with(prefix))
    }

    pub(crate) async fn fallback(method: Method, uri: Uri, headers: HeaderMap) -> Response {
        if is_api_path(uri.path()) {
            return json_not_found(&uri);
        }
        if !matches!(method, Method::GET | Method::HEAD) {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }

        let path = uri.path().trim_start_matches('/');
        let key = if path.is_empty() { INDEX } else { path };

        if let Some(response) = serve(key, &headers, &method) {
            return response;
        }

        // A missing hashed asset must not resolve to HTML: the browser would
        // reject it for a script or stylesheet request under nosniff and the
        // real error would be hidden.
        if key.starts_with("assets/") {
            return json_not_found(&uri);
        }

        // SPA deep link: hand back the shell and let the client router decide.
        serve(INDEX, &headers, &method).unwrap_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "embedded frontend is missing index.html",
            )
                .into_response()
        })
    }

    fn serve(key: &str, headers: &HeaderMap, method: &Method) -> Option<Response> {
        let file = Assets::get(key)?;
        let etag = format!(
            "\"{}\"",
            data_encoding::HEXLOWER.encode(&file.metadata.sha256_hash()[..16])
        );

        if headers
            .get(IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.split(',').any(|candidate| candidate.trim() == etag))
        {
            let mut response = StatusCode::NOT_MODIFIED.into_response();
            insert_cache_headers(response.headers_mut(), key, &etag);
            return Some(response);
        }

        let content_type = mime_guess::from_path(key)
            .first_raw()
            .unwrap_or("application/octet-stream");

        // HEAD must carry the same headers as GET but no body.
        let body = if method == Method::HEAD {
            axum::body::Body::empty()
        } else {
            axum::body::Body::from(file.data.to_vec())
        };

        let mut response = Response::new(body);
        let response_headers = response.headers_mut();
        if let Ok(value) = HeaderValue::from_str(content_type) {
            response_headers.insert(CONTENT_TYPE, value);
        }
        insert_cache_headers(response_headers, key, &etag);
        Some(response)
    }

    fn insert_cache_headers(headers: &mut HeaderMap, key: &str, etag: &str) {
        // Vite emits content-hashed names under assets/, so those are immutable.
        // index.html must revalidate or clients would pin an old asset graph.
        let cache_control = if key.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else if key == INDEX {
            "no-cache"
        } else {
            "public, max-age=3600"
        };
        headers.insert(CACHE_CONTROL, HeaderValue::from_static(cache_control));
        if let Ok(value) = HeaderValue::from_str(etag) {
            headers.insert(ETAG, value);
        }
        headers.insert(
            "x-content-type-options",
            HeaderValue::from_static("nosniff"),
        );
    }

    /// True when the frontend was compiled in and has a usable shell.
    pub(crate) fn is_present() -> bool {
        Assets::get(INDEX).is_some()
    }
}

/// Whether this build serves a browser console.
pub(crate) fn is_embedded() -> bool {
    #[cfg(feature = "embed-web")]
    {
        embedded::is_present()
    }
    #[cfg(not(feature = "embed-web"))]
    {
        false
    }
}
