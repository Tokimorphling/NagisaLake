use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, fmt};

/// Stable infrastructure error codes returned over the wire.
///
/// Discriminants are part of the wire format: values may be added, never
/// changed. Zero is reserved for a successful response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Code {
    /// The operation was cancelled.
    Cancelled = 1,
    /// An unclassified server failure occurred.
    Unknown = 2,
    /// The request was invalid.
    InvalidArgument = 3,
    /// The deadline expired before the operation completed.
    DeadlineExceeded = 4,
    /// The requested resource does not exist.
    NotFound = 5,
    /// The request conflicts with existing state.
    AlreadyExists = 6,
    /// The caller is not authorized.
    PermissionDenied = 7,
    /// The server has no capacity for this request.
    ResourceExhausted = 8,
    /// A required precondition was not satisfied.
    FailedPrecondition = 9,
    /// The operation is not implemented.
    Unimplemented = 10,
    /// An internal invariant failed.
    Internal = 13,
    /// The service is temporarily unavailable.
    Unavailable = 14,
    /// The caller is not authenticated.
    Unauthenticated = 16,
}

impl Code {
    /// Returns the wire representation.
    pub const fn as_wire(self) -> u8 {
        self as u8
    }

    /// Parses a wire status byte.
    ///
    /// Returns `None` for the success byte and for codes this build does not
    /// know, which lets the caller reject the frame instead of guessing.
    pub const fn from_wire(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Cancelled,
            2 => Self::Unknown,
            3 => Self::InvalidArgument,
            4 => Self::DeadlineExceeded,
            5 => Self::NotFound,
            6 => Self::AlreadyExists,
            7 => Self::PermissionDenied,
            8 => Self::ResourceExhausted,
            9 => Self::FailedPrecondition,
            10 => Self::Unimplemented,
            13 => Self::Internal,
            14 => Self::Unavailable,
            16 => Self::Unauthenticated,
            _ => return None,
        })
    }
}

/// A typed RPC infrastructure failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    code:    Code,
    message: String,
}

impl Status {
    /// Creates a status with the provided code and detail.
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns the stable status code.
    pub const fn code(&self) -> Code {
        self.code
    }

    /// Returns the human-readable detail.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Creates a deadline-exceeded status.
    pub fn deadline_exceeded() -> Self {
        Self::new(Code::DeadlineExceeded, "request deadline exceeded")
    }

    /// Creates a resource-exhausted status.
    pub fn resource_exhausted() -> Self {
        Self::new(Code::ResourceExhausted, "server request capacity exhausted")
    }

    /// Creates an already-exists status for a duplicate request identifier.
    pub fn duplicate_request_id() -> Self {
        Self::new(Code::AlreadyExists, "request id is already in flight")
    }

    /// Creates an internal status.
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(Code::Internal, detail)
    }

    /// Creates an invalid-argument status.
    pub fn invalid_argument(detail: impl Into<String>) -> Self {
        Self::new(Code::InvalidArgument, detail)
    }

    pub(crate) fn timeout_too_large() -> Self {
        Self::new(
            Code::InvalidArgument,
            "request timeout exceeds the server limit",
        )
    }

    /// Rebuilds a status from its wire code and message bytes.
    ///
    /// A malformed message is replaced rather than rejected: the code is the
    /// part callers branch on, so losing it to a decoding failure would be
    /// worse than reporting a lossy detail.
    pub(crate) fn from_wire(code: Code, message: Bytes) -> Self {
        Self {
            code,
            message: String::from_utf8_lossy(&message).into_owned(),
        }
    }

    /// Consumes the status and returns its message bytes for the wire.
    pub(crate) fn into_wire_message(self) -> Bytes {
        Bytes::from(self.message.into_bytes())
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for Status {}

impl From<Infallible> for Status {
    fn from(value: Infallible) -> Self {
        match value {}
    }
}
