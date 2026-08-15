//! High-throughput, Tokio-native RPC primitives for Nagisalake services.
//!
//! One connection carries many concurrent calls, applies bounded backpressure,
//! propagates deadlines, and cancels server work when the client future is
//! dropped.
//!
//! ## Request flow
//!
//! ```text
//! Client::call            encode request        (caller's task)
//!   -> bounded client queue
//!   -> dispatcher: in-flight table, deadlines   (one task per connection)
//!   -> writer: length-prefixed frames, coalesced
//!   ~~ wire ~~
//!   -> reader: parses headers, opaque bodies    (one task per connection)
//!   -> supervisor: admission control, cancellation
//!   -> handler task: decode, layers, service, encode
//! ```
//!
//! ## Design notes
//!
//! Application encoding and decoding run on the task that owns the call, never on
//! a connection task, so codec cost scales with the runtime's workers instead of
//! queueing behind a single connection. Frame bodies move as [`bytes::Bytes`], so
//! handing a payload between tasks is a refcount bump.
//!
//! Frames carry fixed binary headers, so routing a response or a cancellation
//! does not deserialize an application message.
//!
//! Service layers are statically dispatched. The only type erasure is the client's
//! codec, which keeps [`Client`] a two-parameter type that application code can
//! name and store.
//!
//! ## Failure isolation
//!
//! A codec failure fails one call. A framing, protocol, or I/O failure ends the
//! connection, because the byte stream can no longer be trusted, and every
//! pending call on it completes with [`RpcError::Connection`].

#![deny(missing_docs)]

mod client;
mod codec;
mod config;
mod context;
mod error;
mod framing;
mod protocol;
mod server;
mod status;
mod transport;

pub use client::{Client, ClientBuilder, ClientConfig, MissingTransport};
pub use codec::{BincodeCodec, Codec, CodecError};
pub use config::FrameConfig;
pub use context::{ClientContext, Principal, ServerContext, TraceId};
pub use error::{
    ConfigError, ConnectError, ConnectionError, ConnectionErrorKind, RpcError, ServeError,
};
pub use motore::{
    Service,
    layer::{Identity, Layer, Stack, layer_fn},
    service::{ServiceFn, service_fn},
};
pub use server::{Server, ServerConfig};
pub use status::{Code, Status};
pub use transport::{
    Accepted, Incoming, MakeIncoming, MakeTransport, SplitDuplex, TcpConnector, TcpIncoming,
    TcpIncomingMaker, Transport,
};

/// Wire-level frames, for diagnostics and protocol tests.
///
/// Application code uses [`Client`] and [`Server`] instead.
pub mod wire {
    pub use crate::protocol::{ClientFrame, ServerFrame};
}
