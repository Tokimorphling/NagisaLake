//! Provisioned state reading, URL validation, and safety limits.

use crate::args::{
    Args, MAX_DURATION_SECONDS, MAX_IN_FLIGHT, MAX_JOB_DRAIN_SECONDS, MAX_JOB_STEP_DELAY_MS,
    MAX_RATE_PER_SECOND, MAX_TENANTS, MAX_TOTAL_REQUESTS, MAX_USERS, MAX_WORKER_CAPACITY,
    MAX_WORKERS, WORKFLOW_ID, WORKFLOW_VERSION,
};
use anyhow::{Context, anyhow, bail};
use nagisalake_protocol::{
    MAX_IDENTITY_CHARS, PROTOCOL_VERSION, Register, Validate, WorkerCapabilities,
    WorkflowCapability,
};
use reqwest::Url;
use serde::Deserialize;
use std::{env, io::Read, path::Path};

/// Provisioned state. Deliberately does not implement `Debug`: an accidental
/// `{:?}` must never copy bearer secrets into CI output or an incident report.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SecretState {
    pub(crate) base_url:   String,
    #[serde(default)]
    pub(crate) worker_url: Option<String>,
    pub(crate) tenants:    Vec<TenantSecrets>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TenantSecrets {
    pub(crate) organization_id:  String,
    pub(crate) api_keys:         Vec<String>,
    pub(crate) worker_tokens:    Vec<String>,
    pub(crate) worker_namespace: String,
}

pub(crate) struct ValidatedState {
    pub(crate) base_url:   Url,
    pub(crate) worker_url: String,
    pub(crate) tenants:    Vec<ValidatedTenant>,
}

pub(crate) struct ValidatedTenant {
    pub(crate) organization_id:  String,
    pub(crate) api_keys:         Vec<String>,
    pub(crate) worker_tokens:    Vec<String>,
    pub(crate) worker_namespace: String,
}

pub(crate) fn read_state(source: &str) -> anyhow::Result<SecretState> {
    let raw = if source == "-" {
        match env::var("NAGISALAKE_LOADGEN_STATE_JSON") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => {
                let mut value = String::new();
                std::io::stdin()
                    .read_to_string(&mut value)
                    .context("read load state from stdin")?;
                value
            }
        }
    } else {
        std::fs::read_to_string(source)
            .with_context(|| format!("read load state from {}", display_path(source)))?
    };
    if raw.trim().is_empty() {
        bail!("load state is empty");
    }
    serde_json::from_str(&raw).context("parse load state JSON")
}

fn display_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("<state-file>")
        .to_owned()
}

pub(crate) fn validate(args: &Args, state: SecretState) -> anyhow::Result<ValidatedState> {
    validate_limits(args)?;
    if state.tenants.is_empty() || state.tenants.len() > MAX_TENANTS {
        bail!("tenant count must be between 1 and {MAX_TENANTS}");
    }
    if args.workers.saturating_mul(state.tenants.len()) > MAX_WORKERS {
        bail!("workers per tenant multiplied by tenant count exceeds {MAX_WORKERS}");
    }
    if args.users.saturating_mul(state.tenants.len()) > MAX_USERS {
        bail!("users per tenant multiplied by tenant count exceeds {MAX_USERS}");
    }

    let mut base_url = Url::parse(state.base_url.trim()).context("base_url is not a valid URL")?;
    validate_http_url(&base_url, "base_url")?;
    if base_url.path() != "/" && !base_url.path().is_empty() {
        bail!("base_url must not contain a path");
    }
    base_url.set_path("");
    let host = base_url
        .host_str()
        .ok_or_else(|| anyhow!("base_url hostname is required"))?;
    if !is_loopback_host(host) && base_url.scheme() != "https" {
        bail!(
            "non-loopback base_url must use https:// so bearer secrets are not sent in cleartext"
        );
    }
    require_production_confirmation(host, args.confirm_production_host.as_deref())?;

    let worker_url = match state.worker_url {
        Some(value) => {
            validate_worker_url(&value, host, !is_loopback_host(host))?;
            value
        }
        None => {
            let scheme = if base_url.scheme() == "https" {
                "wss"
            } else {
                "ws"
            };
            let authority = match base_url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_owned(),
            };
            format!("{scheme}://{authority}/v1/worker/connect")
        }
    };

    let mut tenants = Vec::with_capacity(state.tenants.len());
    for tenant in state.tenants {
        if tenant.organization_id.trim().is_empty() {
            bail!("every tenant organization_id is required");
        }
        let identity = Register {
            protocol_version: PROTOCOL_VERSION,
            namespace:        tenant.worker_namespace.clone(),
            node_name:        "mock-001".into(),
            worker_version:   env!("CARGO_PKG_VERSION").into(),
            capabilities:     WorkerCapabilities {
                workflows: vec![WorkflowCapability {
                    id:           WORKFLOW_ID.into(),
                    version:      WORKFLOW_VERSION.into(),
                    output_types: Vec::new(),
                    manifest:     None,
                }],
                ..WorkerCapabilities::default()
            },
            recovery_job_ids: Vec::new(),
        };
        if identity.validate().is_err() {
            bail!(
                "every worker_namespace must be a valid protocol identity of at most \
                 {MAX_IDENTITY_CHARS} characters"
            );
        }
        if args.users > tenant.api_keys.len() {
            bail!("--users exceeds the number of api_keys in a tenant");
        }
        if tenant.worker_tokens.is_empty() {
            bail!("every tenant requires at least one worker token");
        }
        if tenant.api_keys.iter().any(|key| !valid_secret(key, "nsk_")) {
            bail!("every API key must be a non-whitespace nsk_ bearer secret");
        }
        if tenant
            .worker_tokens
            .iter()
            .any(|token| !valid_secret(token, "nwk_"))
        {
            bail!("every worker token must be a non-whitespace nwk_ bearer secret");
        }
        tenants.push(ValidatedTenant {
            organization_id:  tenant.organization_id,
            api_keys:         tenant.api_keys,
            worker_tokens:    tenant.worker_tokens,
            worker_namespace: tenant.worker_namespace,
        });
    }
    Ok(ValidatedState {
        base_url,
        worker_url,
        tenants,
    })
}

pub(crate) fn validate_limits(args: &Args) -> anyhow::Result<()> {
    if args.workers == 0 || args.workers > MAX_WORKERS {
        bail!("--workers must be between 1 and {MAX_WORKERS}");
    }
    if args.users == 0 || args.users > MAX_USERS {
        bail!("--users must be between 1 and {MAX_USERS}");
    }
    if args.worker_parallelism == 0
        || args
            .worker_parallelism
            .saturating_add(args.worker_queue_depth)
            > MAX_WORKER_CAPACITY
    {
        bail!("worker parallelism plus queue depth must be between 1 and {MAX_WORKER_CAPACITY}");
    }
    if !args.rate.is_finite() || args.rate <= 0.0 || args.rate > MAX_RATE_PER_SECOND {
        bail!("--rate must be greater than 0 and no more than {MAX_RATE_PER_SECOND}");
    }
    if args.duration_seconds == 0 || args.duration_seconds > MAX_DURATION_SECONDS {
        bail!("--duration-seconds must be between 1 and {MAX_DURATION_SECONDS}");
    }
    if args.max_in_flight == 0 || args.max_in_flight > MAX_IN_FLIGHT {
        bail!("--max-in-flight must be between 1 and {MAX_IN_FLIGHT}");
    }
    if args.read_percent > 100 {
        bail!("--read-percent must be between 0 and 100");
    }
    if args.job_step_delay_ms > MAX_JOB_STEP_DELAY_MS {
        bail!("--job-step-delay-ms must be no more than {MAX_JOB_STEP_DELAY_MS}");
    }
    if args.health_interval_seconds == 0 || args.health_interval_seconds > 30 {
        bail!("--health-interval-seconds must be between 1 and 30");
    }
    if args.job_drain_seconds == 0 || args.job_drain_seconds > MAX_JOB_DRAIN_SECONDS {
        bail!("--job-drain-seconds must be between 1 and {MAX_JOB_DRAIN_SECONDS}");
    }
    let planned = args.rate * args.duration_seconds as f64;
    if planned.ceil() as u64 > MAX_TOTAL_REQUESTS {
        bail!("planned requests exceed the hard cap of {MAX_TOTAL_REQUESTS}");
    }
    Ok(())
}

fn valid_secret(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() > prefix.len()
        && !value.chars().any(char::is_whitespace)
}

fn validate_http_url(url: &Url, field: &str) -> anyhow::Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("{field} must use http:// or https://");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("{field} must not contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("{field} must not contain a query or fragment");
    }
    Ok(())
}

fn validate_worker_url(raw: &str, expected_host: &str, require_tls: bool) -> anyhow::Result<()> {
    let url = Url::parse(raw.trim()).context("worker_url is not a valid URL")?;
    if !matches!(url.scheme(), "ws" | "wss") {
        bail!("worker_url must use ws:// or wss://");
    }
    if require_tls && url.scheme() != "wss" {
        bail!(
            "non-loopback worker_url must use wss:// so bearer secrets are not sent in cleartext"
        );
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("worker_url must not contain credentials, a query, or a fragment");
    }
    if url.host_str() != Some(expected_host) {
        bail!("worker_url hostname must match base_url hostname");
    }
    if url.path() != "/v1/worker/connect" {
        bail!("worker_url path must be /v1/worker/connect");
    }
    Ok(())
}

fn require_production_confirmation(host: &str, confirmation: Option<&str>) -> anyhow::Result<()> {
    if !is_loopback_host(host) && confirmation != Some(host) {
        bail!("non-loopback target requires --confirm-production-host with the exact hostname");
    }
    Ok(())
}

pub(crate) fn is_loopback_host(host: &str) -> bool {
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'));
    let candidate = unbracketed.unwrap_or(host);
    candidate.eq_ignore_ascii_case("localhost")
        || candidate
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
        || candidate.to_ascii_lowercase().ends_with(".localhost")
}
