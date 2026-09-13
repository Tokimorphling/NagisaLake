//! Full-duplex WebSocket/SMUX byte bridge shared by Axum and tungstenite.
use crate::TransportError;
use axum::extract::ws::Message as AxumMessage;
use bytes::{Bytes, BytesMut};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    sync::mpsc,
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;

const BRIDGE_CAPACITY_BYTES: usize = 1024 * 1024;
const BRIDGE_READ_CHUNK_BYTES: usize = 32 * 1024;

pub(super) enum Incoming {
    Binary(Bytes),
    Ping(Bytes),
    Ignore,
    Close,
    Text,
}

/// Static message conversion, not a boxed transport or runtime codec lookup.
pub(super) trait BridgeMessage: Send + 'static {
    fn binary(bytes: Bytes) -> Self;
    fn pong(bytes: Bytes) -> Self;
    fn close() -> Self;
    fn incoming(self) -> Incoming;
}

impl BridgeMessage for TungsteniteMessage {
    fn binary(bytes: Bytes) -> Self {
        Self::Binary(bytes)
    }
    fn pong(bytes: Bytes) -> Self {
        Self::Pong(bytes)
    }
    fn close() -> Self {
        Self::Close(None)
    }
    fn incoming(self) -> Incoming {
        match self {
            Self::Binary(data) => Incoming::Binary(data),
            Self::Ping(data) => Incoming::Ping(data),
            Self::Close(_) => Incoming::Close,
            Self::Text(_) => Incoming::Text,
            Self::Pong(_) | Self::Frame(_) => Incoming::Ignore,
        }
    }
}
impl BridgeMessage for AxumMessage {
    fn binary(bytes: Bytes) -> Self {
        Self::Binary(bytes)
    }
    fn pong(bytes: Bytes) -> Self {
        Self::Pong(bytes)
    }
    fn close() -> Self {
        Self::Close(None)
    }
    fn incoming(self) -> Incoming {
        match self {
            Self::Binary(data) => Incoming::Binary(data),
            Self::Ping(data) => Incoming::Ping(data),
            Self::Close(_) => Incoming::Close,
            Self::Text(_) => Incoming::Text,
            Self::Pong(_) => Incoming::Ignore,
        }
    }
}

pub(super) fn bridge<S, M, E>(socket: S) -> (DuplexStream, JoinHandle<Result<(), TransportError>>)
where
    S: Stream<Item = Result<M, E>> + Sink<M, Error = E> + Send + Unpin + 'static,
    M: BridgeMessage,
    E: Into<TransportError> + Send + 'static,
{
    let (smux_io, bridge_io) = tokio::io::duplex(BRIDGE_CAPACITY_BYTES);
    let (mut reader, mut writer) = tokio::io::split(bridge_io);
    let (mut sink, mut source) = socket.split();
    let task = tokio::spawn(async move {
        let (pongs, mut pong_rx) = mpsc::channel::<Bytes>(16);
        let outgoing = async move {
            let mut buffer = BytesMut::with_capacity(BRIDGE_READ_CHUNK_BYTES);
            loop {
                // split() transfers the filled allocation; replenish the tail
                // before each read instead of shrinking to tiny growth steps.
                buffer.reserve(BRIDGE_READ_CHUNK_BYTES);
                let message = tokio::select! {
                    read = reader.read_buf(&mut buffer) => {
                        if read? == 0 {
                            let _ = sink.send(M::close()).await;
                            return Ok::<(), TransportError>(());
                        }
                        M::binary(buffer.split().freeze())
                    }
                    pong = pong_rx.recv() => match pong {
                        Some(bytes) => M::pong(bytes),
                        None => return Ok(()),
                    }
                };
                sink.send(message).await.map_err(Into::into)?;
            }
        };
        let incoming = async move {
            while let Some(message) = source.next().await {
                match message.map_err(Into::into)?.incoming() {
                    Incoming::Binary(data) => writer.write_all(&data).await?,
                    Incoming::Ping(data) => {
                        if pongs.send(data).await.is_err() {
                            break;
                        }
                    }
                    Incoming::Ignore => {}
                    Incoming::Close => break,
                    Incoming::Text => return Err(TransportError::UnexpectedTextFrame),
                }
            }
            Ok::<(), TransportError>(())
        };
        // Poll complete direction loops concurrently. The old per-frame select
        // awaited send/write_all *inside* a winning branch, stopping both
        // directions whenever either peer or the local SMUX buffer was slow.
        tokio::select! { result = outgoing => result, result = incoming => result }
    });
    (smux_io, task)
}

#[cfg(test)]
mod tests;
