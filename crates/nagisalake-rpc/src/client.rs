//! Multiplexed RPC client.
//!
//! ## Task layout
//!
//! ```text
//! Client::call  --requests-->  dispatch  --frames-->  writer task
//!      ^                          |                      |
//!      +------ oneshot -----------+   reader task -------+
//! ```
//!
//! `Client::call` encodes the request on the caller's task, so codec cost is
//! spread across the runtime's workers instead of queueing behind the connection.
//! The dispatcher only moves already-encoded bodies and owns the in-flight table.
//!
//! ## Type erasure
//!
//! The codec is boxed inside [`Client`]. That keeps the handle a two-parameter
//! type that callers can name, store, and return without carrying the transport
//! and codec generics. It is one virtual call per encode and decode, off the
//! per-byte path. Everything else, including the server's layer stack, stays
//! statically dispatched.

use crate::{
    BincodeCodec, ClientContext, Codec, ConfigError, ConnectError, ConnectionError, FrameConfig,
    MakeTransport, RpcError, Status, Transport,
    framing::{ReadEvent, read_frames, write_frames},
    protocol::{ClientFrame, REQUEST_HEADER_LEN, ServerFrame},
};
use bytes::BytesMut;
use futures_util::StreamExt;
use hashbrown::HashMap;
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::time::{DelayQueue, delay_queue};

/// Client connection settings.
#[derive(Clone, Copy, Debug)]
pub struct ClientConfig {
    /// Encoded calls waiting to enter the dispatcher.
    pub pending_request_buffer: usize,
    /// Maximum calls awaiting responses on one connection.
    pub max_in_flight_requests: usize,
    /// Frames waiting for the writer task.
    pub outbound_frame_buffer:  usize,
    /// Frames waiting for the dispatcher.
    pub inbound_frame_buffer:   usize,
    /// Wire framing and batching settings.
    pub frame:                  FrameConfig,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            pending_request_buffer: 256,
            max_in_flight_requests: 1_024,
            outbound_frame_buffer:  256,
            inbound_frame_buffer:   256,
            frame:                  FrameConfig::default(),
        }
    }
}

impl ClientConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.pending_request_buffer == 0 {
            return Err(ConfigError::Zero("pending_request_buffer"));
        }
        if self.max_in_flight_requests == 0 {
            return Err(ConfigError::Zero("max_in_flight_requests"));
        }
        if self.outbound_frame_buffer == 0 {
            return Err(ConfigError::Zero("outbound_frame_buffer"));
        }
        if self.inbound_frame_buffer == 0 {
            return Err(ConfigError::Zero("inbound_frame_buffer"));
        }
        self.frame.validate()
    }
}

/// Builder state meaning no transport has been chosen yet.
#[derive(Clone, Copy, Debug, Default)]
#[doc(hidden)]
pub struct MissingTransport;

/// Type-state builder for a [`Client`].
///
/// Replacing the transport or codec changes the builder type, because it changes
/// what the finished client can connect to and speak. Value-only settings live in
/// [`ClientConfig`] and return the same type. The compatibility bounds are on
/// [`ClientBuilder::connect`] and [`ClientBuilder::connect_transport`].
pub struct ClientBuilder<Req, Resp, MkT = MissingTransport, C = BincodeCodec<Req, Resp>> {
    make_transport: MkT,
    codec:          C,
    config:         ClientConfig,
    marker:         std::marker::PhantomData<fn(Req) -> Resp>,
}

impl<Req, Resp> ClientBuilder<Req, Resp> {
    /// Creates a builder using the default Bincode codec.
    pub const fn new() -> Self {
        Self {
            make_transport: MissingTransport,
            codec:          BincodeCodec::new(),
            config:         ClientConfig::default_const(),
            marker:         std::marker::PhantomData,
        }
    }
}

impl<Req, Resp> Default for ClientBuilder<Req, Resp> {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientConfig {
    /// `Default` in const form, so `ClientBuilder::new` can be `const`.
    const fn default_const() -> Self {
        Self {
            pending_request_buffer: 256,
            max_in_flight_requests: 1_024,
            outbound_frame_buffer:  256,
            inbound_frame_buffer:   256,
            frame:                  FrameConfig {
                max_frame_bytes:      8 * 1024 * 1024,
                initial_buffer_bytes: 16 * 1024,
                read_chunk_bytes:     32 * 1024,
                max_batch_frames:     64,
                max_batch_bytes:      256 * 1024,
            },
        }
    }
}

impl<Req, Resp, MkT, C> ClientBuilder<Req, Resp, MkT, C> {
    /// Replaces the transport maker.
    pub fn transport<T>(self, make_transport: T) -> ClientBuilder<Req, Resp, T, C> {
        ClientBuilder {
            make_transport,
            codec: self.codec,
            config: self.config,
            marker: std::marker::PhantomData,
        }
    }

    /// Replaces the codec.
    pub fn codec<NewCodec>(self, codec: NewCodec) -> ClientBuilder<Req, Resp, MkT, NewCodec> {
        ClientBuilder {
            make_transport: self.make_transport,
            codec,
            config: self.config,
            marker: std::marker::PhantomData,
        }
    }

    /// Replaces value-only settings.
    pub const fn config(mut self, config: ClientConfig) -> Self {
        self.config = config;
        self
    }
}

impl<Req, Resp, MkT, C> ClientBuilder<Req, Resp, MkT, C>
where
    Req: Send + 'static,
    Resp: Send + 'static,
    C: Codec<Req, Resp>,
{
    /// Starts a client over an already-connected transport.
    pub fn connect_transport<T>(self, transport: T) -> Result<Client<Req, Resp>, ConfigError>
    where
        T: Transport,
    {
        self.config.validate()?;
        Ok(spawn_client(transport, self.codec, self.config))
    }
}

impl<Req, Resp, MkT, C> ClientBuilder<Req, Resp, MkT, C>
where
    Req: Send + 'static,
    Resp: Send + 'static,
    MkT: MakeTransport,
    C: Codec<Req, Resp>,
{
    /// Creates the configured transport and starts the client.
    pub async fn connect(self) -> Result<Client<Req, Resp>, ConnectError> {
        self.config.validate()?;
        let transport = self.make_transport.make_transport().await?;
        Ok(spawn_client(transport, self.codec, self.config))
    }
}

/// A cheap, cloneable handle to one multiplexed connection.
///
/// Cloning shares the connection. Dropping every clone shuts the connection down
/// once the dispatcher observes the closed queue.
pub struct Client<Req, Resp> {
    shared:  Arc<ClientShared<Req, Resp>>,
    next_id: Arc<AtomicU64>,
}

struct ClientShared<Req, Resp> {
    requests:       mpsc::Sender<DispatchRequest>,
    cancellations:  mpsc::UnboundedSender<u64>,
    codec:          Box<dyn Codec<Req, Resp>>,
    /// Largest encoded request the frame limit leaves room for.
    max_body_bytes: usize,
}

impl<Req, Resp> Clone for Client<Req, Resp> {
    fn clone(&self) -> Self {
        Self {
            shared:  self.shared.clone(),
            next_id: self.next_id.clone(),
        }
    }
}

impl<Req, Resp> fmt::Debug for Client<Req, Resp> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl<Req, Resp> Client<Req, Resp>
where
    Req: 'static,
    Resp: 'static,
{
    /// Sends one request and waits for its response.
    ///
    /// The request is encoded on this task. Dropping the returned future removes
    /// the call from the dispatcher and sends a best-effort cancellation, so a
    /// server handler stops running when its caller walks away.
    pub async fn call(&self, context: ClientContext, request: Req) -> Result<Resp, RpcError> {
        // A fresh buffer per call: `encode` splits its output, so a shared buffer
        // would need a lock on the hot path to save one allocation.
        let mut scratch = BytesMut::with_capacity(1024);
        let body = self.shared.codec.encode(&request, &mut scratch)?;
        // Checked here so an oversized request fails this call only, instead of
        // reaching the writer and ending the connection for everyone.
        if body.len() > self.shared.max_body_bytes {
            return Err(RpcError::RequestTooLarge {
                bytes: body.len(),
                limit: self.shared.max_body_bytes,
            });
        }

        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (response_tx, mut response_rx) = oneshot::channel();

        // Built before the send: if this future is dropped between the send and
        // the guard's construction, nothing would ever cancel the request.
        let guard = ResponseGuard {
            response: &mut response_rx,
            cancellations: &self.shared.cancellations,
            request_id,
            cancel: true,
        };

        self.shared
            .requests
            .send(DispatchRequest {
                context,
                request_id,
                body,
                response: response_tx,
            })
            .await
            .map_err(|_| RpcError::Shutdown)?;

        match guard.wait().await? {
            Ok(body) => self.shared.codec.decode(body).map_err(RpcError::Codec),
            Err(status) => Err(RpcError::Remote(status)),
        }
    }
}

/// What the dispatcher needs to track one call.
///
/// Not generic over the message types: by this point the request is encoded and
/// the response is still raw, which is exactly why the dispatcher never has to
/// run a codec.
struct DispatchRequest {
    context:    ClientContext,
    request_id: u64,
    body:       bytes::Bytes,
    response:   oneshot::Sender<ResponseSlot>,
}

/// The dispatcher returns raw bodies; decoding happens on the caller's task.
type ResponseSlot = Result<Result<bytes::Bytes, Status>, RpcError>;

/// Cancels a call if its future is dropped before the response arrives.
struct ResponseGuard<'a> {
    response:      &'a mut oneshot::Receiver<ResponseSlot>,
    cancellations: &'a mpsc::UnboundedSender<u64>,
    request_id:    u64,
    cancel:        bool,
}

impl ResponseGuard<'_> {
    async fn wait(mut self) -> ResponseSlot {
        let result = (&mut self.response).await;
        // A response arrived, so `Drop` must not send a cancellation for a
        // request the peer already finished.
        self.cancel = false;
        result.unwrap_or(Err(RpcError::Shutdown))
    }
}

impl Drop for ResponseGuard<'_> {
    fn drop(&mut self) {
        // Closing first is what makes the cancellation race safe: a cancellation
        // can reach the dispatcher before the request it refers to, and the
        // dispatcher checks `is_closed` before tracking a request. Closing here
        // guarantees that check sees the abandonment.
        self.response.close();
        if self.cancel {
            let _ = self.cancellations.send(self.request_id);
        }
    }
}

struct Pending {
    response:     oneshot::Sender<ResponseSlot>,
    deadline_key: delay_queue::Key,
}

fn spawn_client<Req, Resp, T, C>(transport: T, codec: C, config: ClientConfig) -> Client<Req, Resp>
where
    Req: Send + 'static,
    Resp: Send + 'static,
    T: Transport,
    C: Codec<Req, Resp>,
{
    let (requests_tx, requests_rx) = mpsc::channel(config.pending_request_buffer);
    // Unbounded because cancellations are sent from `Drop`, which cannot await.
    // The queue is still bounded in practice by the in-flight limit.
    let (cancellations_tx, cancellations_rx) = mpsc::unbounded_channel();

    tokio::spawn(run_client(transport, config, requests_rx, cancellations_rx));

    Client {
        shared:  Arc::new(ClientShared {
            requests:       requests_tx,
            cancellations:  cancellations_tx,
            codec:          Box::new(codec),
            max_body_bytes: config
                .frame
                .max_frame_bytes
                .saturating_sub(REQUEST_HEADER_LEN),
        }),
        // Starts at 1 so that 0 never appears on the wire and can be used as a
        // sentinel by tooling reading a capture.
        next_id: Arc::new(AtomicU64::new(1)),
    }
}

async fn run_client<T>(
    transport: T,
    config: ClientConfig,
    requests: mpsc::Receiver<DispatchRequest>,
    cancellations: mpsc::UnboundedReceiver<u64>,
) where
    T: Transport,
{
    let (read_half, write_half) = transport.split();
    let (incoming_tx, incoming_rx) = mpsc::channel(config.inbound_frame_buffer);
    let reader = tokio::spawn(read_frames::<_, ServerFrame>(
        read_half,
        config.frame,
        incoming_tx,
    ));
    let (outgoing_tx, outgoing_rx) = mpsc::channel(config.outbound_frame_buffer);
    let mut writer = tokio::spawn(write_frames::<_, ClientFrame>(
        write_half,
        config.frame,
        outgoing_rx,
    ));

    let result = dispatch(
        config,
        requests,
        cancellations,
        incoming_rx,
        outgoing_tx,
        &mut writer,
    )
    .await;

    reader.abort();
    if !writer.is_finished() {
        writer.abort();
    }
    if let Err(error) = result {
        tracing::debug!(%error, "RPC client connection stopped");
    }
}

async fn dispatch(
    config: ClientConfig,
    mut requests: mpsc::Receiver<DispatchRequest>,
    mut cancellations: mpsc::UnboundedReceiver<u64>,
    mut incoming: mpsc::Receiver<ReadEvent<ServerFrame>>,
    outgoing: mpsc::Sender<ClientFrame>,
    writer: &mut JoinHandle<Result<(), ConnectionError>>,
) -> Result<(), ConnectionError> {
    let capacity = config.max_in_flight_requests.min(1_024);
    let mut accepting = true;
    let mut in_flight = HashMap::<u64, Pending>::with_capacity(capacity);
    let mut deadlines = DelayQueue::with_capacity(capacity);

    let terminal_error = loop {
        if !accepting && in_flight.is_empty() {
            break None;
        }

        tokio::select! {
            // Stop pulling new calls at the in-flight limit. Responses and
            // deadlines are what free capacity, and both are polled below, so no
            // separate wakeup is needed.
            request = requests.recv(), if accepting && in_flight.len() < config.max_in_flight_requests => {
                let Some(request) = request else {
                    accepting = false;
                    continue;
                };
                if request.response.is_closed() {
                    continue;
                }
                let timeout_micros = request.context.remaining_micros();
                if timeout_micros == 0 {
                    let _ = request.response.send(Err(RpcError::DeadlineExceeded));
                    continue;
                }
                let deadline_key = deadlines.insert_at(request.request_id, request.context.deadline());
                in_flight.insert(request.request_id, Pending {
                    response: request.response,
                    deadline_key,
                });
                let frame = ClientFrame::Request {
                    id: request.request_id,
                    timeout_micros,
                    trace_id: request.context.trace_id(),
                    body: request.body,
                };
                if outgoing.send(frame).await.is_err() {
                    break Some(ConnectionError::closed("RPC writer stopped"));
                }
            }
            cancellation = cancellations.recv() => {
                let Some(request_id) = cancellation else {
                    continue;
                };
                if let Some(pending) = in_flight.remove(&request_id) {
                    deadlines.remove(&pending.deadline_key);
                    if outgoing.send(ClientFrame::Cancel { id: request_id }).await.is_err() {
                        break Some(ConnectionError::closed("RPC writer stopped"));
                    }
                }
            }
            event = incoming.recv() => {
                match event {
                    Some(ReadEvent::Frame(ServerFrame::Response { id, code, body })) => {
                        if let Some(pending) = in_flight.remove(&id) {
                            deadlines.remove(&pending.deadline_key);
                            let outcome = match code {
                                None => Ok(body),
                                Some(code) => Err(Status::from_wire(code, body)),
                            };
                            let _ = pending.response.send(Ok(outcome));
                        }
                    }
                    Some(ReadEvent::Closed) | None => {
                        break Some(ConnectionError::closed("RPC peer closed the connection"));
                    }
                    Some(ReadEvent::Failed(error)) => break Some(error),
                }
            }
            // The guard prevents a busy loop: an empty `DelayQueue` returns
            // `None` immediately, and `select!` re-checks the condition each
            // iteration, so a new deadline re-enables this branch.
            expired = deadlines.next(), if !deadlines.is_empty() => {
                if let Some(expired) = expired {
                    let request_id = expired.into_inner();
                    if let Some(pending) = in_flight.remove(&request_id) {
                        let _ = pending.response.send(Err(RpcError::DeadlineExceeded));
                        // The server enforces the same deadline, but it may be
                        // mid-handler; the cancel stops that work now.
                        if outgoing.send(ClientFrame::Cancel { id: request_id }).await.is_err() {
                            break Some(ConnectionError::closed("RPC writer stopped"));
                        }
                    }
                }
            }
            result = &mut *writer => {
                break Some(match result {
                    Ok(Ok(())) => ConnectionError::closed("RPC writer stopped"),
                    Ok(Err(error)) => error,
                    Err(error) => ConnectionError::runtime(format!("RPC writer task failed: {error}")),
                });
            }
        }
    };

    drop(outgoing);
    if let Some(error) = terminal_error {
        // Fail every call that can no longer complete, including those still
        // queued, so no caller waits on a dead connection.
        let rpc_error = RpcError::Connection(error.clone());
        for (_, pending) in in_flight.drain() {
            let _ = pending.response.send(Err(rpc_error.clone()));
        }
        requests.close();
        while let Ok(request) = requests.try_recv() {
            let _ = request.response.send(Err(rpc_error.clone()));
        }
        return Err(error);
    }
    Ok(())
}
