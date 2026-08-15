//! Composable services for the ComfyUI HTTP API.

use nagisalake_core::{
    ComfyHistoryRequest, ComfyHistoryResponse, ComfyPromptRequest, ComfyPromptResponse,
    ComfyPromptStatus, ComfyQueueDeleteRequest, ComfyQueueStatusRequest, ComfyUploadImageRequest,
    ComfyUploadImageResponse, ComfyViewRequest, OutputRef,
};
use reqwest::{
    Body, Client,
    multipart::{Form, Part},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use service_async::{
    MakeService, Service,
    layer::{FactoryLayer, layer_fn},
    stack::FactoryStack,
};
use std::{collections::BTreeSet, convert::Infallible, sync::Arc, time::Duration};
use thiserror::Error;
use tokio_util::{io::ReaderStream, sync::CancellationToken};
use tracing::debug;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComfyUiConfig {
    #[serde(default = "default_base_url")]
    pub base_url:                String,
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms:        u64,
    #[serde(default = "default_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes:        u64,
}

impl Default for ComfyUiConfig {
    fn default() -> Self {
        Self {
            base_url:                default_base_url(),
            poll_interval_ms:        default_poll_interval_ms(),
            request_timeout_seconds: default_request_timeout_seconds(),
            max_output_bytes:        default_max_output_bytes(),
        }
    }
}

impl ComfyUiConfig {
    pub fn validate(&self) -> Result<(), BuildError> {
        if self.base_url.trim().is_empty() {
            return Err(BuildError::InvalidConfig("base_url must not be empty"));
        }
        if self.poll_interval_ms < 100 {
            return Err(BuildError::InvalidConfig(
                "poll_interval_ms must be at least 100",
            ));
        }
        if self.request_timeout_seconds == 0 {
            return Err(BuildError::InvalidConfig(
                "request_timeout_seconds must be greater than zero",
            ));
        }
        if self.max_output_bytes == 0 || self.max_output_bytes > 5 * 1024 * 1024 * 1024 {
            return Err(BuildError::InvalidConfig(
                "max_output_bytes must be between 1 byte and 5 GiB",
            ));
        }
        Ok(())
    }
}

fn default_base_url() -> String {
    "http://127.0.0.1:8188".into()
}

const fn default_poll_interval_ms() -> u64 {
    1_000
}

const fn default_request_timeout_seconds() -> u64 {
    60
}

const fn default_max_output_bytes() -> u64 {
    5 * 1024 * 1024 * 1024
}

#[derive(Debug, Clone)]
pub struct ComfyUiStackConfig {
    config: Arc<ComfyUiConfig>,
    client: Client,
}

impl ComfyUiStackConfig {
    pub fn new(config: ComfyUiConfig) -> Result<Self, BuildError> {
        config.validate()?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(config.request_timeout_seconds))
            .build()
            .map_err(BuildError::Client)?;
        Ok(Self {
            config: Arc::new(config),
            client,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ComfyUiService {
    client: Client,
    /// `base_url` with any trailing `/` stripped, computed once at construction
    /// so every `endpoint` call avoids `trim_end_matches` and a `format!` from
    /// scratch.
    base:   String,
}

impl ComfyUiService {
    pub fn layer() -> impl FactoryLayer<ComfyUiStackConfig, (), Factory = ComfyUiServiceFactory> {
        layer_fn(|config: &ComfyUiStackConfig, ()| ComfyUiServiceFactory {
            config: Arc::clone(&config.config),
            client: config.client.clone(),
        })
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }
}

#[derive(Debug, Clone)]
pub struct ComfyUiServiceFactory {
    config: Arc<ComfyUiConfig>,
    client: Client,
}

impl MakeService for ComfyUiServiceFactory {
    type Service = ComfyUiService;
    type Error = Infallible;

    fn make_via_ref(&self, _old: Option<&Self::Service>) -> Result<Self::Service, Self::Error> {
        Ok(ComfyUiService {
            client: self.client.clone(),
            base:   self.config.base_url.trim_end_matches('/').to_owned(),
        })
    }
}

impl Service<ComfyPromptRequest> for ComfyUiService {
    type Response = ComfyPromptResponse;
    type Error = ComfyUiError;

    async fn call(&self, request: ComfyPromptRequest) -> Result<Self::Response, Self::Error> {
        if !request.workflow.is_object() {
            return Err(ComfyUiError::InvalidResponse(
                "workflow must be a ComfyUI API JSON object".into(),
            ));
        }
        let response = self
            .client
            .post(self.endpoint("/prompt"))
            .json(&json!({
                "prompt": request.workflow,
                "client_id": request.client_id,
                "extra_data": {"nagisalake_job_id": request.job_id},
            }))
            .send()
            .await?
            .error_for_status()?;
        let payload: JsonValue = response.json().await?;
        if let Some(errors) = payload.get("node_errors")
            && errors.as_object().is_some_and(|errors| !errors.is_empty())
        {
            return Err(ComfyUiError::WorkflowRejected(truncate(
                &errors.to_string(),
                1_000,
            )));
        }
        let prompt_id = payload
            .get("prompt_id")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ComfyUiError::InvalidResponse("prompt response did not contain prompt_id".into())
            })?;
        Ok(ComfyPromptResponse {
            prompt_id: prompt_id.into(),
        })
    }
}

impl Service<ComfyHistoryRequest> for ComfyUiService {
    type Response = ComfyHistoryResponse;
    type Error = ComfyUiError;

    async fn call(&self, request: ComfyHistoryRequest) -> Result<Self::Response, Self::Error> {
        let response = self
            .client
            .get(self.endpoint(&format!("/history/{}", request.prompt_id)))
            .send()
            .await?
            .error_for_status()?;
        parse_history(request.prompt_id.as_str(), response.json().await?)
    }
}

impl Service<ComfyQueueStatusRequest> for ComfyUiService {
    type Response = ComfyPromptStatus;
    type Error = ComfyUiError;

    async fn call(&self, request: ComfyQueueStatusRequest) -> Result<Self::Response, Self::Error> {
        let response = self
            .client
            .get(self.endpoint("/queue"))
            .send()
            .await?
            .error_for_status()?;
        Ok(parse_queue_status(
            &request.prompt_id,
            response.json().await?,
        ))
    }
}

impl Service<ComfyUploadImageRequest> for ComfyUiService {
    type Response = ComfyUploadImageResponse;
    type Error = ComfyUiError;

    async fn call(&self, request: ComfyUploadImageRequest) -> Result<Self::Response, Self::Error> {
        let file = tokio::fs::File::open(&request.path).await?;
        let size = file.metadata().await?.len();
        let part = Part::stream_with_length(Body::wrap_stream(ReaderStream::new(file)), size)
            .file_name(request.file_name);
        let response = self
            .client
            .post(self.endpoint("/upload/image"))
            .multipart(
                Form::new()
                    .part("image", part)
                    .text("type", "input")
                    .text("overwrite", "true"),
            )
            .send()
            .await?
            .error_for_status()?;
        let payload: JsonValue = response.json().await?;
        let name = payload
            .get("name")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ComfyUiError::InvalidResponse("upload response did not contain name".into())
            })?;
        Ok(ComfyUploadImageResponse { name: name.into() })
    }
}

impl Service<ComfyQueueDeleteRequest> for ComfyUiService {
    type Response = ();
    type Error = ComfyUiError;

    async fn call(&self, request: ComfyQueueDeleteRequest) -> Result<Self::Response, Self::Error> {
        self.client
            .post(self.endpoint("/queue"))
            .json(&json!({"delete": [request.prompt_id]}))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

impl Service<ComfyViewRequest> for ComfyUiService {
    type Response = reqwest::Response;
    type Error = ComfyUiError;

    async fn call(&self, request: ComfyViewRequest) -> Result<Self::Response, Self::Error> {
        Ok(self
            .client
            .get(self.endpoint("/view"))
            .query(&[
                ("filename", request.output.filename.as_str()),
                ("subfolder", request.output.subfolder.as_str()),
                ("type", request.output.storage_type.as_str()),
            ])
            .send()
            .await?
            .error_for_status()?)
    }
}

#[derive(Debug, Clone)]
pub struct WaitForCompletion {
    pub prompt_id:    String,
    pub cancellation: CancellationToken,
    pub status_tx:    tokio::sync::watch::Sender<ComfyPromptStatus>,
}

#[derive(Debug, Clone)]
pub struct PollUntilCompleteService<T> {
    inner:    T,
    interval: Duration,
}

impl<T> PollUntilCompleteService<T> {
    pub fn layer<F>()
    -> impl FactoryLayer<ComfyUiStackConfig, F, Factory = PollUntilCompleteFactory<F>> {
        layer_fn(
            |config: &ComfyUiStackConfig, inner| PollUntilCompleteFactory {
                inner,
                interval: Duration::from_millis(config.config.poll_interval_ms),
            },
        )
    }

    pub fn inner(&self) -> &T {
        &self.inner
    }
}

impl<T> Service<WaitForCompletion> for PollUntilCompleteService<T>
where
    T: Service<ComfyHistoryRequest, Response = ComfyHistoryResponse, Error = ComfyUiError>
        + Service<ComfyQueueStatusRequest, Response = ComfyPromptStatus, Error = ComfyUiError>,
{
    type Response = Vec<OutputRef>;
    type Error = ComfyUiError;

    async fn call(&self, request: WaitForCompletion) -> Result<Self::Response, Self::Error> {
        loop {
            if request.cancellation.is_cancelled() {
                return Err(ComfyUiError::Cancelled);
            }
            match self
                .inner
                .call(ComfyHistoryRequest {
                    prompt_id: request.prompt_id.clone(),
                })
                .await?
            {
                ComfyHistoryResponse::Pending => {
                    match tokio::time::timeout(
                        self.interval,
                        self.inner.call(ComfyQueueStatusRequest {
                            prompt_id: request.prompt_id.clone(),
                        }),
                    )
                    .await
                    {
                        Ok(Ok(ComfyPromptStatus::Unknown)) => {}
                        Ok(Ok(status)) => {
                            request.status_tx.send_replace(status);
                        }
                        Ok(Err(error)) => {
                            debug!(
                                prompt_id = %request.prompt_id,
                                ?error,
                                "failed to inspect ComfyUI queue; history polling continues"
                            );
                        }
                        Err(_) => debug!(
                            prompt_id = %request.prompt_id,
                            "ComfyUI queue inspection timed out; history polling continues"
                        ),
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(self.interval) => {}
                        _ = request.cancellation.cancelled() => return Err(ComfyUiError::Cancelled),
                    }
                }
                ComfyHistoryResponse::Complete(outputs) => return Ok(outputs),
                ComfyHistoryResponse::Failed(message) => {
                    return Err(ComfyUiError::ExecutionFailed(message));
                }
            }
        }
    }
}

macro_rules! passthrough_service {
    ($request:ty, $response:ty) => {
        impl<T> Service<$request> for PollUntilCompleteService<T>
        where
            T: Service<$request, Response = $response, Error = ComfyUiError>,
        {
            type Response = $response;
            type Error = ComfyUiError;

            async fn call(&self, request: $request) -> Result<Self::Response, Self::Error> {
                self.inner.call(request).await
            }
        }
    };
}

passthrough_service!(ComfyPromptRequest, ComfyPromptResponse);
passthrough_service!(ComfyQueueStatusRequest, ComfyPromptStatus);
passthrough_service!(ComfyUploadImageRequest, ComfyUploadImageResponse);
passthrough_service!(ComfyQueueDeleteRequest, ());
passthrough_service!(ComfyViewRequest, reqwest::Response);

#[derive(Debug, Clone)]
pub struct PollUntilCompleteFactory<F> {
    inner:    F,
    interval: Duration,
}

impl<F> MakeService for PollUntilCompleteFactory<F>
where
    F: MakeService<Error = Infallible>,
{
    type Service = PollUntilCompleteService<F::Service>;
    type Error = Infallible;

    fn make_via_ref(&self, old: Option<&Self::Service>) -> Result<Self::Service, Self::Error> {
        Ok(PollUntilCompleteService {
            inner:    self.inner.make_via_ref(old.map(|service| &service.inner))?,
            interval: self.interval,
        })
    }
}

pub fn build_service(
    config: ComfyUiConfig,
) -> Result<PollUntilCompleteService<ComfyUiService>, BuildError> {
    let stack_config = ComfyUiStackConfig::new(config)?;
    let stack = FactoryStack::new(stack_config)
        .push(ComfyUiService::layer())
        .push(PollUntilCompleteService::<ComfyUiService>::layer());
    Ok(stack.make().expect("ComfyUI factories are infallible"))
}

fn parse_history(
    prompt_id: &str,
    payload: JsonValue,
) -> Result<ComfyHistoryResponse, ComfyUiError> {
    let Some(entry) = payload.get(prompt_id) else {
        return Ok(ComfyHistoryResponse::Pending);
    };
    if entry
        .pointer("/status/status_str")
        .and_then(JsonValue::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("error"))
    {
        return Ok(ComfyHistoryResponse::Failed(
            entry
                .get("status")
                .map(JsonValue::to_string)
                .unwrap_or_else(|| "unknown execution error".into()),
        ));
    }
    let outputs = extract_outputs(entry.get("outputs"));
    if outputs.is_empty() {
        if entry
            .pointer("/status/completed")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
        {
            return Ok(ComfyHistoryResponse::Failed(
                "workflow completed without image, video, or audio artifacts".into(),
            ));
        }
        return Ok(ComfyHistoryResponse::Pending);
    }
    Ok(ComfyHistoryResponse::Complete(outputs))
}

fn parse_queue_status(prompt_id: &str, payload: JsonValue) -> ComfyPromptStatus {
    if queue_position(payload.get("queue_running"), prompt_id).is_some() {
        return ComfyPromptStatus::Running;
    }
    queue_position(payload.get("queue_pending"), prompt_id)
        .map_or(ComfyPromptStatus::Unknown, |position| {
            ComfyPromptStatus::Queued { position }
        })
}

fn queue_position(entries: Option<&JsonValue>, prompt_id: &str) -> Option<u32> {
    entries
        .and_then(JsonValue::as_array)?
        .iter()
        .position(|entry| {
            entry
                .as_array()
                .and_then(|fields| fields.get(1))
                .and_then(JsonValue::as_str)
                == Some(prompt_id)
        })
        .map(|index| u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX))
}

fn extract_outputs(outputs: Option<&JsonValue>) -> Vec<OutputRef> {
    let Some(outputs) = outputs.and_then(JsonValue::as_object) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut unique = BTreeSet::new();
    for node in outputs.values().filter_map(JsonValue::as_object) {
        for kind in ["images", "gifs", "videos", "audio"] {
            let Some(items) = node.get(kind).and_then(JsonValue::as_array) else {
                continue;
            };
            for item in items.iter().filter_map(JsonValue::as_object) {
                let Some(filename) = item.get("filename").and_then(JsonValue::as_str) else {
                    continue;
                };
                let subfolder = item
                    .get("subfolder")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("");
                let storage_type = item
                    .get("type")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("output");
                let identity = (storage_type, subfolder, filename);
                if unique.insert(identity) {
                    found.push(OutputRef {
                        filename:     filename.into(),
                        subfolder:    subfolder.into(),
                        storage_type: storage_type.into(),
                        content_type: content_type_for(filename, kind).into(),
                    });
                }
            }
        }
    }
    found
}

fn content_type_for(filename: &str, kind: &str) -> &'static str {
    match filename.rsplit_once('.').map(|(_, extension)| extension) {
        Some(ext) if ext.eq_ignore_ascii_case("png") => "image/png",
        Some(ext) if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") => {
            "image/jpeg"
        }
        Some(ext) if ext.eq_ignore_ascii_case("webp") => "image/webp",
        Some(ext) if ext.eq_ignore_ascii_case("gif") => "image/gif",
        Some(ext) if ext.eq_ignore_ascii_case("mp4") => "video/mp4",
        Some(ext) if ext.eq_ignore_ascii_case("webm") => "video/webm",
        Some(ext) if ext.eq_ignore_ascii_case("mov") => "video/quicktime",
        Some(ext) if ext.eq_ignore_ascii_case("wav") => "audio/wav",
        Some(ext) if ext.eq_ignore_ascii_case("mp3") => "audio/mpeg",
        _ if kind == "videos" => "video/mp4",
        _ if kind == "audio" => "audio/wav",
        _ => "application/octet-stream",
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("invalid ComfyUI config: {0}")]
    InvalidConfig(&'static str),
    #[error("failed to build ComfyUI HTTP client: {0}")]
    Client(reqwest::Error),
}

#[derive(Debug, Error)]
pub enum ComfyUiError {
    #[error("ComfyUI HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("ComfyUI file operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("ComfyUI returned an invalid response: {0}")]
    InvalidResponse(String),
    #[error("ComfyUI rejected workflow nodes: {0}")]
    WorkflowRejected(String),
    #[error("ComfyUI execution failed: {0}")]
    ExecutionFailed(String),
    #[error("ComfyUI wait was cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ComfyUI's `/queue` entries are heterogeneous arrays shaped
    /// `[number, prompt_id, prompt, extra_data, outputs]`, so the prompt id has
    /// to be read positionally. Getting the index wrong would silently report
    /// every prompt as unknown and the console would show no queue position.
    #[test]
    fn queue_status_distinguishes_running_from_queued() {
        let payload = json!({
            "queue_running": [[0, "running-prompt", {}, {}, []]],
            "queue_pending": [
                [1, "first-pending", {}, {}, []],
                [2, "second-pending", {}, {}, []]
            ]
        });

        assert_eq!(
            parse_queue_status("running-prompt", payload.clone()),
            ComfyPromptStatus::Running
        );
        // Position is 1-based so it reads naturally as "1st in line".
        assert_eq!(
            parse_queue_status("first-pending", payload.clone()),
            ComfyPromptStatus::Queued { position: 1 }
        );
        assert_eq!(
            parse_queue_status("second-pending", payload.clone()),
            ComfyPromptStatus::Queued { position: 2 }
        );
        // A prompt that has left the queue is Unknown, not Queued: the caller
        // keeps the last known status instead of resetting it.
        assert_eq!(
            parse_queue_status("finished-prompt", payload),
            ComfyPromptStatus::Unknown
        );
    }

    /// A malformed or empty payload must degrade to Unknown rather than panic;
    /// this parses a response from an external process we do not control.
    #[test]
    fn queue_status_tolerates_unexpected_payloads() {
        for payload in [
            json!({}),
            json!({"queue_running": [], "queue_pending": []}),
            json!({"queue_running": "not-an-array"}),
            // Entry too short to hold a prompt id.
            json!({"queue_pending": [[0]]}),
            // Prompt id in the wrong position.
            json!({"queue_pending": [["wanted", 0]]}),
            json!({"queue_pending": [null]}),
            json!(null),
        ] {
            assert_eq!(
                parse_queue_status("wanted", payload.clone()),
                ComfyPromptStatus::Unknown,
                "payload {payload} should not yield a position"
            );
        }
    }

    #[test]
    fn extracts_known_output_collections_only() {
        let history = json!({
            "prompt-1": {
                "status": {"completed": true},
                "outputs": {
                    "1": {"images":[{"filename":"a.png","subfolder":"","type":"output"}]},
                    "2": {"videos":[{"filename":"b.mp4","subfolder":"clips","type":"output"}]},
                    "3": {"arbitrary":[{"filename":"secret.bin"}]}
                }
            }
        });
        let ComfyHistoryResponse::Complete(outputs) = parse_history("prompt-1", history).unwrap()
        else {
            panic!("expected completed history");
        };
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].content_type, "image/png");
        assert_eq!(outputs[1].content_type, "video/mp4");
    }

    #[test]
    fn stack_builds_without_contacting_comfyui() {
        build_service(ComfyUiConfig::default()).unwrap();
    }
}
