//! Sanitized HTTP diagnostics and the public runtime error boundary.
use crate::util::truncate;
use nagisalake_transport::TransportError;
use std::{error::Error as StdError, fmt};
use thiserror::Error;

const HTTP_CAUSE_LIMIT: usize = 6;
const HTTP_CAUSE_CHARS: usize = 240;
const HTTP_CAUSES_TOTAL_CHARS: usize = 700;

/// URL-free diagnostic information extracted at the reqwest boundary.
///
/// A reqwest error can retain the complete request URL, including a presigned
/// object's credentials and signature. Keeping only this owned, sanitized
/// representation makes both `Display` (persisted in job events) and `Debug`
/// (written to Worker logs) safe by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpFailure {
    pub(super) operation: &'static str,
    pub(super) class:     HttpFailureClass,
    pub(super) status:    Option<u16>,
    pub(super) transient: bool,
    pub(super) attempts:  u8,
    causes:               Vec<String>,
}

impl HttpFailure {
    pub(super) fn new(
        operation: &'static str,
        class: HttpFailureClass,
        status: Option<u16>,
        transient: bool,
        attempts: u8,
        causes: Vec<String>,
    ) -> Self {
        Self {
            operation,
            class,
            status,
            transient,
            attempts,
            causes,
        }
    }

    pub(super) fn status(
        operation: &'static str,
        attempts: u8,
        status: reqwest::StatusCode,
    ) -> Self {
        Self::new(
            operation,
            HttpFailureClass::Status,
            Some(status.as_u16()),
            retryable_http_status(status),
            attempts,
            vec![format!("upstream returned HTTP status {}", status.as_u16())],
        )
    }
}

impl fmt::Display for HttpFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "operation={} class={} transient={} attempts={}",
            self.operation,
            self.class.as_str(),
            self.transient,
            self.attempts
        )?;
        if let Some(status) = self.status {
            write!(formatter, " status={status}")?;
        }
        if !self.causes.is_empty() {
            write!(formatter, " caused_by=[{}]", self.causes.join(" <- "))?;
        }
        Ok(())
    }
}

impl StdError for HttpFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HttpFailureClass {
    Timeout,
    Connect,
    Request,
    Body,
    Status,
    Redirect,
    Builder,
    Decode,
    TicketExpired,
    Unknown,
}

impl HttpFailureClass {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connect => "connect",
            Self::Request => "request",
            Self::Body => "body",
            Self::Status => "status",
            Self::Redirect => "redirect",
            Self::Builder => "builder",
            Self::Decode => "decode",
            Self::TicketExpired => "ticket_expired",
            Self::Unknown => "unknown",
        }
    }

    const fn is_transient(self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::Connect | Self::Request | Self::Body
        )
    }
}

pub(super) fn http_failure(
    operation: &'static str,
    attempts: u8,
    error: reqwest::Error,
) -> RuntimeError {
    let class = if error.is_timeout() {
        HttpFailureClass::Timeout
    } else if error.is_connect() {
        HttpFailureClass::Connect
    } else if error.is_body() {
        HttpFailureClass::Body
    } else if error.is_redirect() {
        HttpFailureClass::Redirect
    } else if error.is_builder() {
        HttpFailureClass::Builder
    } else if error.is_decode() {
        HttpFailureClass::Decode
    } else if error.is_request() {
        HttpFailureClass::Request
    } else {
        HttpFailureClass::Unknown
    };
    let status = error.status().map(|value| value.as_u16());
    // `without_url` is deliberately called before Display or source traversal.
    // This prevents a signed query from entering either the durable outbox or
    // tracing, even if a later caller formats RuntimeError with Debug.
    let safe_error = error.without_url();
    let causes = sanitized_http_causes(&safe_error);
    RuntimeError::Http(HttpFailure::new(
        operation,
        class,
        status,
        class.is_transient(),
        attempts,
        causes,
    ))
}

fn sanitized_http_causes(error: &(dyn StdError + 'static)) -> Vec<String> {
    let mut causes = Vec::new();
    let mut total_chars = 0usize;
    let mut current = Some(error);
    while let Some(cause) = current {
        if causes.len() >= HTTP_CAUSE_LIMIT || total_chars >= HTTP_CAUSES_TOTAL_CHARS {
            break;
        }
        let value = sanitize_http_cause(&cause.to_string());
        if !value.is_empty() && causes.last() != Some(&value) {
            total_chars = total_chars.saturating_add(value.chars().count());
            causes.push(value);
        }
        current = cause.source();
    }
    causes
}

fn sanitize_http_cause(value: &str) -> String {
    let normalized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let lower = normalized.to_ascii_lowercase();
    if [
        "://",
        "x-amz-",
        "signature=",
        "credential=",
        "authorization=",
        "authorization:",
        "token=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "<redacted sensitive HTTP detail>".into();
    }
    truncate(&normalized, HTTP_CAUSE_CHARS)
}

fn retryable_http_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

/// Errors crossing the worker runtime boundary.
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("invalid runtime configuration: {0}")]
    InvalidConfig(String),
    #[error("control connection is closed")]
    ConnectionClosed,
    #[error("worker job capacity is full ({0} jobs)")]
    CapacityExhausted(usize),
    #[error("job was cancelled")]
    Cancelled,
    #[error("journal operation failed: {0}")]
    Journal(String),
    #[error("workflow operation failed: {0}")]
    Workflow(String),
    #[error("ComfyUI operation failed: {0}")]
    Comfy(String),
    #[error("artifact operation failed: {0}")]
    Artifact(String),
    #[error("HTTP operation failed: {0}")]
    Http(#[source] HttpFailure),
    #[error("I/O operation failed: {0}")]
    Io(#[from] std::io::Error),
}

impl From<TransportError> for RuntimeError {
    fn from(_error: TransportError) -> Self {
        Self::ConnectionClosed
    }
}
