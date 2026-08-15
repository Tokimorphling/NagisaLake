//! Fixed-layout wire headers.
//!
//! Every frame is `[u32 payload_len BE][payload]`. The payload starts with a
//! one-byte kind tag followed by a fixed header, so a connection task can route
//! a frame without deserializing the application message. Message bodies stay
//! opaque [`Bytes`] until they reach the task that owns the call, which is what
//! keeps codec work off the shared read and write tasks.
//!
//! ```text
//! Request  0x01 | id u64 | timeout_micros u64 | trace_id 16B | body
//! Cancel   0x02 | id u64
//! Response 0x03 | id u64 | status_code u8 | body
//! ```
//!
//! Integers are little-endian: every target that runs this code is
//! little-endian, so encoding and decoding are plain loads and stores.

use crate::{Code, TraceId};
use bytes::{BufMut, Bytes, BytesMut};

const KIND_REQUEST: u8 = 0x01;
const KIND_CANCEL: u8 = 0x02;
const KIND_RESPONSE: u8 = 0x03;

/// Status byte reserved for a successful response.
const STATUS_OK: u8 = 0;

/// Header length of a request frame, excluding the body.
pub(crate) const REQUEST_HEADER_LEN: usize = 1 + 8 + 8 + 16;
/// Total length of a cancel frame.
pub(crate) const CANCEL_FRAME_LEN: usize = 1 + 8;
/// Header length of a response frame, excluding the body.
pub(crate) const RESPONSE_HEADER_LEN: usize = 1 + 8 + 1;

/// Largest header this protocol emits, used to size write reservations.
pub(crate) const MAX_HEADER_LEN: usize = REQUEST_HEADER_LEN;

/// Overhead a response body pays on the wire.
pub(crate) const RESPONSE_OVERHEAD: usize = RESPONSE_HEADER_LEN;

/// A frame sent from an RPC client to a server.
#[derive(Clone, Debug)]
pub enum ClientFrame {
    /// Starts a request.
    Request {
        /// Connection-local request identifier.
        id:             u64,
        /// Remaining deadline budget when the frame was encoded.
        timeout_micros: u64,
        /// Distributed trace identifier.
        trace_id:       TraceId,
        /// Encoded application request.
        body:           Bytes,
    },
    /// Cancels an in-flight request.
    Cancel {
        /// Connection-local request identifier.
        id: u64,
    },
}

/// A frame sent from an RPC server to a client.
#[derive(Clone, Debug)]
pub enum ServerFrame {
    /// Completes an in-flight request.
    Response {
        /// Connection-local request identifier.
        id:   u64,
        /// `None` for an application response, otherwise the failure code.
        code: Option<Code>,
        /// Encoded application response, or encoded status message.
        body: Bytes,
    },
}

/// A frame that can be written to the wire.
///
/// Framing only needs the header bytes and the already-encoded body, so both
/// directions share one writer implementation.
pub(crate) trait Frame {
    /// Length of this frame's payload, including its header.
    fn payload_len(&self) -> usize;

    /// Appends the header to `dst` and returns the body to write after it.
    fn split_header(&self, dst: &mut BytesMut) -> Bytes;
}

impl Frame for ClientFrame {
    fn payload_len(&self) -> usize {
        match self {
            Self::Request { body, .. } => REQUEST_HEADER_LEN + body.len(),
            Self::Cancel { .. } => CANCEL_FRAME_LEN,
        }
    }

    fn split_header(&self, dst: &mut BytesMut) -> Bytes {
        match self {
            Self::Request {
                id,
                timeout_micros,
                trace_id,
                body,
            } => {
                dst.put_u8(KIND_REQUEST);
                dst.put_u64_le(*id);
                dst.put_u64_le(*timeout_micros);
                dst.put_slice(&trace_id.into_bytes());
                body.clone()
            }
            Self::Cancel { id } => {
                dst.put_u8(KIND_CANCEL);
                dst.put_u64_le(*id);
                Bytes::new()
            }
        }
    }
}

impl Frame for ServerFrame {
    fn payload_len(&self) -> usize {
        match self {
            Self::Response { body, .. } => RESPONSE_HEADER_LEN + body.len(),
        }
    }

    fn split_header(&self, dst: &mut BytesMut) -> Bytes {
        match self {
            Self::Response { id, code, body } => {
                dst.put_u8(KIND_RESPONSE);
                dst.put_u64_le(*id);
                dst.put_u8(code.map_or(STATUS_OK, Code::as_wire));
                body.clone()
            }
        }
    }
}

/// A frame payload that failed to parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameError {
    /// The payload was shorter than the header its tag requires.
    Truncated,
    /// The leading tag byte is not part of this protocol version.
    UnknownKind(u8),
    /// The status byte is not a known [`Code`].
    UnknownStatus(u8),
}

impl FrameError {
    pub(crate) const fn detail(self) -> &'static str {
        match self {
            Self::Truncated => "frame payload is shorter than its header",
            Self::UnknownKind(_) => "frame has an unknown kind tag",
            Self::UnknownStatus(_) => "response frame has an unknown status code",
        }
    }
}

/// A frame payload that can be parsed from one wire payload.
pub(crate) trait ParseFrame: Sized {
    /// Parses one complete payload, keeping the body as a slice of `payload`.
    fn parse(payload: Bytes) -> Result<Self, FrameError>;
}

impl ParseFrame for ClientFrame {
    fn parse(mut payload: Bytes) -> Result<Self, FrameError> {
        let kind = *payload.first().ok_or(FrameError::Truncated)?;
        match kind {
            KIND_REQUEST => {
                if payload.len() < REQUEST_HEADER_LEN {
                    return Err(FrameError::Truncated);
                }
                let header = payload.split_to(REQUEST_HEADER_LEN);
                Ok(Self::Request {
                    id:             read_u64(&header[1..9]),
                    timeout_micros: read_u64(&header[9..17]),
                    trace_id:       read_trace_id(&header[17..33]),
                    body:           payload,
                })
            }
            KIND_CANCEL => {
                if payload.len() < CANCEL_FRAME_LEN {
                    return Err(FrameError::Truncated);
                }
                Ok(Self::Cancel {
                    id: read_u64(&payload[1..9]),
                })
            }
            other => Err(FrameError::UnknownKind(other)),
        }
    }
}

impl ParseFrame for ServerFrame {
    fn parse(mut payload: Bytes) -> Result<Self, FrameError> {
        let kind = *payload.first().ok_or(FrameError::Truncated)?;
        match kind {
            KIND_RESPONSE => {
                if payload.len() < RESPONSE_HEADER_LEN {
                    return Err(FrameError::Truncated);
                }
                let header = payload.split_to(RESPONSE_HEADER_LEN);
                let status = header[9];
                let code = if status == STATUS_OK {
                    None
                } else {
                    Some(Code::from_wire(status).ok_or(FrameError::UnknownStatus(status))?)
                };
                Ok(Self::Response {
                    id: read_u64(&header[1..9]),
                    code,
                    body: payload,
                })
            }
            other => Err(FrameError::UnknownKind(other)),
        }
    }
}

fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("caller checked the header length"))
}

fn read_trace_id(bytes: &[u8]) -> TraceId {
    TraceId::from_bytes(bytes.try_into().expect("caller checked the header length"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    /// Round-trips one frame through the writer's header layout and the reader's
    /// parser, which is the only pairing that has to agree.
    fn round_trip<T: Frame + ParseFrame>(frame: &T) -> T {
        let mut buffer = BytesMut::new();
        let body = frame.split_header(&mut buffer);
        buffer.extend_from_slice(&body);
        assert_eq!(buffer.len(), frame.payload_len(), "payload_len disagrees");
        T::parse(buffer.freeze()).expect("a frame this crate wrote must parse")
    }

    #[test]
    fn request_frame_round_trips() {
        let frame = ClientFrame::Request {
            id:             7,
            timeout_micros: 1_234_567,
            trace_id:       TraceId::from_bytes([9; 16]),
            body:           Bytes::from_static(b"payload"),
        };
        let ClientFrame::Request {
            id,
            timeout_micros,
            trace_id,
            body,
        } = round_trip(&frame)
        else {
            panic!("kind changed");
        };
        assert_eq!(id, 7);
        assert_eq!(timeout_micros, 1_234_567);
        assert_eq!(trace_id, TraceId::from_bytes([9; 16]));
        assert_eq!(body, Bytes::from_static(b"payload"));
    }

    #[test]
    fn cancel_frame_round_trips() {
        let ClientFrame::Cancel { id } = round_trip(&ClientFrame::Cancel { id: u64::MAX }) else {
            panic!("kind changed");
        };
        assert_eq!(id, u64::MAX);
    }

    #[test]
    fn response_frame_carries_success_and_failure() {
        let ok = ServerFrame::Response {
            id:   1,
            code: None,
            body: Bytes::from_static(b"ok"),
        };
        let ServerFrame::Response { id, code, body } = round_trip(&ok);
        assert_eq!(id, 1);
        assert_eq!(code, None, "a success must not carry a status code");
        assert_eq!(body, Bytes::from_static(b"ok"));

        let failed = ServerFrame::Response {
            id:   2,
            code: Some(Code::Unavailable),
            body: Bytes::from_static(b"try later"),
        };
        let ServerFrame::Response { id, code, body } = round_trip(&failed);
        assert_eq!(id, 2);
        assert_eq!(code, Some(Code::Unavailable));
        assert_eq!(body, Bytes::from_static(b"try later"));
    }

    #[test]
    fn empty_payload_is_rejected() {
        assert_eq!(
            ClientFrame::parse(Bytes::new()).unwrap_err(),
            FrameError::Truncated
        );
    }

    #[test]
    fn truncated_header_is_rejected() {
        let mut buffer = BytesMut::new();
        ClientFrame::Request {
            id:             1,
            timeout_micros: 1,
            trace_id:       TraceId::default(),
            body:           Bytes::new(),
        }
        .split_header(&mut buffer);
        buffer.truncate(REQUEST_HEADER_LEN - 1);
        assert_eq!(
            ClientFrame::parse(buffer.freeze()).unwrap_err(),
            FrameError::Truncated
        );
    }

    #[test]
    fn unknown_kind_is_rejected() {
        assert_eq!(
            ClientFrame::parse(Bytes::from_static(&[0xFF])).unwrap_err(),
            FrameError::UnknownKind(0xFF)
        );
        // A server frame is not valid in the client-to-server direction.
        assert_eq!(
            ClientFrame::parse(Bytes::from_static(&[KIND_RESPONSE])).unwrap_err(),
            FrameError::UnknownKind(KIND_RESPONSE)
        );
    }

    #[test]
    fn unknown_status_is_rejected() {
        // Forward compatibility has a limit: a code this build cannot name must
        // not be silently reported as some other code.
        let mut payload = vec![KIND_RESPONSE];
        payload.extend_from_slice(&5u64.to_le_bytes());
        payload.push(200);
        assert_eq!(
            ServerFrame::parse(Bytes::from(payload)).unwrap_err(),
            FrameError::UnknownStatus(200)
        );
    }

    #[test]
    fn status_codes_round_trip_through_the_wire_byte() {
        for code in [
            Code::Cancelled,
            Code::Unknown,
            Code::InvalidArgument,
            Code::DeadlineExceeded,
            Code::NotFound,
            Code::AlreadyExists,
            Code::PermissionDenied,
            Code::ResourceExhausted,
            Code::FailedPrecondition,
            Code::Unimplemented,
            Code::Internal,
            Code::Unavailable,
            Code::Unauthenticated,
        ] {
            assert_ne!(code.as_wire(), 0, "zero is reserved for success");
            assert_eq!(Code::from_wire(code.as_wire()), Some(code));
        }
        assert_eq!(Code::from_wire(0), None);
    }
}
