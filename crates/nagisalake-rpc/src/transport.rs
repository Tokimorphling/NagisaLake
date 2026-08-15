//! Transport makers and their products.
//!
//! A transport is presented as an owned read half and write half rather than one
//! duplex object. The reader and writer run as independent tasks, and splitting
//! at the source lets a TCP connection hand out two lock-free halves instead of
//! sharing one object behind `tokio::io::split`'s mutex.

use std::{future::Future, io, net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{
        TcpListener, TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    time::timeout,
};

/// A transport split into the halves the connection tasks own.
pub trait Transport: Send + 'static {
    /// Owned read half.
    type Read: AsyncRead + Unpin + Send + 'static;
    /// Owned write half.
    type Write: AsyncWrite + Unpin + Send + 'static;

    /// Splits the transport for the reader and writer tasks.
    fn split(self) -> (Self::Read, Self::Write);
}

impl Transport for TcpStream {
    type Read = OwnedReadHalf;
    type Write = OwnedWriteHalf;

    fn split(self) -> (Self::Read, Self::Write) {
        self.into_split()
    }
}

/// A duplex stream, split with `tokio::io::split`.
///
/// This covers in-process transports and TLS streams, which cannot hand out
/// independent halves. It costs one lock per I/O operation, so prefer a native
/// split where the transport offers one.
pub struct SplitDuplex<IO>(pub IO);

impl<IO> Transport for SplitDuplex<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Read = tokio::io::ReadHalf<IO>;
    type Write = tokio::io::WriteHalf<IO>;

    fn split(self) -> (Self::Read, Self::Write) {
        tokio::io::split(self.0)
    }
}

/// An accepted transport and its peer metadata.
#[derive(Debug)]
pub struct Accepted<T> {
    /// The accepted transport.
    pub transport: T,
    /// Peer socket address, when the listener provides one.
    pub peer_addr: Option<SocketAddr>,
}

/// Creates one listener-like incoming connection source.
pub trait MakeIncoming {
    /// Live incoming source.
    type Incoming: Incoming;

    /// Consumes listener configuration and binds the live source.
    fn make_incoming(self) -> impl Future<Output = io::Result<Self::Incoming>> + Send;
}

/// Accepts connections from a bound listener.
///
/// `accept` takes `&mut self` because it advances listener state, and the accept
/// loop is a single task, so this trait needs `Send` but not `Sync`.
pub trait Incoming: Send + 'static {
    /// Accepted transport type.
    type Transport: Transport;

    /// Accepts the next connection, or returns `None` when the source closes.
    fn accept(
        &mut self,
    ) -> impl Future<Output = io::Result<Option<Accepted<Self::Transport>>>> + Send;
}

/// Repeatedly creates client transports from shared immutable configuration.
///
/// `&self` because one maker serves every reconnect.
pub trait MakeTransport: Clone + Send + Sync + 'static {
    /// Connected transport type.
    type Transport: Transport;

    /// Creates one new connection.
    fn make_transport(&self) -> impl Future<Output = io::Result<Self::Transport>> + Send;
}

/// Tokio TCP client transport maker.
#[derive(Clone, Copy, Debug)]
pub struct TcpConnector {
    addr:            SocketAddr,
    connect_timeout: Option<Duration>,
    nodelay:         bool,
}

impl TcpConnector {
    /// Creates a connector for `addr` with `TCP_NODELAY` enabled.
    ///
    /// Nagle batching adds up to a round trip of latency to a small request that
    /// is already framed and flushed, so it is off by default.
    pub const fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            connect_timeout: None,
            nodelay: true,
        }
    }

    /// Applies a connect timeout.
    pub const fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Enables or disables `TCP_NODELAY`.
    pub const fn with_nodelay(mut self, nodelay: bool) -> Self {
        self.nodelay = nodelay;
        self
    }

    /// Returns the target address.
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl MakeTransport for TcpConnector {
    type Transport = TcpStream;

    async fn make_transport(&self) -> io::Result<Self::Transport> {
        let stream = match self.connect_timeout {
            Some(limit) => timeout(limit, TcpStream::connect(self.addr))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "RPC connect timed out"))??,
            None => TcpStream::connect(self.addr).await?,
        };
        stream.set_nodelay(self.nodelay)?;
        Ok(stream)
    }
}

/// Tokio TCP listener maker.
#[derive(Clone, Copy, Debug)]
pub struct TcpIncomingMaker {
    addr:    SocketAddr,
    nodelay: bool,
}

impl TcpIncomingMaker {
    /// Creates a listener maker with `TCP_NODELAY` enabled for accepted streams.
    pub const fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            nodelay: true,
        }
    }

    /// Enables or disables `TCP_NODELAY` on accepted streams.
    pub const fn with_nodelay(mut self, nodelay: bool) -> Self {
        self.nodelay = nodelay;
        self
    }
}

impl MakeIncoming for TcpIncomingMaker {
    type Incoming = TcpIncoming;

    async fn make_incoming(self) -> io::Result<Self::Incoming> {
        let listener = TcpListener::bind(self.addr).await?;
        Ok(TcpIncoming {
            listener,
            nodelay: self.nodelay,
        })
    }
}

/// A bound Tokio TCP listener.
#[derive(Debug)]
pub struct TcpIncoming {
    listener: TcpListener,
    nodelay:  bool,
}

impl TcpIncoming {
    /// Wraps an already-bound listener.
    pub const fn new(listener: TcpListener) -> Self {
        Self {
            listener,
            nodelay: true,
        }
    }

    /// Enables or disables `TCP_NODELAY` on accepted streams.
    pub const fn with_nodelay(mut self, nodelay: bool) -> Self {
        self.nodelay = nodelay;
        self
    }

    /// Returns the bound local address.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

impl MakeIncoming for TcpIncoming {
    type Incoming = Self;

    async fn make_incoming(self) -> io::Result<Self::Incoming> {
        Ok(self)
    }
}

impl Incoming for TcpIncoming {
    type Transport = TcpStream;

    async fn accept(&mut self) -> io::Result<Option<Accepted<Self::Transport>>> {
        loop {
            match self.listener.accept().await {
                Ok((stream, peer_addr)) => {
                    stream.set_nodelay(self.nodelay)?;
                    return Ok(Some(Accepted {
                        transport: stream,
                        peer_addr: Some(peer_addr),
                    }));
                }
                // A connection that dies between the SYN and our accept must not
                // take down the listener.
                Err(error) if is_transient_accept_error(&error) => {
                    tracing::debug!(%error, "RPC accept failed for one connection");
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn is_transient_accept_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::Interrupted
            | io::ErrorKind::TimedOut
    )
}
