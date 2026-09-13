//! Tokilake SMUX transport for Nagisalake's typed control protocol.
//! Framing, WebSocket bridging, TLS, and session lifecycle are separate modules;
//! public re-exports preserve the original API for Hub and Worker consumers.
mod bridge;
mod codec;
mod connect;
mod error;
mod hub;
mod tls;

pub use codec::{HubControl, JsonLineCodec, WorkerControl};
pub use connect::{WorkerConnectConfig, WorkerTransport};
pub use error::TransportError;
pub use hub::HubTransport;
pub use tls::{ConnectScheme, WorkerTlsConfig, connect_scheme};

pub const TOKILAKE_SUBPROTOCOL: &str = "tokilake.v1";
pub const DEFAULT_MAX_CONTROL_FRAME_BYTES: usize = 1024 * 1024;

#[cfg(test)]
mod tests;
