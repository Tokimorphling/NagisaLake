//! Tokilake SMUX transport for Nagisalake's typed control protocol.

use axum::extract::ws::{Message as AxumMessage, WebSocket as AxumWebSocket};
use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use nagisalake_protocol::{HubMessage, Validate, WorkerMessage};
use rustls::pki_types::{CertificateDer, pem::PemObject};
use serde::{Serialize, de::DeserializeOwned};
use std::{sync::Arc, time::Duration};
use thiserror::Error;
use tokilake_core::tunnel::{TunnelSession, TunnelStream};
use tokilake_smux::{Config as SmuxConfig, Session as SmuxSession, Stream as SmuxStream};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    net::TcpStream,
    sync::Mutex,
    task::JoinHandle,
};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, client_async_tls_with_config,
    connect_async_tls_with_config,
    tungstenite::{
        Message as TungsteniteMessage,
        client::IntoClientRequest,
        handshake::client::{Request, Response},
        http::{
            Uri,
            header::{AUTHORIZATION, SEC_WEBSOCKET_PROTOCOL, USER_AGENT},
        },
    },
};

pub const TOKILAKE_SUBPROTOCOL: &str = "tokilake.v1";
pub const DEFAULT_MAX_CONTROL_FRAME_BYTES: usize = 1024 * 1024;
const BRIDGE_CAPACITY_BYTES: usize = 1024 * 1024;

pub struct JsonLineCodec<S> {
    stream:          S,
    read_buffer:     BytesMut,
    max_frame_bytes: usize,
}

impl<S> JsonLineCodec<S>
where
    S: TunnelStream,
{
    pub fn new(stream: S, max_frame_bytes: usize) -> Result<Self, TransportError> {
        if max_frame_bytes == 0 {
            return Err(TransportError::InvalidConfig(
                "max_frame_bytes must be greater than zero",
            ));
        }
        Ok(Self {
            stream,
            read_buffer: BytesMut::with_capacity(max_frame_bytes.min(8 * 1024)),
            max_frame_bytes,
        })
    }

    pub async fn send<T: Serialize>(&mut self, message: &T) -> Result<(), TransportError> {
        let mut frame = serde_json::to_vec(message)?;
        if frame.len() > self.max_frame_bytes {
            return Err(TransportError::FrameTooLarge {
                actual: frame.len(),
                limit:  self.max_frame_bytes,
            });
        }
        frame.push(b'\n');
        let mut written = 0;
        while written < frame.len() {
            let count = self.stream.write(&frame[written..]).await?;
            if count == 0 {
                return Err(TransportError::Closed);
            }
            written += count;
        }
        self.stream.flush().await?;
        Ok(())
    }

    pub async fn receive<T: DeserializeOwned>(&mut self) -> Result<Option<T>, TransportError> {
        loop {
            if let Some(position) = self.read_buffer.iter().position(|byte| *byte == b'\n') {
                if position > self.max_frame_bytes {
                    return Err(TransportError::FrameTooLarge {
                        actual: position,
                        limit:  self.max_frame_bytes,
                    });
                }
                let mut frame = self.read_buffer.split_to(position + 1);
                frame.truncate(position);
                if frame.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                return Ok(Some(serde_json::from_slice(&frame)?));
            }
            if self.read_buffer.len() > self.max_frame_bytes {
                return Err(TransportError::FrameTooLarge {
                    actual: self.read_buffer.len(),
                    limit:  self.max_frame_bytes,
                });
            }
            let mut chunk = [0u8; 8 * 1024];
            let count = self.stream.read(&mut chunk).await?;
            if count == 0 {
                if self.read_buffer.is_empty() {
                    return Ok(None);
                }
                if self.read_buffer.len() > self.max_frame_bytes {
                    return Err(TransportError::FrameTooLarge {
                        actual: self.read_buffer.len(),
                        limit:  self.max_frame_bytes,
                    });
                }
                let frame = self.read_buffer.split().freeze();
                return Ok(Some(serde_json::from_slice(&frame)?));
            }
            self.read_buffer.extend_from_slice(&chunk[..count]);
        }
    }

    pub async fn close(&mut self) -> Result<(), TransportError> {
        self.stream.close().await?;
        Ok(())
    }

    pub fn into_inner(self) -> S {
        self.stream
    }
}

pub struct WorkerControl<S> {
    codec: JsonLineCodec<S>,
}

impl<S: TunnelStream> WorkerControl<S> {
    pub fn new(stream: S, max_frame_bytes: usize) -> Result<Self, TransportError> {
        Ok(Self {
            codec: JsonLineCodec::new(stream, max_frame_bytes)?,
        })
    }

    pub async fn send(&mut self, message: &WorkerMessage) -> Result<(), TransportError> {
        message.validate()?;
        self.codec.send(message).await
    }

    pub async fn receive(&mut self) -> Result<Option<HubMessage>, TransportError> {
        let message = self.codec.receive::<HubMessage>().await?;
        if let Some(message) = &message {
            message.validate()?;
        }
        Ok(message)
    }
}

pub struct HubControl<S> {
    codec: JsonLineCodec<S>,
}

impl<S: TunnelStream> HubControl<S> {
    pub fn new(stream: S, max_frame_bytes: usize) -> Result<Self, TransportError> {
        Ok(Self {
            codec: JsonLineCodec::new(stream, max_frame_bytes)?,
        })
    }

    pub async fn send(&mut self, message: &HubMessage) -> Result<(), TransportError> {
        message.validate()?;
        self.codec.send(message).await
    }

    pub async fn receive(&mut self) -> Result<Option<WorkerMessage>, TransportError> {
        let message = self.codec.receive::<WorkerMessage>().await?;
        if let Some(message) = &message {
            message.validate()?;
        }
        Ok(message)
    }
}

#[derive(Debug, Clone)]
pub struct WorkerConnectConfig {
    pub url:             String,
    pub token:           String,
    /// Optional HTTP proxy used to establish the outbound WebSocket tunnel.
    /// TLS still terminates at the Hub hostname after the proxy CONNECT.
    pub proxy:           Option<String>,
    pub connect_timeout: Duration,
    pub max_frame_bytes: usize,
    pub smux:            SmuxConfig,
    pub tls:             WorkerTlsConfig,
}

impl WorkerConnectConfig {
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            url:             url.into(),
            token:           token.into(),
            proxy:           None,
            connect_timeout: Duration::from_secs(15),
            max_frame_bytes: DEFAULT_MAX_CONTROL_FRAME_BYTES,
            smux:            SmuxConfig::default(),
            tls:             WorkerTlsConfig::default(),
        }
    }
}

/// TLS settings for a `wss://` Hub url.
///
/// Empty is the common case: a Hub behind a public certificate needs nothing
/// here, because the bundled webpki root store already trusts its issuer.
#[derive(Debug, Clone, Default)]
pub struct WorkerTlsConfig {
    /// PEM-encoded CA certificates to trust *in addition to* the built-in
    /// roots, for a Hub whose certificate comes from a private CA.
    ///
    /// These have to be certificate authorities. A self-signed server leaf
    /// without `basicConstraints: CA:TRUE` is not a usable trust anchor and
    /// webpki rejects the chain as having an unknown issuer.
    pub extra_root_certificates: Vec<Vec<u8>>,
}

/// Transport-level scheme of a Hub url.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectScheme {
    /// Cleartext WebSocket.
    Plain,
    /// WebSocket over TLS.
    Tls,
}

/// Classifies a Hub url, rejecting anything the worker cannot dial.
///
/// Worth doing before a connection is attempted: `ws`/`wss` are the only
/// schemes tungstenite dials, and it reports the rest as a generic url error
/// on every reconnect. An `https://` paste is a config mistake, and saying so
/// once at startup beats an obscure failure looping forever.
pub fn connect_scheme(url: &str) -> Result<ConnectScheme, TransportError> {
    let uri = url
        .trim()
        .parse::<Uri>()
        .map_err(|_| TransportError::UnsupportedUrl(url.trim().into()))?;
    match uri.scheme_str() {
        Some("ws") => Ok(ConnectScheme::Plain),
        Some("wss") => Ok(ConnectScheme::Tls),
        _ => Err(TransportError::UnsupportedUrl(url.trim().into())),
    }
}

pub struct WorkerTransport {
    session: Arc<Mutex<SmuxSession>>,
    control: WorkerControl<SmuxStream>,
    bridge:  JoinHandle<Result<(), TransportError>>,
}

impl WorkerTransport {
    pub async fn connect(config: WorkerConnectConfig) -> Result<Self, TransportError> {
        validate_connect_config(&config)?;
        let scheme = connect_scheme(&config.url)?;
        let connector = match scheme {
            ConnectScheme::Tls => Some(tls_connector(&config.tls)?),
            // Extra roots on a cleartext url would silently secure nothing, so
            // treat it as the configuration error it is rather than ignoring it.
            ConnectScheme::Plain if !config.tls.extra_root_certificates.is_empty() => {
                return Err(TransportError::InvalidConfig(
                    "tls certificates are configured but the hub url is not wss://",
                ));
            }
            ConnectScheme::Plain => None,
        };
        let mut request = config.url.clone().into_client_request()?;
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {}", config.token)
                .parse()
                .map_err(|_| TransportError::InvalidConfig("worker token is not a valid header"))?,
        );
        request.headers_mut().insert(
            USER_AGENT,
            "nagisalake-worker/0.1"
                .parse()
                .expect("static user agent is valid"),
        );
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            TOKILAKE_SUBPROTOCOL
                .parse()
                .expect("static subprotocol is valid"),
        );
        let (socket, response) =
            if let Some(proxy) = config.proxy.as_deref().filter(|value| !value.is_empty()) {
                tokio::time::timeout(
                    config.connect_timeout,
                    connect_via_http_proxy(request, proxy, connector),
                )
                .await
                .map_err(|_| TransportError::ConnectTimeout)??
            } else {
                tokio::time::timeout(
                    config.connect_timeout,
                    // Nagle's algorithm holds a small write back waiting for company.
                    // The control channel is a stream of individually meaningful JSON
                    // frames — a dispatch, an ack, a heartbeat — so delaying one to
                    // coalesce it with the next only adds latency to both.
                    connect_async_tls_with_config(request, None, true, connector),
                )
                .await
                .map_err(|_| TransportError::ConnectTimeout)??
            };
        if response
            .headers()
            .get(SEC_WEBSOCKET_PROTOCOL)
            .and_then(|value| value.to_str().ok())
            != Some(TOKILAKE_SUBPROTOCOL)
        {
            return Err(TransportError::UnexpectedSubprotocol);
        }
        let (io, bridge) = tungstenite_bridge(socket);
        let mut session = SmuxSession::client(io, config.smux);
        let stream = tokio::time::timeout(config.connect_timeout, session.open())
            .await
            .map_err(|_| TransportError::ConnectTimeout)?
            .ok_or(TransportError::Closed)?;
        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            control: WorkerControl::new(stream, config.max_frame_bytes)?,
            bridge,
        })
    }

    pub fn control_mut(&mut self) -> &mut WorkerControl<SmuxStream> {
        &mut self.control
    }

    pub async fn open_stream(&self) -> Result<SmuxStream, TransportError> {
        self.session
            .lock()
            .await
            .open_stream()
            .await
            .map_err(Into::into)
    }

    pub fn is_alive(&self) -> bool {
        !self.bridge.is_finished()
    }
}

type ProxyWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn connect_via_http_proxy(
    request: Request,
    proxy_url: &str,
    connector: Option<Connector>,
) -> Result<(ProxyWebSocket, Response), TransportError> {
    let target_host = request
        .uri()
        .host()
        .ok_or_else(|| TransportError::Proxy("hub URL has no host".into()))?;
    let target_port = request
        .uri()
        .port_u16()
        .or_else(|| match request.uri().scheme_str() {
            Some("wss") => Some(443),
            Some("ws") => Some(80),
            _ => None,
        })
        .ok_or_else(|| TransportError::Proxy("hub URL has no port".into()))?;
    let (proxy_host, proxy_port) = parse_http_proxy(proxy_url)?;
    let proxy_address = format_host_port(&proxy_host, proxy_port);
    let target_authority = format_host_port(target_host, target_port);

    let mut socket = TcpStream::connect(proxy_address).await?;
    socket.set_nodelay(true)?;
    let connect_request = format!(
        "CONNECT {target_authority} HTTP/1.1\r\nHost: {target_authority}\r\nProxy-Connection: \
         Keep-Alive\r\n\r\n"
    );
    socket.write_all(connect_request.as_bytes()).await?;
    let status_line = read_proxy_status_line(&mut socket).await?;
    let status = status_line
        .split_ascii_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| TransportError::Proxy(format!("invalid CONNECT response: {status_line}")))?;
    if status != 200 {
        return Err(TransportError::Proxy(format!(
            "HTTP CONNECT returned {status}: {status_line}"
        )));
    }

    client_async_tls_with_config(request, socket, None, connector)
        .await
        .map_err(TransportError::WebSocket)
}

fn parse_http_proxy(proxy_url: &str) -> Result<(String, u16), TransportError> {
    let uri = proxy_url
        .trim()
        .parse::<Uri>()
        .map_err(|_| TransportError::Proxy("proxy URL is invalid".into()))?;
    match uri.scheme_str() {
        Some("http") => {}
        _ => {
            return Err(TransportError::Proxy("proxy URL must use http://".into()));
        }
    }
    let host = uri
        .host()
        .ok_or_else(|| TransportError::Proxy("proxy URL has no host".into()))?;
    let port = uri.port_u16().unwrap_or(80);
    Ok((host.to_owned(), port))
}

fn format_host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

async fn read_proxy_status_line(socket: &mut TcpStream) -> Result<String, TransportError> {
    const MAX_PROXY_HEADER_BYTES: usize = 16 * 1024;
    let mut header = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        if header.len() >= MAX_PROXY_HEADER_BYTES {
            return Err(TransportError::Proxy(
                "HTTP CONNECT response headers are too large".into(),
            ));
        }
        socket.read_exact(&mut byte).await.map_err(|_| {
            TransportError::Proxy("HTTP CONNECT response ended before headers completed".into())
        })?;
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let header = std::str::from_utf8(&header)
        .map_err(|_| TransportError::Proxy("HTTP CONNECT response is not UTF-8".into()))?;
    header
        .lines()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| TransportError::Proxy("HTTP CONNECT response has no status line".into()))
}

impl Drop for WorkerTransport {
    fn drop(&mut self) {
        self.bridge.abort();
    }
}

pub struct HubTransport {
    session: Arc<Mutex<SmuxSession>>,
    control: HubControl<SmuxStream>,
    bridge:  JoinHandle<Result<(), TransportError>>,
}

impl HubTransport {
    pub async fn accept(
        socket: AxumWebSocket,
        max_frame_bytes: usize,
        accept_timeout: Duration,
    ) -> Result<Self, TransportError> {
        if max_frame_bytes == 0 {
            return Err(TransportError::InvalidConfig(
                "max_frame_bytes must be greater than zero",
            ));
        }
        if accept_timeout.is_zero() {
            return Err(TransportError::InvalidConfig(
                "accept_timeout must be greater than zero",
            ));
        }
        let (io, bridge) = axum_bridge(socket);
        let mut session = SmuxSession::server(io, SmuxConfig::default());
        let stream = tokio::time::timeout(accept_timeout, session.accept())
            .await
            .map_err(|_| TransportError::ConnectTimeout)?
            .ok_or(TransportError::Closed)?;
        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            control: HubControl::new(stream, max_frame_bytes)?,
            bridge,
        })
    }

    pub fn control_mut(&mut self) -> &mut HubControl<SmuxStream> {
        &mut self.control
    }

    pub async fn open_stream(&self) -> Result<SmuxStream, TransportError> {
        self.session
            .lock()
            .await
            .open_stream()
            .await
            .map_err(Into::into)
    }

    pub fn is_alive(&self) -> bool {
        !self.bridge.is_finished()
    }
}

impl Drop for HubTransport {
    fn drop(&mut self) {
        self.bridge.abort();
    }
}

fn validate_connect_config(config: &WorkerConnectConfig) -> Result<(), TransportError> {
    if config.url.trim().is_empty() {
        return Err(TransportError::InvalidConfig("url must not be empty"));
    }
    if config.token.trim().is_empty() {
        return Err(TransportError::InvalidConfig("token must not be empty"));
    }
    if config.connect_timeout.is_zero() {
        return Err(TransportError::InvalidConfig(
            "connect_timeout must be greater than zero",
        ));
    }
    if config.max_frame_bytes == 0 {
        return Err(TransportError::InvalidConfig(
            "max_frame_bytes must be greater than zero",
        ));
    }
    Ok(())
}

/// Builds the TLS client config for a `wss://` Hub.
///
/// Always supplied rather than letting tungstenite construct its own, for one
/// reason: its default calls `ClientConfig::builder()`, which panics outright
/// when two crypto providers are compiled in and neither is installed as the
/// process default. That is one S3-SDK-shaped dependency away in a workspace
/// like this one, and it would surface as a crash on the first connection. The
/// provider is therefore named here, and the roots this returns are the same
/// public set tungstenite would have used, plus any private CA.
fn tls_connector(config: &WorkerTlsConfig) -> Result<Connector, TransportError> {
    Ok(Connector::Rustls(Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|error| TransportError::Tls(error.to_string()))?
        .with_root_certificates(root_store(&config.extra_root_certificates)?)
        .with_no_client_auth(),
    )))
}

/// The public webpki roots plus every CA found in `bundles`.
///
/// Additive on purpose. A fleet is rarely uniform — one Hub behind a corporate
/// CA and another behind a public certificate is the normal case — so replacing
/// the public set would break the hubs that were working.
fn root_store(bundles: &[Vec<u8>]) -> Result<rustls::RootCertStore, TransportError> {
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    for pem in bundles {
        let mut added = 0usize;
        for certificate in CertificateDer::pem_slice_iter(pem) {
            let certificate =
                certificate.map_err(|error| TransportError::Tls(format!("{error:?}")))?;
            roots
                .add(certificate)
                .map_err(|error| TransportError::Tls(error.to_string()))?;
            added += 1;
        }
        // A file of the wrong kind parses to nothing at all rather than
        // failing. Silently trusting only the public roots would turn a
        // deployment mistake into a handshake failure much later, with nothing
        // pointing back here.
        if added == 0 {
            return Err(TransportError::Tls(
                "a configured CA bundle contains no certificates".into(),
            ));
        }
    }
    Ok(roots)
}

fn tungstenite_bridge<S>(
    socket: tokio_tungstenite::WebSocketStream<S>,
) -> (DuplexStream, JoinHandle<Result<(), TransportError>>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let (smux_io, bridge_io) = tokio::io::duplex(BRIDGE_CAPACITY_BYTES);
    let (mut bridge_reader, mut bridge_writer) = tokio::io::split(bridge_io);
    let (mut sink, mut source) = socket.split();
    let task = tokio::spawn(async move {
        let mut buffer = BytesMut::with_capacity(32 * 1024);
        loop {
            tokio::select! {
                read = bridge_reader.read_buf(&mut buffer) => {
                    let count = read?;
                    if count == 0 {
                        let _ = sink.send(TungsteniteMessage::Close(None)).await;
                        return Ok(());
                    }
                    sink.send(TungsteniteMessage::Binary(buffer.split().freeze()))
                        .await?;
                }
                incoming = source.next() => match incoming {
                    Some(Ok(TungsteniteMessage::Binary(data))) => bridge_writer.write_all(&data).await?,
                    Some(Ok(TungsteniteMessage::Ping(data))) => sink.send(TungsteniteMessage::Pong(data)).await?,
                    Some(Ok(TungsteniteMessage::Pong(_))) => {}
                    Some(Ok(TungsteniteMessage::Close(_))) | None => return Ok(()),
                    Some(Ok(TungsteniteMessage::Text(_))) => return Err(TransportError::UnexpectedTextFrame),
                    Some(Ok(TungsteniteMessage::Frame(_))) => {}
                    Some(Err(error)) => return Err(error.into()),
                }
            }
        }
    });
    (smux_io, task)
}

fn axum_bridge(socket: AxumWebSocket) -> (DuplexStream, JoinHandle<Result<(), TransportError>>) {
    let (smux_io, bridge_io) = tokio::io::duplex(BRIDGE_CAPACITY_BYTES);
    let (mut bridge_reader, mut bridge_writer) = tokio::io::split(bridge_io);
    let (mut sink, mut source) = socket.split();
    let task = tokio::spawn(async move {
        let mut buffer = BytesMut::with_capacity(32 * 1024);
        loop {
            tokio::select! {
                read = bridge_reader.read_buf(&mut buffer) => {
                    let count = read?;
                    if count == 0 {
                        let _ = sink.send(AxumMessage::Close(None)).await;
                        return Ok(());
                    }
                    sink.send(AxumMessage::Binary(buffer.split().freeze())).await?;
                }
                incoming = source.next() => match incoming {
                    Some(Ok(AxumMessage::Binary(data))) => bridge_writer.write_all(&data).await?,
                    Some(Ok(AxumMessage::Ping(data))) => sink.send(AxumMessage::Pong(data)).await?,
                    Some(Ok(AxumMessage::Pong(_))) => {}
                    Some(Ok(AxumMessage::Close(_))) | None => return Ok(()),
                    Some(Ok(AxumMessage::Text(_))) => return Err(TransportError::UnexpectedTextFrame),
                    Some(Err(error)) => return Err(error.into()),
                }
            }
        }
    });
    (smux_io, task)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use nagisalake_protocol::{Ping, Pong};
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
        KeyPair, KeyUsagePurpose,
    };
    use rustls::pki_types::PrivateKeyDer;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};

    struct TestStream(DuplexStream);

    impl TunnelStream for TestStream {
        async fn read(
            &mut self,
            buffer: &mut [u8],
        ) -> Result<usize, tokilake_core::error::TunnelError> {
            Ok(AsyncReadExt::read(&mut self.0, buffer).await?)
        }

        async fn write(
            &mut self,
            buffer: &[u8],
        ) -> Result<usize, tokilake_core::error::TunnelError> {
            Ok(AsyncWriteExt::write(&mut self.0, buffer).await?)
        }

        async fn flush(&mut self) -> Result<(), tokilake_core::error::TunnelError> {
            Ok(AsyncWriteExt::flush(&mut self.0).await?)
        }

        async fn close(&mut self) -> Result<(), tokilake_core::error::TunnelError> {
            Ok(AsyncWriteExt::shutdown(&mut self.0).await?)
        }
    }

    #[tokio::test]
    async fn typed_control_messages_round_trip_over_a_tunnel_stream() {
        let (left, right) = tokio::io::duplex(16 * 1024);
        let mut worker = WorkerControl::new(TestStream(left), 1024).unwrap();
        let mut hub = HubControl::new(TestStream(right), 1024).unwrap();

        worker
            .send(&WorkerMessage::Pong(Pong {
                nonce: "one".into(),
            }))
            .await
            .unwrap();
        assert_eq!(
            hub.receive().await.unwrap(),
            Some(WorkerMessage::Pong(Pong {
                nonce: "one".into(),
            }))
        );

        hub.send(&HubMessage::Ping(Ping {
            nonce: "two".into(),
        }))
        .await
        .unwrap();
        assert_eq!(
            worker.receive().await.unwrap(),
            Some(HubMessage::Ping(Ping {
                nonce: "two".into(),
            }))
        );
    }

    #[test]
    fn only_websocket_schemes_are_accepted() {
        assert_eq!(
            connect_scheme("ws://127.0.0.1:9091/v1/worker/connect").unwrap(),
            ConnectScheme::Plain
        );
        assert_eq!(
            connect_scheme("  wss://hub.example.com/v1/worker/connect  ").unwrap(),
            ConnectScheme::Tls
        );
        // The mistakes worth catching at startup: the scheme of the Hub's web
        // UI, the scheme of its API, and a url with no scheme at all.
        for url in [
            "https://hub.example.com/v1/worker/connect",
            "http://127.0.0.1:9091/v1/worker/connect",
            "hub.example.com/v1/worker/connect",
            "wss//hub.example.com",
        ] {
            assert!(
                matches!(connect_scheme(url), Err(TransportError::UnsupportedUrl(_))),
                "{url} should not be dialable"
            );
        }
    }

    #[test]
    fn a_ca_bundle_that_decodes_to_nothing_is_rejected() {
        // Pointing at the wrong file is the likely mistake, and it decodes to
        // an empty set rather than an error. Accepting it would leave the
        // worker trusting only the public roots and blaming the certificate.
        let Err(error) = tls_connector(&WorkerTlsConfig {
            extra_root_certificates: vec![b"-----BEGIN CERTIFICATE-----\n".to_vec()],
        }) else {
            panic!("a bundle with no certificates must not build a connector");
        };
        assert!(matches!(error, TransportError::Tls(_)), "{error:?}");
    }

    #[test]
    fn a_wss_config_is_built_even_with_no_private_ca() {
        // Not delegating to tungstenite's default is the point: its builder
        // panics when two crypto providers are compiled in, which the Hub's own
        // dependency graph already does.
        assert!(tls_connector(&WorkerTlsConfig::default()).is_ok());
        assert_eq!(
            root_store(&[]).unwrap().len(),
            webpki_roots::TLS_SERVER_ROOTS.len()
        );
    }

    #[test]
    fn a_private_ca_joins_the_public_roots_instead_of_replacing_them() {
        let authority = TestAuthority::new();
        let roots = root_store(&[authority.ca_pem.clone().into_bytes()]).unwrap();
        // A fleet where one Hub uses a private CA and another a public issuer
        // has to keep verifying both.
        assert_eq!(roots.len(), webpki_roots::TLS_SERVER_ROOTS.len() + 1);
        assert!(
            tls_connector(&WorkerTlsConfig {
                extra_root_certificates: vec![authority.ca_pem.into_bytes()],
            })
            .is_ok()
        );
    }

    #[tokio::test]
    async fn a_worker_completes_a_wss_handshake_against_a_private_ca() {
        let authority = TestAuthority::new();
        let hub = authority.serve().await;

        let mut transport = WorkerTransport::connect(WorkerConnectConfig {
            url: format!("wss://localhost:{}/v1/worker/connect", hub.port),
            tls: WorkerTlsConfig {
                extra_root_certificates: vec![authority.ca_pem.clone().into_bytes()],
            },
            ..WorkerConnectConfig::new("wss://replaced", "test-token")
        })
        .await
        .expect("the private CA should verify the hub certificate");

        // Prove the tunnel carries protocol frames, not just that TLS agreed:
        // the smux control stream is open on both sides past the handshake.
        transport
            .control_mut()
            .send(&WorkerMessage::Pong(Pong {
                nonce: "tls".into(),
            }))
            .await
            .unwrap();
        assert_eq!(
            hub.received.await.unwrap(),
            Some(WorkerMessage::Pong(Pong {
                nonce: "tls".into(),
            }))
        );
    }

    #[tokio::test]
    async fn a_wss_handshake_fails_when_the_ca_is_not_trusted() {
        let authority = TestAuthority::new();
        let hub = authority.serve().await;

        // Same server, no extra root: the public store cannot vouch for this
        // certificate, so verification has to fail rather than fall through.
        let Err(error) = WorkerTransport::connect(WorkerConnectConfig {
            url: format!("wss://localhost:{}/v1/worker/connect", hub.port),
            ..WorkerConnectConfig::new("wss://replaced", "test-token")
        })
        .await
        else {
            panic!("an untrusted certificate must not be accepted");
        };
        assert!(
            matches!(&error, TransportError::WebSocket(source)
                if source.to_string().contains("certificate")),
            "expected certificate verification to fail, got {error:?}"
        );
    }

    #[tokio::test]
    async fn extra_roots_on_a_cleartext_url_are_a_configuration_error() {
        let Err(error) = WorkerTransport::connect(WorkerConnectConfig {
            url: "ws://127.0.0.1:9091/v1/worker/connect".into(),
            tls: WorkerTlsConfig {
                extra_root_certificates: vec![TestAuthority::new().ca_pem.into_bytes()],
            },
            ..WorkerConnectConfig::new("ws://replaced", "test-token")
        })
        .await
        else {
            panic!("trust material on a ws:// url secures nothing");
        };
        assert!(
            matches!(error, TransportError::InvalidConfig(_)),
            "{error:?}"
        );
    }

    /// A throwaway CA and the `localhost` server certificate it issued.
    ///
    /// Generated per test rather than committed: no private key lives in the
    /// repository and there is nothing to expire and break the suite later.
    struct TestAuthority {
        ca_pem: String,
        server: Arc<rustls::ServerConfig>,
    }

    /// A one-shot TLS + WebSocket + smux listener standing in for the Hub.
    struct HubStub {
        port:     u16,
        received: tokio::task::JoinHandle<Option<WorkerMessage>>,
    }

    impl TestAuthority {
        fn new() -> Self {
            let ca_key = KeyPair::generate().unwrap();
            let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
            ca_params
                .distinguished_name
                .push(DnType::CommonName, "nagisalake test ca");
            // `CA:TRUE` plus `keyCertSign` is what makes this a usable trust
            // anchor. Without them webpki rejects the chain it signs, which is
            // the trap a self-signed server certificate falls into.
            ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
            ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
            let ca = ca_params.self_signed(&ca_key).unwrap();
            let ca_pem = ca.pem();
            let issuer = Issuer::new(ca_params, ca_key);

            let server_key = KeyPair::generate().unwrap();
            let mut server_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
            server_params
                .distinguished_name
                .push(DnType::CommonName, "localhost");
            server_params.use_authority_key_identifier_extension = true;
            server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
            let server = server_params.signed_by(&server_key, &issuer).unwrap();

            // Same explicit provider as the client, for the same reason: under
            // Match the provider used by the S3 and HTTP clients explicitly.
            let config = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::aws_lc_rs::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![server.der().clone()],
                PrivateKeyDer::Pkcs8(server_key.serialize_der().into()),
            )
            .unwrap();
            Self {
                ca_pem,
                server: Arc::new(config),
            }
        }

        async fn serve(&self) -> HubStub {
            // Port 0 so parallel tests never collide, and loopback so nothing
            // is reachable off the machine. The worker still dials `localhost`
            // — the name the certificate is issued for.
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let acceptor = tokio_rustls::TlsAcceptor::from(self.server.clone());
            let received = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.ok()?;
                let tls = acceptor.accept(socket).await.ok()?;
                let websocket = tokio_tungstenite::accept_hdr_async(tls, negotiate_subprotocol)
                    .await
                    .ok()?;
                let (io, _bridge) = tungstenite_bridge(websocket);
                let mut session = SmuxSession::server(io, SmuxConfig::default());
                let stream = session.accept().await?;
                let mut control = HubControl::new(stream, DEFAULT_MAX_CONTROL_FRAME_BYTES).ok()?;
                control.receive().await.ok()?
            });
            HubStub { port, received }
        }
    }

    /// Echoes the Tokilake subprotocol back, as the real Hub handler does.
    ///
    /// The worker drops any connection whose subprotocol was not confirmed, so a
    /// stub that skipped this would fail for the wrong reason.
    // The `Err` type is tungstenite's `ErrorResponse`, fixed by the `Callback`
    // trait, so there is nothing here to make smaller.
    #[allow(clippy::result_large_err)]
    fn negotiate_subprotocol(
        _request: &Request,
        mut response: Response,
    ) -> Result<Response, ErrorResponse> {
        response.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            TOKILAKE_SUBPROTOCOL.parse().unwrap(),
        );
        Ok(response)
    }
}
