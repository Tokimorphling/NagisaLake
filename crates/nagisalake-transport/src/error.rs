use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("invalid transport config: {0}")]
    InvalidConfig(&'static str),
    #[error("hub url {0:?} is not a ws:// or wss:// endpoint")]
    UnsupportedUrl(String),
    #[error("TLS configuration failed: {0}")]
    Tls(String),
    #[error("control frame is {actual} bytes, exceeding the {limit}-byte limit")]
    FrameTooLarge { actual: usize, limit: usize },
    #[error("Tokilake control stream closed")]
    Closed,
    #[error("timed out establishing Tokilake control stream")]
    ConnectTimeout,
    #[error("peer selected an unexpected WebSocket subprotocol")]
    UnexpectedSubprotocol,
    #[error("text WebSocket frames are not valid Tokilake transport frames")]
    UnexpectedTextFrame,
    #[error("protocol validation failed: {0}")]
    Validation(#[from] nagisalake_protocol::ValidationError),
    #[error("protocol serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Tokilake tunnel failed: {0}")]
    Tunnel(#[from] tokilake_core::error::TunnelError),
    #[error("WebSocket failed: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("WebSocket request is invalid: {0}")]
    WebSocketRequest(#[from] tokio_tungstenite::tungstenite::http::Error),
    #[error("transport I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("hub WebSocket failed: {0}")]
    AxumWebSocket(#[from] axum::Error),
    #[error("HTTP proxy failed: {0}")]
    Proxy(String),
}
