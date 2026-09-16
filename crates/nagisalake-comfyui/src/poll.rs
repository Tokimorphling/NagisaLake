//! Poll-until-complete service layered over the ComfyUI history endpoint.

use crate::{
    config::{BuildError, ComfyUiConfig, ComfyUiStackConfig},
    error::ComfyUiError,
    service::ComfyUiService,
};
use nagisalake_core::{
    ComfyHistoryRequest, ComfyHistoryResponse, ComfyPromptRequest, ComfyPromptResponse,
    ComfyPromptStatus, ComfyQueueDeleteRequest, ComfyQueueStatusRequest, ComfyUploadImageRequest,
    ComfyUploadImageResponse, ComfyViewRequest, OutputRef,
};
use service_async::{
    MakeService, Service,
    layer::{FactoryLayer, layer_fn},
    stack::FactoryStack,
};
use std::{convert::Infallible, time::Duration};
use tokio_util::sync::CancellationToken;
use tracing::debug;

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
