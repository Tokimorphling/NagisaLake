use crate::Status;
use std::{fmt, io};
use thiserror::Error;

/// Invalid runtime configuration.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    /// A required capacity was zero.
    #[error("{0} must be greater than zero")]
    Zero(&'static str),
    /// The frame limit cannot be represented by the wire prefix.
    #[error("max_frame_bytes must be between 1 and u32::MAX, got {0}")]
    InvalidFrameLimit(usize),
    /// A duration falls outside the runtime's supported range.
    #[error("{0} must be greater than zero and at most 24 hours")]
    InvalidDuration(&'static str),
}

/// Failure while constructing a client connection.
#[derive(Debug, Error)]
pub enum ConnectError {
    /// Client settings are invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The transport maker could not connect.
    #[error("transport connect failed: {0}")]
    Io(#[from] io::Error),
}

/// Failure while starting or serving an RPC listener.
#[derive(Debug, Error)]
pub enum ServeError {
    /// Server settings are invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The incoming transport source failed.
    #[error("incoming transport failed: {0}")]
    Io(#[from] io::Error),
    /// A directly served connection failed.
    #[error(transparent)]
    Connection(#[from] ConnectionError),
}

/// Broad connection failure category suitable for cloning across pending calls.
///
/// Message codec failures are absent by design: they belong to one call
/// ([`RpcError::Codec`]) and never end a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionErrorKind {
    /// The peer closed the stream.
    Closed,
    /// Socket or stream I/O failed.
    Io,
    /// A frame exceeded the configured limit.
    FrameTooLarge,
    /// A peer sent a frame this protocol version cannot parse.
    Protocol,
    /// A background RPC task stopped unexpectedly.
    Runtime,
}

impl fmt::Display for ConnectionErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// A cloneable connection failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{kind}: {detail}")]
pub struct ConnectionError {
    kind:   ConnectionErrorKind,
    detail: String,
}

impl ConnectionError {
    /// Creates a connection failure.
    pub fn new(kind: ConnectionErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// Returns the broad failure category.
    pub const fn kind(&self) -> ConnectionErrorKind {
        self.kind
    }

    /// Returns the human-readable detail.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub(crate) fn io(error: io::Error) -> Self {
        let kind = if error.kind() == io::ErrorKind::UnexpectedEof {
            ConnectionErrorKind::Closed
        } else {
            ConnectionErrorKind::Io
        };
        Self::new(kind, error.to_string())
    }

    pub(crate) fn frame_too_large(actual: usize, limit: usize) -> Self {
        Self::new(
            ConnectionErrorKind::FrameTooLarge,
            format!("frame is {actual} bytes, limit is {limit}"),
        )
    }

    pub(crate) fn protocol(detail: impl Into<String>) -> Self {
        Self::new(ConnectionErrorKind::Protocol, detail)
    }

    pub(crate) fn closed(detail: impl Into<String>) -> Self {
        Self::new(ConnectionErrorKind::Closed, detail)
    }

    pub(crate) fn runtime(detail: impl Into<String>) -> Self {
        Self::new(ConnectionErrorKind::Runtime, detail)
    }
}

/// Failure returned by [`crate::Client::call`].
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RpcError {
    /// The request or response could not be serialized by the local codec.
    ///
    /// Only this call fails; the connection keeps serving other calls.
    #[error(transparent)]
    Codec(#[from] crate::CodecError),
    /// The encoded request exceeds the connection's frame limit.
    ///
    /// Detected before anything is written, so only this call fails.
    #[error("encoded request is {bytes} bytes, limit is {limit}")]
    RequestTooLarge {
        /// Encoded request size.
        bytes: usize,
        /// Configured limit for one request body.
        limit: usize,
    },
    /// The call's deadline expired.
    #[error("request deadline exceeded")]
    DeadlineExceeded,
    /// The server returned an infrastructure status.
    #[error("remote status: {0}")]
    Remote(Status),
    /// The connection failed or shut down.
    #[error("connection failed: {0}")]
    Connection(ConnectionError),
    /// The client dispatch task is no longer running.
    #[error("client is shut down")]
    Shutdown,
}
