//! Length-prefixed frame transport.
//!
//! Both directions share one reader and one writer. Neither touches application
//! types: bodies are opaque [`Bytes`], so these tasks only move memory and never
//! run a codec.
//!
//! The reader keeps one [`BytesMut`] and parses every complete frame already in
//! it before issuing another read, so a batch of small frames costs one syscall
//! instead of two per frame. Frame bodies are `split_to` slices of that buffer,
//! which makes the handoff to the dispatcher a refcount bump rather than a copy.
//!
//! The writer coalesces queued frames into one buffer and writes it in a single
//! call. Bodies at or above `max_batch_bytes` are written directly from their own
//! `Bytes` instead, so a large payload is not copied into the batch buffer first.

use crate::{
    ConnectionError, FrameConfig,
    protocol::{Frame, MAX_HEADER_LEN, ParseFrame},
};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc,
};

const PREFIX_BYTES: usize = size_of::<u32>();

/// Why a read loop stopped.
///
/// The dispatcher needs to tell a clean shutdown from a failure, and the reader
/// is a separate task, so the reason travels with the stream of frames.
pub(crate) enum ReadEvent<T> {
    /// One parsed frame.
    Frame(T),
    /// The peer closed the connection at a frame boundary.
    Closed,
    /// The connection failed.
    Failed(ConnectionError),
}

/// Reads length-prefixed frames until the stream ends or fails.
pub(crate) async fn read_frames<R, T>(
    mut io: R,
    config: FrameConfig,
    events: mpsc::Sender<ReadEvent<T>>,
) where
    R: AsyncRead + Unpin,
    T: ParseFrame,
{
    let mut buffer = BytesMut::with_capacity(config.initial_buffer_bytes);
    loop {
        // Drain everything already buffered before touching the socket again.
        loop {
            match take_frame(&mut buffer, config.max_frame_bytes) {
                Ok(Some(payload)) => match T::parse(payload) {
                    Ok(frame) => {
                        if events.send(ReadEvent::Frame(frame)).await.is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = events
                            .send(ReadEvent::Failed(ConnectionError::protocol(error.detail())))
                            .await;
                        return;
                    }
                },
                Ok(None) => break,
                Err(error) => {
                    let _ = events.send(ReadEvent::Failed(error)).await;
                    return;
                }
            }
        }

        // `read_buf` appends into spare capacity, so no zeroing is needed. One
        // read can deliver many frames, which the loop above then drains.
        buffer.reserve(config.read_chunk_bytes);
        match io.read_buf(&mut buffer).await {
            Ok(0) => {
                let event = if buffer.is_empty() {
                    ReadEvent::Closed
                } else {
                    ReadEvent::Failed(ConnectionError::closed("stream ended inside a frame"))
                };
                let _ = events.send(event).await;
                return;
            }
            Ok(_) => {}
            Err(error) => {
                let _ = events
                    .send(ReadEvent::Failed(ConnectionError::io(error)))
                    .await;
                return;
            }
        }
    }
}

/// Splits off the next complete frame payload, if the buffer holds one.
fn take_frame(
    buffer: &mut BytesMut,
    max_frame_bytes: usize,
) -> Result<Option<Bytes>, ConnectionError> {
    if buffer.len() < PREFIX_BYTES {
        return Ok(None);
    }
    let payload_len = u32::from_be_bytes(
        buffer[..PREFIX_BYTES]
            .try_into()
            .expect("slice is exactly four bytes"),
    ) as usize;
    if payload_len > max_frame_bytes {
        return Err(ConnectionError::frame_too_large(
            payload_len,
            max_frame_bytes,
        ));
    }
    if buffer.len() < PREFIX_BYTES + payload_len {
        // Reserve the rest of this frame now so the next read can finish it in
        // one call rather than growing the buffer repeatedly.
        buffer.reserve(PREFIX_BYTES + payload_len - buffer.len());
        return Ok(None);
    }
    buffer.advance(PREFIX_BYTES);
    Ok(Some(buffer.split_to(payload_len).freeze()))
}

/// Writes frames until the queue closes, coalescing whatever is already queued.
pub(crate) async fn write_frames<W, T>(
    mut io: W,
    config: FrameConfig,
    mut frames: mpsc::Receiver<T>,
) -> Result<(), ConnectionError>
where
    W: AsyncWrite + Unpin,
    T: Frame,
{
    let mut batch = BytesMut::with_capacity(config.initial_buffer_bytes);
    // Tracks how much this connection actually writes per batch. Reclaiming
    // against a fixed threshold instead would reallocate on every batch for any
    // load whose steady state sits near that threshold.
    let mut high_water = config.initial_buffer_bytes;

    while let Some(first) = frames.recv().await {
        let mut pending = Some(first);
        let mut frame_count = 0;

        while let Some(frame) = pending.take() {
            let payload_len = frame.payload_len();
            if payload_len > config.max_frame_bytes {
                return Err(ConnectionError::frame_too_large(
                    payload_len,
                    config.max_frame_bytes,
                ));
            }

            batch.reserve(PREFIX_BYTES + MAX_HEADER_LEN);
            batch.put_u32(payload_len as u32);
            let body = frame.split_header(&mut batch);

            // A large body is written from its own allocation. Copying it into
            // the batch would double the memory traffic for the payloads where
            // that traffic actually matters.
            if body.len() >= config.max_batch_bytes {
                io.write_all(&batch).await.map_err(ConnectionError::io)?;
                batch.clear();
                io.write_all(&body).await.map_err(ConnectionError::io)?;
            } else {
                batch.put_slice(&body);
            }

            frame_count += 1;
            if frame_count >= config.max_batch_frames || batch.len() >= config.max_batch_bytes {
                break;
            }
            match frames.try_recv() {
                Ok(next) => pending = Some(next),
                Err(_) => break,
            }
        }

        let written = batch.len();
        if written > 0 {
            io.write_all(&batch).await.map_err(ConnectionError::io)?;
            batch.clear();
        }
        io.flush().await.map_err(ConnectionError::io)?;

        // Decay towards recent usage so a burst's footprint fades rather than
        // being held for the life of the connection.
        high_water = written
            .max(high_water - high_water / 8)
            .max(config.initial_buffer_bytes);
        if batch.capacity() > high_water.saturating_mul(4) {
            // Keep headroom for the next batch of this size, so shrinking does
            // not just trade one large allocation for repeated growth.
            batch = BytesMut::with_capacity(high_water.saturating_mul(2));
        }
    }
    io.shutdown().await.map_err(ConnectionError::io)
}
