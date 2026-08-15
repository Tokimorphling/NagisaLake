//! Message codecs.
//!
//! A codec turns one application message into [`Bytes`] and back. Framing never
//! sees these types: the connection tasks move opaque bodies, and this trait is
//! invoked by the task that owns the call. Encoding a request therefore happens
//! on the caller's task and decoding a request on the handler's task, so codec
//! cost scales with the runtime's worker threads instead of queueing behind one
//! connection task.

use bytes::{BufMut, Bytes, BytesMut};
use faststr::FastStr;
use serde::{Serialize, de::DeserializeOwned};
use std::{fmt, marker::PhantomData};
use thiserror::Error;

/// A local serialization or deserialization failure.
///
/// The message is an [`FastStr`] because this error is cloned into every pending
/// call when a connection fails, and short reasons stay inline.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CodecError {
    /// A message could not be serialized.
    #[error("encode failed: {0}")]
    Encode(FastStr),
    /// A message could not be deserialized.
    #[error("decode failed: {0}")]
    Decode(FastStr),
}

impl CodecError {
    /// Creates an encode failure.
    pub fn encode(detail: impl fmt::Display) -> Self {
        Self::Encode(FastStr::from_string(detail.to_string()))
    }

    /// Creates a decode failure.
    pub fn decode(detail: impl fmt::Display) -> Self {
        Self::Decode(FastStr::from_string(detail.to_string()))
    }
}

/// Encodes and decodes the message pair of one RPC direction.
///
/// One trait owns both halves so a client and server cannot be built from a
/// mismatched encoder and decoder. `Out` is what this side sends and `In` is
/// what it receives, so the client uses `Codec<Req, Resp>` and the server uses
/// the same codec as `Codec<Resp, Req>`.
pub trait Codec<Out, In>: Send + Sync + 'static {
    /// Serializes an outbound message.
    ///
    /// `dst` is a reusable buffer; the returned [`Bytes`] must contain only this
    /// message.
    fn encode(&self, message: &Out, dst: &mut BytesMut) -> Result<Bytes, CodecError>;

    /// Deserializes an inbound message.
    fn decode(&self, src: Bytes) -> Result<In, CodecError>;
}

/// Bincode 2 codec with fixed-width little-endian integers.
///
/// Fixed-width integers cost a few bytes on small values but avoid the varint
/// branch per field, which is the better trade for in-process service links.
pub struct BincodeCodec<Out, In>(PhantomData<fn(&Out) -> In>);

impl<Out, In> BincodeCodec<Out, In> {
    /// Creates the codec.
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<Out, In> Default for BincodeCodec<Out, In> {
    fn default() -> Self {
        Self::new()
    }
}

// Hand-written: the marker carries no data, so deriving these would demand
// `Out: Clone` and `Out: Debug` from callers for no reason.
impl<Out, In> Clone for BincodeCodec<Out, In> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Out, In> Copy for BincodeCodec<Out, In> {}

impl<Out, In> fmt::Debug for BincodeCodec<Out, In> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BincodeCodec")
    }
}

fn bincode_config() -> impl bincode::config::Config {
    bincode::config::standard()
        .with_fixed_int_encoding()
        .with_little_endian()
}

impl<Out, In> Codec<Out, In> for BincodeCodec<Out, In>
where
    Out: Serialize + Send + Sync + 'static,
    In: DeserializeOwned + Send + Sync + 'static,
{
    fn encode(&self, message: &Out, dst: &mut BytesMut) -> Result<Bytes, CodecError> {
        dst.clear();
        let mut writer = dst.writer();
        bincode::serde::encode_into_std_write(message, &mut writer, bincode_config())
            .map_err(CodecError::encode)?;
        Ok(dst.split().freeze())
    }

    fn decode(&self, src: Bytes) -> Result<In, CodecError> {
        let (message, consumed) = bincode::serde::decode_from_slice(&src, bincode_config())
            .map_err(CodecError::decode)?;
        if consumed != src.len() {
            return Err(CodecError::Decode(FastStr::from_static_str(
                "trailing bytes after the decoded message",
            )));
        }
        Ok(message)
    }
}
