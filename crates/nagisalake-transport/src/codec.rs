//! Typed newline-delimited control framing over a generic TunnelStream.
use crate::TransportError;
use bytes::BytesMut;
use nagisalake_protocol::{HubMessage, Validate, WorkerMessage};
use serde::{Serialize, de::DeserializeOwned};
use tokilake_core::tunnel::TunnelStream;

/// Bytes pulled from the tunnel per read syscall.
const READ_CHUNK_BYTES: usize = 8 * 1024;

#[cfg(test)]
mod tests;

pub struct JsonLineCodec<S> {
    stream:          S,
    read_buffer:     BytesMut,
    /// How much of `read_buffer` has already been searched for a newline.
    ///
    /// Without this the delimiter scan restarts at offset 0 after every read,
    /// so accumulating one frame over N reads costs O(N^2) bytes scanned. A
    /// 1 MiB `DispatchJob` arriving in 8 KiB reads scanned ~65 MB to find one
    /// byte.
    scan_offset:     usize,
    /// Reused across reads so the per-frame path does not allocate and zero a
    /// fresh staging buffer on every syscall.
    read_chunk:      Box<[u8]>,
    /// Reused serialization buffer, so sending a control frame does not
    /// allocate a new `Vec` per message.
    write_buffer:    Vec<u8>,
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
            read_buffer: BytesMut::with_capacity(max_frame_bytes.min(READ_CHUNK_BYTES)),
            scan_offset: 0,
            read_chunk: vec![0u8; max_frame_bytes.clamp(1, READ_CHUNK_BYTES)].into_boxed_slice(),
            write_buffer: Vec::with_capacity(4 * 1024),
            max_frame_bytes,
        })
    }

    pub async fn send<T: Serialize>(&mut self, message: &T) -> Result<(), TransportError> {
        self.write_buffer.clear();
        serde_json::to_writer(&mut self.write_buffer, message)?;
        if self.write_buffer.len() > self.max_frame_bytes {
            let actual = self.write_buffer.len();
            // A rejected oversized frame must not leave its capacity attached to
            // this long-lived connection.
            self.write_buffer = Vec::with_capacity(4 * 1024);
            return Err(TransportError::FrameTooLarge {
                actual,
                limit: self.max_frame_bytes,
            });
        }
        self.write_buffer.push(b'\n');
        let mut written = 0;
        while written < self.write_buffer.len() {
            let count = self.stream.write(&self.write_buffer[written..]).await?;
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
            // Resume the delimiter search where the previous read left off.
            // Everything before `scan_offset` has already been shown to hold no
            // newline.
            let found = self.read_buffer[self.scan_offset..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|offset| self.scan_offset + offset);
            if let Some(position) = found {
                if position > self.max_frame_bytes {
                    return Err(TransportError::FrameTooLarge {
                        actual: position,
                        limit:  self.max_frame_bytes,
                    });
                }
                let mut frame = self.read_buffer.split_to(position + 1);
                // Bytes after the delimiter were never examined, so the next
                // frame starts from an unscanned buffer.
                self.scan_offset = 0;
                frame.truncate(position);
                if frame.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                return Ok(Some(serde_json::from_slice(&frame)?));
            }
            self.scan_offset = self.read_buffer.len();
            if self.read_buffer.len() > self.max_frame_bytes {
                return Err(TransportError::FrameTooLarge {
                    actual: self.read_buffer.len(),
                    limit:  self.max_frame_bytes,
                });
            }
            let count = self.stream.read(&mut self.read_chunk).await?;
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
                self.scan_offset = 0;
                return Ok(Some(serde_json::from_slice(&frame)?));
            }
            self.read_buffer
                .extend_from_slice(&self.read_chunk[..count]);
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
