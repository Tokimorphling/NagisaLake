use super::*;

pub(super) const DEFAULT_LISTEN: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 9091);
pub(super) const MAX_NAME_CHARS: usize = 255;
pub(super) const DEFAULT_ORGANIZATION_ID: &str = "00000000-0000-0000-0000-000000000001";

/// Hub configuration loaded from TOML.
#[derive(Debug, Clone, Deserialize)]
pub struct HubConfig {
    #[serde(default)]
    pub server:       ServerConfig,
    #[serde(default)]
    pub auth:         AuthConfig,
    #[serde(default)]
    pub browser:      BrowserConfig,
    #[serde(default)]
    pub database:     Option<StoreConfig>,
    #[serde(default)]
    pub transport:    TransportConfig,
    #[serde(default)]
    pub object_store: Option<S3ObjectStoreConfig>,
    /// Federated sign-in. Absent means password-only, which on a public
    /// deployment leaves users with no way to recover a forgotten password.
    #[serde(default)]
    pub oauth:        Option<crate::oauth::OauthConfig>,
    #[serde(default)]
    pub rate_limit:   RateLimitConfig,
    #[serde(default)]
    pub log:          LogConfig,
}

/// Log verbosity. `RUST_LOG` overrides this when set, so the file value is a
/// deployment default rather than a hard setting.
#[derive(Debug, Clone, Deserialize)]
pub struct LogConfig {
    /// A `tracing-subscriber` filter directive: a bare level such as `info`, or
    /// per-target overrides such as `info,nagisalake_hub=debug`. Validated at
    /// load so a typo fails startup instead of silencing the log.
    #[serde(default = "default_log_filter")]
    pub filter: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            filter: default_log_filter(),
        }
    }
}

pub(super) fn default_log_filter() -> String {
    "info".to_owned()
}

/// Throttling for the credential and submission endpoints.
#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    /// Off only makes sense on loopback or in the legacy no-database mode.
    #[serde(default = "default_true")]
    pub enabled:             bool,
    /// Whether `X-Forwarded-For` may be believed. Enable only behind a proxy
    /// that overwrites it: trusting it otherwise gives every caller an unlimited
    /// supply of source addresses.
    #[serde(default)]
    pub trust_forwarded_for: bool,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled:             true,
            trust_forwarded_for: false,
        }
    }
}

/// Listener settings.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: DEFAULT_LISTEN,
        }
    }
}

pub(super) fn default_listen() -> SocketAddr {
    DEFAULT_LISTEN
}

/// Bearer credentials. Values may be supplied through environment variables.
#[derive(Clone, Deserialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub worker_token:           Option<String>,
    #[serde(default)]
    pub consumer_token:         Option<String>,
    #[serde(default = "default_organization_id")]
    pub legacy_organization_id: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            worker_token:           None,
            consumer_token:         None,
            legacy_organization_id: default_organization_id(),
        }
    }
}

impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthConfig")
            .field(
                "worker_token",
                &self.worker_token.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "consumer_token",
                &self.consumer_token.as_ref().map(|_| "[redacted]"),
            )
            .field("legacy_organization_id", &self.legacy_organization_id)
            .finish()
    }
}

pub(super) fn default_organization_id() -> String {
    DEFAULT_ORGANIZATION_ID.into()
}

/// Browser-session security and public control-plane settings.
#[derive(Debug, Clone, Deserialize)]
pub struct BrowserConfig {
    /// Public sign-up is opt-in. When enabled without password auth, OAuth
    /// providers must be configured and become the only registration path.
    #[serde(default)]
    pub registration_enabled:   bool,
    /// Password auth is kept for loopback/local compatibility only. Public
    /// deployments should leave this false and use the configured OAuth
    /// providers, which provide the account recovery boundary.
    #[serde(default)]
    pub password_auth_enabled:  bool,
    #[serde(default = "default_true")]
    pub cookie_secure:          bool,
    #[serde(default = "default_access_ttl")]
    pub access_ttl_seconds:     i64,
    #[serde(default = "default_refresh_ttl")]
    pub refresh_ttl_seconds:    i64,
    #[serde(default)]
    pub allowed_origins:        Vec<String>,
    /// Acknowledges an off-host deployment without HTTPS. Named for what it
    /// costs rather than what it enables, so it cannot be set casually.
    #[serde(default)]
    pub allow_insecure_cookies: bool,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            registration_enabled:   false,
            password_auth_enabled:  false,
            cookie_secure:          true,
            access_ttl_seconds:     default_access_ttl(),
            refresh_ttl_seconds:    default_refresh_ttl(),
            allowed_origins:        Vec::new(),
            allow_insecure_cookies: false,
        }
    }
}

pub(super) const fn default_true() -> bool {
    true
}
pub(super) const fn default_access_ttl() -> i64 {
    15 * 60
}
pub(super) const fn default_refresh_ttl() -> i64 {
    30 * 24 * 60 * 60
}

/// Limits and timeouts for the control plane.
#[derive(Debug, Clone, Deserialize)]
pub struct TransportConfig {
    #[serde(default = "default_max_frame")]
    pub max_frame_bytes:             usize,
    #[serde(default = "default_accept_timeout")]
    pub accept_timeout_seconds:      u64,
    #[serde(default = "default_ack_timeout")]
    pub command_ack_timeout_seconds: u64,
    #[serde(default = "default_heartbeat_seconds")]
    pub heartbeat_interval_seconds:  u64,
    #[serde(default = "default_max_artifact_bytes")]
    pub max_artifact_bytes:          u64,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            max_frame_bytes:             default_max_frame(),
            accept_timeout_seconds:      default_accept_timeout(),
            command_ack_timeout_seconds: default_ack_timeout(),
            heartbeat_interval_seconds:  default_heartbeat_seconds(),
            max_artifact_bytes:          default_max_artifact_bytes(),
        }
    }
}

pub(super) const fn default_max_frame() -> usize {
    DEFAULT_MAX_CONTROL_FRAME_BYTES
}

pub(super) const fn default_accept_timeout() -> u64 {
    15
}

pub(super) const fn default_ack_timeout() -> u64 {
    10
}

pub(super) const fn default_heartbeat_seconds() -> u64 {
    15
}

pub(super) const fn default_max_artifact_bytes() -> u64 {
    5 * 1024 * 1024 * 1024
}

impl HubConfig {
    /// Reads, expands, and validates a TOML file.
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, HubError> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path).map_err(|source| HubError::ConfigIo {
            path: path.to_path_buf(),
            source,
        })?;
        let mut config: Self = toml::from_str(&raw).map_err(HubError::ConfigParse)?;
        if config
            .auth
            .worker_token
            .as_deref()
            .is_none_or(str::is_empty)
        {
            config.auth.worker_token = env::var("NAGISALAKE_WORKER_TOKEN").ok();
        }
        if config
            .auth
            .consumer_token
            .as_deref()
            .is_none_or(str::is_empty)
        {
            config.auth.consumer_token = env::var("NAGISALAKE_API_TOKEN").ok();
        }
        if let Ok(url) = env::var("NAGISALAKE_DATABASE_URL") {
            if let Some(database) = config.database.as_mut() {
                database.url = url;
            } else {
                config.database = Some(StoreConfig {
                    url,
                    max_connections: 16,
                    run_migrations: true,
                });
            }
        }
        config.validate()?;
        Ok(config)
    }

    /// Validates values that would otherwise create an unusable server.
    pub fn validate(&self) -> Result<(), HubError> {
        if let Err(error) = self.log.filter.parse::<tracing_subscriber::EnvFilter>() {
            return Err(HubError::InvalidConfig(format!(
                "log.filter {:?} is not a valid filter directive: {error}",
                self.log.filter
            )));
        }
        if self.database.is_none()
            && self
                .auth
                .worker_token
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        {
            return Err(HubError::InvalidConfig(
                "auth.worker_token or NAGISALAKE_WORKER_TOKEN is required".into(),
            ));
        }
        if self.database.is_none()
            && self
                .auth
                .consumer_token
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        {
            return Err(HubError::InvalidConfig(
                "auth.consumer_token or NAGISALAKE_API_TOKEN is required".into(),
            ));
        }
        if self.transport.max_frame_bytes == 0
            || self.transport.accept_timeout_seconds == 0
            || self.transport.command_ack_timeout_seconds == 0
            || self.transport.heartbeat_interval_seconds == 0
            || self.transport.max_artifact_bytes == 0
        {
            return Err(HubError::InvalidConfig(
                "transport limits must be greater than zero".into(),
            ));
        }
        if self.auth.legacy_organization_id.trim().is_empty() {
            return Err(HubError::InvalidConfig(
                "auth.legacy_organization_id must not be empty".into(),
            ));
        }
        if self.browser.access_ttl_seconds <= 0 || self.browser.refresh_ttl_seconds <= 0 {
            return Err(HubError::InvalidConfig(
                "browser session TTL values must be greater than zero".into(),
            ));
        }
        self.validate_exposure()?;
        if self.browser.registration_enabled
            && !self.browser.password_auth_enabled
            && self
                .oauth
                .as_ref()
                .is_none_or(|oauth| oauth.providers.is_empty())
            && self.server.listen.ip().is_loopback()
        {
            return Err(HubError::InvalidConfig(
                "registration_enabled requires OAuth providers when password_auth_enabled is false"
                    .into(),
            ));
        }
        if let Some(oauth) = &self.oauth {
            // Resolving loads every provider secret, so a typo in an env var name
            // fails startup instead of producing a sign-in button that 500s.
            oauth
                .resolve()
                .map_err(|error| HubError::InvalidConfig(error.to_string()))?;
        }
        Ok(())
    }

    /// Refuses configurations that are unsafe once the Hub is reachable off-host.
    ///
    /// These are startup errors rather than warnings because a warning in a log
    /// nobody reads is how a session cookie ends up travelling in clear text.
    /// Each can be overridden deliberately; none can be hit by accident.
    fn validate_exposure(&self) -> Result<(), HubError> {
        let loopback_only = self.server.listen.ip().is_loopback();

        // A session cookie without Secure travels over plain HTTP, so anyone on
        // the path can take the session. Acceptable on loopback, never off-host.
        if !self.browser.cookie_secure && !loopback_only && !self.browser.allow_insecure_cookies {
            return Err(HubError::InvalidConfig(format!(
                "browser.cookie_secure is false while listening on {}. Session cookies would be \
                 sent over plain HTTP. Serve HTTPS and set cookie_secure = true, bind to \
                 127.0.0.1, or set browser.allow_insecure_cookies = true for a trusted LAN.",
                self.server.listen
            )));
        }

        // Open registration on a reachable address with no throttle is an open
        // invitation to fill the user table.
        if self.browser.registration_enabled
            && !loopback_only
            && !self.rate_limit.enabled
            && !self.browser.allow_insecure_cookies
        {
            return Err(HubError::InvalidConfig(format!(
                "registration is enabled on {} with rate_limit.enabled = false. Enable rate \
                 limiting, close registration, or acknowledge the trusted-network setup with \
                 browser.allow_insecure_cookies = true.",
                self.server.listen
            )));
        }

        // Password registration without an email recovery path is not a public
        // product. OAuth-only is the supported public account lifecycle.
        if self.browser.registration_enabled
            && !loopback_only
            && !self.browser.allow_insecure_cookies
            && self.browser.password_auth_enabled
        {
            return Err(HubError::InvalidConfig(
                "password_auth_enabled cannot be used for public registration; disable it and \
                 configure OAuth providers"
                    .into(),
            ));
        }
        if self.browser.registration_enabled
            && !loopback_only
            && !self.browser.password_auth_enabled
            && self
                .oauth
                .as_ref()
                .is_none_or(|oauth| oauth.providers.is_empty())
        {
            return Err(HubError::InvalidConfig(
                "public registration requires at least one OAuth provider".into(),
            ));
        }

        // Behind a proxy the peer address is always the proxy, so a per-address
        // limit would treat every user as one client.
        if self.rate_limit.enabled
            && !loopback_only
            && !self.rate_limit.trust_forwarded_for
            && self.oauth.is_some()
        {
            tracing::warn!(
                "rate_limit.trust_forwarded_for is false; if this Hub sits behind a reverse \
                 proxy, every request will be attributed to the proxy's address and per-address \
                 limits will apply to all users together"
            );
        }

        // The legacy static tokens bypass all account checks. Shipping the
        // example values on a reachable address is a credential leak.
        const EXAMPLE_TOKENS: [&str; 4] = [
            "development-worker-token",
            "development-api-token",
            "local-stack-legacy-worker",
            "local-stack-legacy-consumer",
        ];
        if !loopback_only {
            for token in [
                self.auth.worker_token.as_deref(),
                self.auth.consumer_token.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                if EXAMPLE_TOKENS.contains(&token) {
                    return Err(HubError::InvalidConfig(format!(
                        "auth token {token:?} is an example value and this Hub is reachable on \
                         {}. Replace it or remove the legacy token entirely.",
                        self.server.listen
                    )));
                }
            }
        }
        Ok(())
    }
}
