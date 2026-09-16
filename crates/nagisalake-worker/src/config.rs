//! Worker configuration: TOML shape, path resolution, and validation.

use crate::error::WorkerError;
use nagisalake_comfyui::ComfyUiConfig;
use nagisalake_transport::{ConnectScheme, connect_scheme};
use nagisalake_workflow::WorkflowConfig;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
};

pub const MAX_QUEUE_DEPTH: u16 = 1_024;

/// Complete worker configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkerConfig {
    #[serde(default)]
    pub hub:       HubConfig,
    pub worker:    WorkerIdentity,
    #[serde(default)]
    pub state:     StateConfig,
    #[serde(default)]
    pub comfyui:   ComfyUiConfig,
    #[serde(default = "default_work_dir")]
    pub work_dir:  PathBuf,
    pub workflows: Vec<WorkflowConfig>,
}

/// Outbound Hub connection settings.
#[derive(Debug, Clone, Deserialize)]
pub struct HubConfig {
    #[serde(default = "default_hub_url")]
    pub url:                     String,
    #[serde(default)]
    pub token:                   Option<String>,
    #[serde(default)]
    pub proxy:                   Option<String>,
    #[serde(default = "default_reconnect_max_seconds")]
    pub reconnect_max_seconds:   u64,
    #[serde(default = "default_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,
    #[serde(default = "default_max_frame_bytes")]
    pub max_frame_bytes:         usize,
    #[serde(default)]
    pub tls:                     HubTlsConfig,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            url:                     default_hub_url(),
            token:                   None,
            proxy:                   None,
            reconnect_max_seconds:   default_reconnect_max_seconds(),
            connect_timeout_seconds: default_connect_timeout_seconds(),
            max_frame_bytes:         default_max_frame_bytes(),
            tls:                     HubTlsConfig::default(),
        }
    }
}

/// TLS settings for a `wss://` Hub url.
///
/// A Hub behind a publicly issued certificate needs no `[hub.tls]` section at
/// all — `wss://` in `hub.url` is the whole configuration, and the built-in
/// public root store verifies it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HubTlsConfig {
    /// PEM CA bundles to trust alongside the public roots, for a Hub whose
    /// certificate is issued by a private CA.
    ///
    /// Relative paths resolve against the directory holding the worker config,
    /// matching `work_dir` and the workflow files. Read on every connection
    /// attempt rather than cached, so rotating the bundle on disk takes effect
    /// at the next reconnect without a restart.
    #[serde(default)]
    pub ca_certificates: Vec<PathBuf>,
}

/// Stable worker identity and advertised capacity.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkerIdentity {
    pub namespace:   String,
    pub node_name:   String,
    #[serde(default = "default_worker_version")]
    pub version:     String,
    #[serde(default = "default_parallelism", alias = "concurrency")]
    pub parallelism: u16,
    #[serde(default)]
    pub queue_depth: u16,
    #[serde(default)]
    pub labels:      BTreeMap<String, String>,
}

/// SQLite state location.
#[derive(Debug, Clone, Deserialize)]
pub struct StateConfig {
    #[serde(default = "default_sqlite_url")]
    pub sqlite_url: String,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            sqlite_url: default_sqlite_url(),
        }
    }
}

fn default_hub_url() -> String {
    "ws://127.0.0.1:9091/v1/worker/connect".into()
}

const fn default_reconnect_max_seconds() -> u64 {
    60
}

const fn default_connect_timeout_seconds() -> u64 {
    15
}

const fn default_max_frame_bytes() -> usize {
    1024 * 1024
}

fn default_worker_version() -> String {
    env!("CARGO_PKG_VERSION").into()
}

const fn default_parallelism() -> u16 {
    1
}

fn default_sqlite_url() -> String {
    "sqlite://nagisalake-worker.db".into()
}

fn default_work_dir() -> PathBuf {
    "./nagisalake-work".into()
}

impl WorkerConfig {
    /// Loads TOML and resolves worker secrets from `NAGISALAKE_WORKER_TOKEN`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, WorkerError> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path).map_err(|source| WorkerError::ConfigIo {
            path: path.to_path_buf(),
            source,
        })?;
        let mut config: Self = toml::from_str(&raw).map_err(WorkerError::ConfigParse)?;
        if config.hub.token.as_deref().is_none_or(str::is_empty) {
            config.hub.token = env::var("NAGISALAKE_WORKER_TOKEN").ok();
        }
        if config.hub.proxy.as_deref().is_none_or(str::is_empty) {
            config.hub.proxy = env::var("NAGISALAKE_WORKER_PROXY").ok();
        }
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let base = if base.is_absolute() {
            base.to_path_buf()
        } else {
            env::current_dir().map_err(WorkerError::Io)?.join(base)
        };
        if config.work_dir.is_relative() {
            config.work_dir = base.join(&config.work_dir);
        }
        config.state.sqlite_url = resolve_sqlite_url(&config.state.sqlite_url, &base);
        for workflow in &mut config.workflows {
            if workflow.file.is_relative() {
                workflow.file = base.join(&workflow.file);
            }
        }
        for certificate in &mut config.hub.tls.ca_certificates {
            if certificate.is_relative() {
                *certificate = base.join(&certificate);
            }
        }
        config.validate()?;
        Ok(config)
    }

    /// Validates authentication, identity, and execution limits.
    pub fn validate(&self) -> Result<(), WorkerError> {
        if self.hub.url.trim().is_empty() {
            return Err(WorkerError::InvalidConfig(
                "hub.url must not be empty".into(),
            ));
        }
        // Fail here rather than inside the reconnect loop. An `https://` paste
        // is dialable by nothing, and without this the worker would retry it
        // forever behind an exponential backoff, reporting only a url error.
        let scheme = connect_scheme(&self.hub.url).map_err(|_| {
            WorkerError::InvalidConfig(format!(
                "hub.url must be a ws:// or wss:// endpoint, got {:?}",
                self.hub.url.trim()
            ))
        })?;
        if scheme == ConnectScheme::Plain && !self.hub.tls.ca_certificates.is_empty() {
            return Err(WorkerError::InvalidConfig(
                "hub.tls.ca_certificates is set but hub.url is not wss://, so nothing would be \
                 encrypted"
                    .into(),
            ));
        }
        if self
            .hub
            .token
            .as_deref()
            .is_none_or(|token| token.trim().is_empty())
        {
            return Err(WorkerError::InvalidConfig(
                "hub.token or NAGISALAKE_WORKER_TOKEN is required".into(),
            ));
        }
        if self.worker.namespace.trim().is_empty() || self.worker.node_name.trim().is_empty() {
            return Err(WorkerError::InvalidConfig(
                "worker.namespace and worker.node_name are required".into(),
            ));
        }
        if self.worker.parallelism == 0 {
            return Err(WorkerError::InvalidConfig(
                "worker.parallelism must be greater than zero".into(),
            ));
        }
        if self.worker.queue_depth > MAX_QUEUE_DEPTH {
            return Err(WorkerError::InvalidConfig(format!(
                "worker.queue_depth must not exceed {MAX_QUEUE_DEPTH}"
            )));
        }
        if self.hub.connect_timeout_seconds == 0 || self.hub.max_frame_bytes == 0 {
            return Err(WorkerError::InvalidConfig(
                "hub timeout and max_frame_bytes must be greater than zero".into(),
            ));
        }
        if self.workflows.is_empty() {
            return Err(WorkerError::InvalidConfig(
                "at least one workflow must be configured".into(),
            ));
        }
        Ok(())
    }
}

/// Resolves a relative `sqlite://` URL against the config file's directory.
///
/// The result always uses `/` separators. A SQLite URI is a URI, so `\` is not
/// a path separator there; emitting the native separator on Windows produces a
/// URL that is wrong rather than merely unusual. Windows forbids `\` inside
/// file names, so rewriting every separator is lossless.
pub(crate) fn resolve_sqlite_url(url: &str, base: &Path) -> String {
    let Some(path) = url.strip_prefix("sqlite://") else {
        return url.into();
    };
    // ":memory:" and absolute paths are already final.
    if path == ":memory:" || Path::new(path).is_absolute() {
        return url.into();
    }
    let joined = base.join(path);
    format!(
        "sqlite://{}",
        joined.display().to_string().replace('\\', "/")
    )
}
