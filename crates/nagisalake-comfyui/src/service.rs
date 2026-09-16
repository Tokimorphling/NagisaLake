//! Per-endpoint services for the ComfyUI HTTP API.

use crate::{
    config::{ComfyUiConfig, ComfyUiStackConfig},
    error::ComfyUiError,
    parse::{parse_history, parse_queue_status},
};
use nagisalake_core::{
    ComfyHistoryRequest, ComfyHistoryResponse, ComfyPromptRequest, ComfyPromptResponse,
    ComfyPromptStatus, ComfyQueueDeleteRequest, ComfyQueueStatusRequest, ComfyUploadImageRequest,
    ComfyUploadImageResponse, ComfyViewRequest,
};
use reqwest::{
    Body, Client,
    multipart::{Form, Part},
};
use serde_json::{Value as JsonValue, json};
use service_async::{
    MakeService, Service,
    layer::{FactoryLayer, layer_fn},
};
use std::{convert::Infallible, sync::Arc};
use tokio_util::io::ReaderStream;

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

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}
