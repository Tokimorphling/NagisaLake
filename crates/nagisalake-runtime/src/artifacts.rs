//! Bounded artifact byte streams, validation, signed lifetimes and retries.
use crate::{
    RuntimeError, WorkerRuntime,
    error::{HttpFailure, HttpFailureClass, http_failure},
    util::now_unix_ms,
};
use data_encoding::HEXLOWER;
use nagisalake_protocol::{ArtifactReady, PresignedRequest};
use reqwest::{Body, Client, Method, Response};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};
use tokio::{
    fs::{self, File},
    io::{AsyncWriteExt, BufWriter},
};
use tokio_util::{io::ReaderStream, sync::CancellationToken};
use tracing::warn;

const ARTIFACT_PUT_MAX_ATTEMPTS: u8 = 3;
const PRESIGNED_REQUEST_EXPIRY_GUARD: Duration = Duration::from_secs(1);
const ARTIFACT_STREAM_CHUNK_BYTES: usize = 256 * 1024;

pub(super) async fn download_to_file(
    client: &Client,
    request: &PresignedRequest,
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), RuntimeError> {
    validate_presigned(request, "GET")?;
    if expected_size == 0 {
        return Err(RuntimeError::Artifact(
            "input size must be greater than zero".into(),
        ));
    }
    let mut builder = client.get(&request.url);
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    let response = builder
        .send()
        .await
        .map_err(|error| http_failure("artifact_get_send", 1, error))?;
    if !response.status().is_success() {
        return Err(RuntimeError::Http(HttpFailure::status(
            "artifact_get",
            1,
            response.status(),
        )));
    }
    let (size, digest) =
        stream_response_to_file(response, path, expected_size, "artifact_get_body").await?;
    if size != expected_size || !digest.eq_ignore_ascii_case(expected_sha256) {
        let _ = fs::remove_file(path).await;
        return Err(RuntimeError::Artifact(
            "downloaded input size or SHA-256 does not match metadata".into(),
        ));
    }
    Ok(())
}

pub(super) async fn stream_response_to_file(
    mut response: Response,
    path: &Path,
    max_bytes: u64,
    operation: &'static str,
) -> Result<(u64, String), RuntimeError> {
    // Coalesce small network chunks before dispatching writes to the file
    // worker, rather than issuing a write for every response chunk.
    let mut file = BufWriter::with_capacity(ARTIFACT_STREAM_CHUNK_BYTES, File::create(path).await?);
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| http_failure(operation, 1, error))?
    {
        size = size
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| RuntimeError::Artifact("artifact size overflow".into()))?;
        if size > max_bytes {
            let _ = fs::remove_file(path).await;
            return Err(RuntimeError::Artifact(format!(
                "artifact exceeds {max_bytes}-byte limit"
            )));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok((size, HEXLOWER.encode(&hasher.finalize())))
}

pub(super) async fn upload_file_with_retry(
    client: &Client,
    runtime: &WorkerRuntime,
    ready: &ArtifactReady,
    path: &Path,
    cancellation: &CancellationToken,
) -> Result<String, RuntimeError> {
    let mut artifact_id: Option<String> = None;
    let mut last_failure: Option<HttpFailure> = None;

    for attempt in 1..=ARTIFACT_PUT_MAX_ATTEMPTS {
        if attempt > 1 {
            // The request id deterministically spreads workers across a bounded
            // jitter window, avoiding a retry wave when one regional failure
            // releases many uploads at once without introducing another RNG.
            let delay = artifact_put_retry_delay(&ready.request_id, attempt);
            tokio::select! {
                () = cancellation.cancelled() => return Err(RuntimeError::Cancelled),
                () = tokio::time::sleep(delay) => {}
            }
        }

        // Replaying ArtifactReady with the same request id is idempotent in the
        // Hub and returns a fresh presigned URL for the same object key.
        let ticket = tokio::select! {
            () = cancellation.cancelled() => return Err(RuntimeError::Cancelled),
            result = runtime.request_artifact(ready.clone()) => result?,
        };
        if ticket.request_id != ready.request_id {
            return Err(RuntimeError::Artifact(
                "Hub returned an upload ticket for a different request".into(),
            ));
        }
        if let Some(expected) = artifact_id.as_deref() {
            if expected != ticket.artifact_id {
                return Err(RuntimeError::Artifact(
                    "Hub changed the artifact id while refreshing an upload ticket".into(),
                ));
            }
        } else {
            artifact_id = Some(ticket.artifact_id.clone());
        }

        match upload_file_once(
            client,
            &ticket.upload,
            path,
            ready.size_bytes,
            attempt,
            cancellation,
        )
        .await
        {
            Ok(()) => return Ok(ticket.artifact_id),
            Err(RuntimeError::Http(failure))
                if failure.transient && attempt < ARTIFACT_PUT_MAX_ATTEMPTS =>
            {
                warn!(
                    operation = failure.operation,
                    class = failure.class.as_str(),
                    status = ?failure.status,
                    upload_attempt = attempt,
                    max_attempts = ARTIFACT_PUT_MAX_ATTEMPTS,
                    "transient artifact upload failed; refreshing ticket and retrying"
                );
                last_failure = Some(failure);
            }
            Err(error) => return Err(error),
        }
    }

    Err(RuntimeError::Http(last_failure.unwrap_or_else(|| {
        HttpFailure::new(
            "artifact_put",
            HttpFailureClass::Unknown,
            None,
            false,
            ARTIFACT_PUT_MAX_ATTEMPTS,
            vec!["upload attempts were exhausted".into()],
        )
    })))
}

async fn upload_file_once(
    client: &Client,
    request: &PresignedRequest,
    path: &Path,
    expected_size: u64,
    attempt: u8,
    cancellation: &CancellationToken,
) -> Result<(), RuntimeError> {
    let request_timeout =
        artifact_put_request_timeout(request.expires_at_unix_ms, now_unix_ms(), attempt)
            .map_err(RuntimeError::Http)?;
    validate_presigned(request, "PUT")?;
    let file = File::open(path).await?;
    let size = file.metadata().await?.len();
    if size != expected_size {
        return Err(RuntimeError::Artifact(
            "output file size changed before upload".into(),
        ));
    }
    // Bound the request by the signed lifetime, not by a shorter fixed wall
    // clock. Outputs may be as large as 5 GiB, so a low absolute timeout would
    // reject otherwise healthy uploads on ordinary GPU-node uplinks. A timeout
    // at the ticket boundary is transient and still enters the bounded retry.
    let mut builder = client
        .request(Method::PUT, &request.url)
        .timeout(request_timeout)
        // Larger chunks amortize file reads and HTTP stream scheduling. This
        // changes chunk granularity, not the size limit or signed lifetime.
        .body(Body::wrap_stream(ReaderStream::with_capacity(
            file,
            ARTIFACT_STREAM_CHUNK_BYTES,
        )))
        .header(reqwest::header::CONTENT_LENGTH, size);
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    let response = tokio::select! {
        () = cancellation.cancelled() => return Err(RuntimeError::Cancelled),
        result = builder.send() => result.map_err(|error| http_failure("artifact_put", attempt, error))?,
    };
    if response.status().is_success() {
        Ok(())
    } else {
        Err(RuntimeError::Http(HttpFailure::status(
            "artifact_put",
            attempt,
            response.status(),
        )))
    }
}

pub(super) fn artifact_put_request_timeout(
    expires_at_unix_ms: i64,
    now_unix_ms: i64,
    attempt: u8,
) -> Result<Duration, HttpFailure> {
    let remaining_ms = expires_at_unix_ms.saturating_sub(now_unix_ms);
    let guard_ms = i64::try_from(PRESIGNED_REQUEST_EXPIRY_GUARD.as_millis()).unwrap_or(i64::MAX);
    if remaining_ms <= guard_ms {
        return Err(HttpFailure::new(
            "artifact_put",
            HttpFailureClass::TicketExpired,
            None,
            true,
            attempt,
            vec!["presigned upload ticket is expired or too close to expiry".into()],
        ));
    }
    Ok(Duration::from_millis(
        u64::try_from(remaining_ms.saturating_sub(guard_ms)).unwrap_or(1),
    ))
}

pub(super) fn validate_presigned(
    request: &PresignedRequest,
    method: &str,
) -> Result<(), RuntimeError> {
    if request.method != method {
        return Err(RuntimeError::Artifact(format!(
            "presigned request method must be {method}"
        )));
    }
    if request.url.trim().is_empty() {
        return Err(RuntimeError::Artifact(
            "presigned request URL is empty".into(),
        ));
    }
    if request.expires_at_unix_ms <= now_unix_ms() {
        return Err(RuntimeError::Artifact(
            "presigned request has expired".into(),
        ));
    }
    Ok(())
}

pub(super) fn safe_filename(value: &str) -> String {
    let candidate = Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact.bin");
    let filtered: String = candidate
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if filtered.is_empty() || filtered == "." || filtered == ".." {
        "artifact.bin".into()
    } else {
        filtered
    }
}

pub(super) async fn cleanup_job_dir(work_dir: &Path, job_id: &str) {
    let _ = fs::remove_dir_all(work_dir.join(job_id)).await;
}

pub(super) fn artifact_put_retry_delay(request_id: &str, attempt: u8) -> Duration {
    let base_ms = if attempt == 2 { 250 } else { 1_000 };
    let jitter_ms = request_id.bytes().fold(u64::from(attempt), |hash, byte| {
        hash.wrapping_mul(16_777_619) ^ u64::from(byte)
    }) % 251;
    Duration::from_millis(base_ms + jitter_ms)
}
