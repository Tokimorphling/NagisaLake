use crate::AgentError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelRef {
    #[serde(rename = "providerID", alias = "provider_id")]
    pub provider_id: String,
    #[serde(rename = "modelID", alias = "model_id")]
    pub model_id:    String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OpenCodeConfig {
    pub base_url:                String,
    pub username:                String,
    /// Name of the secret environment variable, never an inline password.
    pub password_env:            Option<String>,
    pub agent:                   String,
    /// Server-side OpenCode project directory. Never accepted from public input.
    pub directory:               Option<String>,
    pub model:                   Option<ModelRef>,
    pub request_timeout_seconds: u64,
    pub poll_interval_ms:        u64,
    pub upstream_buffer:         usize,
    pub max_response_bytes:      usize,
    pub max_event_bytes:         usize,
    pub cleanup_sessions:        bool,
}

impl Default for OpenCodeConfig {
    fn default() -> Self {
        Self {
            base_url:                "http://127.0.0.1:4096".into(),
            username:                "opencode".into(),
            password_env:            None,
            agent:                   "build".into(),
            directory:               None,
            model:                   None,
            request_timeout_seconds: 15,
            poll_interval_ms:        2000,
            upstream_buffer:         256,
            max_response_bytes:      8 * 1024 * 1024,
            max_event_bytes:         1024 * 1024,
            cleanup_sessions:        true,
        }
    }
}

impl OpenCodeConfig {
    pub fn validate(&self) -> Result<(), AgentError> {
        let url = reqwest::Url::parse(&self.base_url)
            .map_err(|_| AgentError::Config("invalid OpenCode base_url"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(AgentError::Config(
                "base_url must be HTTP(S) without credentials, query or fragment",
            ));
        }
        if self.agent.trim().is_empty()
            || self.username.trim().is_empty()
            || !(1..=120).contains(&self.request_timeout_seconds)
            || !(50..=60_000).contains(&self.poll_interval_ms)
            || !(1..=4096).contains(&self.upstream_buffer)
            || !(1024..=64 * 1024 * 1024).contains(&self.max_response_bytes)
            || !(1024..=4 * 1024 * 1024).contains(&self.max_event_bytes)
        {
            return Err(AgentError::Config("invalid OpenCode limits or agent name"));
        }
        let loopback = matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
        );
        if !loopback && self.password_env.is_none() {
            return Err(AgentError::Config(
                "non-loopback OpenCode requires password_env",
            ));
        }
        Ok(())
    }
}
