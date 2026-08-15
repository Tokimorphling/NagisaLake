use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::time::Instant;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// A distributed trace identifier propagated across RPC calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TraceId([u8; 16]);

impl TraceId {
    /// Creates a trace identifier from its wire representation.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Returns the wire representation.
    pub const fn into_bytes(self) -> [u8; 16] {
        self.0
    }
}

/// Client-side call metadata.
#[derive(Clone, Copy, Debug)]
pub struct ClientContext {
    deadline: Instant,
    trace_id: TraceId,
}

impl Default for ClientContext {
    fn default() -> Self {
        Self::with_timeout(DEFAULT_TIMEOUT)
    }
}

impl ClientContext {
    /// Creates a context whose deadline is `timeout` from now.
    pub fn with_timeout(timeout: Duration) -> Self {
        let now = Instant::now();
        Self {
            deadline: now
                .checked_add(timeout)
                .expect("RPC timeout is too large for Tokio Instant"),
            trace_id: TraceId::default(),
        }
    }

    /// Creates a context with an absolute Tokio deadline.
    pub const fn with_deadline(deadline: Instant) -> Self {
        Self {
            deadline,
            trace_id: TraceId([0; 16]),
        }
    }

    /// Sets the distributed trace identifier.
    pub const fn with_trace_id(mut self, trace_id: TraceId) -> Self {
        self.trace_id = trace_id;
        self
    }

    /// Returns the call deadline.
    pub const fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Returns the distributed trace identifier.
    pub const fn trace_id(&self) -> TraceId {
        self.trace_id
    }

    /// Returns the remaining budget in microseconds, saturating at zero.
    ///
    /// Deadlines cross the wire as a relative budget so the two peers do not
    /// need synchronized clocks.
    pub(crate) fn remaining_micros(&self) -> u64 {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return 0;
        }
        remaining.as_micros().min(u64::MAX as u128) as u64
    }
}

/// The authenticated identity of an RPC caller.
///
/// This is injected by the transport or a server-side layer (mTLS, HMAC
/// handshake, loopback trust) and must never originate from the request
/// payload itself. Services read `cx.principal()` instead of trusting a
/// `user_id` field supplied by the client.
#[derive(Clone, Debug)]
pub struct Principal {
    inner: Arc<PrincipalInner>,
}

#[derive(Debug)]
struct PrincipalInner {
    user_id:         String,
    organization_id: Option<String>,
}

impl Principal {
    /// Creates a principal for `user_id`, optionally scoped to `organization_id`.
    pub fn new(user_id: impl Into<String>, organization_id: Option<String>) -> Self {
        Self {
            inner: Arc::new(PrincipalInner {
                user_id: user_id.into(),
                organization_id,
            }),
        }
    }

    /// The authenticated user id. Services use this instead of a client-supplied field.
    pub fn user_id(&self) -> &str {
        &self.inner.user_id
    }

    /// The organization the caller acts in, when the transport could bind one.
    pub fn organization_id(&self) -> Option<&str> {
        self.inner.organization_id.as_deref()
    }
}

/// Mutable server-side metadata for one request.
#[derive(Debug)]
pub struct ServerContext {
    request_id: u64,
    deadline:   Instant,
    trace_id:   TraceId,
    peer_addr:  Option<SocketAddr>,
    principal:  Option<Principal>,
}

impl ServerContext {
    pub(crate) fn new(
        request_id: u64,
        timeout_micros: u64,
        trace_id: TraceId,
        peer_addr: Option<SocketAddr>,
    ) -> Self {
        let now = Instant::now();
        Self {
            request_id,
            deadline: now
                .checked_add(Duration::from_micros(timeout_micros))
                .expect("wire timeout is validated before context construction"),
            trace_id,
            peer_addr,
            principal: None,
        }
    }

    /// Attaches an authenticated [`Principal`] to this context.
    ///
    /// Intended for a server-side layer that has verified the caller out of
    /// band (mTLS client cert, HMAC token, loopback trust). Application code
    /// must not derive a principal from the request payload.
    pub fn set_principal(&mut self, principal: Principal) {
        self.principal = Some(principal);
    }

    /// Returns the connection-local request identifier.
    pub const fn request_id(&self) -> u64 {
        self.request_id
    }

    /// Returns the deadline enforced by the RPC runtime.
    pub const fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Returns the distributed trace identifier.
    pub const fn trace_id(&self) -> TraceId {
        self.trace_id
    }

    /// Returns the peer address when the incoming transport provides one.
    pub const fn peer_addr(&self) -> Option<SocketAddr> {
        self.peer_addr
    }

    /// Returns the authenticated principal, if a server layer attached one.
    pub fn principal(&self) -> Option<&Principal> {
        self.principal.as_ref()
    }

    /// Creates a child client context that propagates this request's deadline
    /// and trace identifier.
    pub const fn child_context(&self) -> ClientContext {
        ClientContext {
            deadline: self.deadline,
            trace_id: self.trace_id,
        }
    }
}
