//! ComfyUI connection settings and the motore factory-stack config layer.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use thiserror::Error;

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
    pub(super) config: Arc<ComfyUiConfig>,
    pub(super) client: Client,
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

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("invalid ComfyUI config: {0}")]
    InvalidConfig(&'static str),
    #[error("failed to build ComfyUI HTTP client: {0}")]
    Client(reqwest::Error),
}
