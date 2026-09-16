//! Allowlisted render service facade.

use crate::{catalog::WorkflowCatalog, error::WorkflowError};
use nagisalake_protocol::DispatchJob;
use serde_json::Value as JsonValue;
use service_async::{
    MakeService, Service,
    layer::{FactoryLayer, layer_fn},
};
use std::{convert::Infallible, sync::Arc};

#[derive(Debug, Clone)]
pub struct RenderWorkflow {
    pub dispatch:    DispatchJob,
    pub input_names: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct WorkflowService {
    catalog: Arc<WorkflowCatalog>,
}

impl WorkflowService {
    pub fn new(catalog: Arc<WorkflowCatalog>) -> Self {
        Self { catalog }
    }

    pub fn layer<C>(
        catalog: Arc<WorkflowCatalog>,
    ) -> impl FactoryLayer<C, (), Factory = WorkflowServiceFactory> {
        layer_fn(move |_config: &C, ()| WorkflowServiceFactory {
            catalog: Arc::clone(&catalog),
        })
    }
}

impl Service<RenderWorkflow> for WorkflowService {
    type Response = JsonValue;
    type Error = WorkflowError;

    async fn call(&self, request: RenderWorkflow) -> Result<Self::Response, Self::Error> {
        self.catalog.render(&request.dispatch, &request.input_names)
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowServiceFactory {
    catalog: Arc<WorkflowCatalog>,
}

impl MakeService for WorkflowServiceFactory {
    type Service = WorkflowService;
    type Error = Infallible;

    fn make_via_ref(&self, _old: Option<&Self::Service>) -> Result<Self::Service, Self::Error> {
        Ok(WorkflowService::new(Arc::clone(&self.catalog)))
    }
}
