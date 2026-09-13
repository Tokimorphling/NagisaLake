//! Worker connection, HTTP proxy CONNECT and SMUX lifecycle.
use crate::{
    ConnectScheme, DEFAULT_MAX_CONTROL_FRAME_BYTES, TOKILAKE_SUBPROTOCOL, TransportError,
    WorkerControl, WorkerTlsConfig, bridge::bridge, connect_scheme, tls::tls_connector,
};
use std::{sync::Arc, time::Duration};
use tokilake_core::tunnel::TunnelSession;
use tokilake_smux::{Config as SmuxConfig, Session as SmuxSession, Stream as SmuxStream};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Mutex,
    task::JoinHandle,
};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, client_async_tls_with_config,
    connect_async_tls_with_config,
    tungstenite::{
        client::IntoClientRequest,
        handshake::client::{Request, Response},
        http::{
            Uri,
            header::{AUTHORIZATION, SEC_WEBSOCKET_PROTOCOL, USER_AGENT},
        },
    },
};

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
        let (io, bridge) = bridge(socket);
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
