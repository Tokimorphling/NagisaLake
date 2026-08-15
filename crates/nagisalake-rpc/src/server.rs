//! Multiplexed RPC server.
//!
//! ## Request flow
//!
//! ```text
//! reader task -> connection supervisor -> handler task
//!                                          | decode request  (this task)
//!                                          | outermost layer
//!                                          | ...
//!                                          | innermost layer
//!                                          | application service
//!                                          | encode response (this task)
//!                                          v
//!                                        writer task
//! ```
//!
//! The supervisor only routes fixed headers and owns admission control. Decoding,
//! the layer stack, the handler, and encoding all run on the spawned handler task,
//! so codec and application work use every runtime worker rather than serializing
//! behind one connection task.
//!
//! The service stack is statically dispatched: there is no boxing between layers.

use crate::{
    BincodeCodec, Codec, ConfigError, ConnectionError, FrameConfig, Incoming, Layer, MakeIncoming,
    ServeError, ServerContext, Service, Status, Transport,
    framing::{ReadEvent, read_frames, write_frames},
    protocol::{ClientFrame, RESPONSE_OVERHEAD, ServerFrame},
};
use bytes::BytesMut;
use hashbrown::HashMap;
use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    sync::{Semaphore, mpsc, watch},
    task::{AbortHandle, JoinHandle, JoinSet},
    time::timeout_at,
};

/// Upper bound on a client-supplied deadline, so a bad budget cannot pin a
/// request slot for the life of the process.
const MAX_SUPPORTED_REQUEST_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// Server connection, admission, and framing settings.
#[derive(Clone, Copy, Debug)]
pub struct ServerConfig {
    /// Maximum concurrently open accepted connections.
    pub max_connections: usize,
    /// Maximum concurrently executing requests on one connection.
    pub max_in_flight_requests_per_connection: usize,
    /// Largest deadline budget accepted from a client.
    pub max_request_timeout: Duration,
    /// How long in-flight requests may finish after shutdown begins.
    ///
    /// `Duration::ZERO` cancels them immediately.
    pub shutdown_grace: Duration,
    /// Frames waiting for the connection supervisor.
    pub inbound_frame_buffer: usize,
    /// Frames waiting for the writer task.
    pub outbound_frame_buffer: usize,
    /// Wire framing and batching settings.
    pub frame: FrameConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_connections: 1_024,
            max_in_flight_requests_per_connection: 1_024,
            max_request_timeout: Duration::from_secs(5 * 60),
            shutdown_grace: Duration::from_secs(10),
            inbound_frame_buffer: 256,
            outbound_frame_buffer: 256,
            frame: FrameConfig::default(),
        }
    }
}

impl ServerConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.max_connections == 0 {
            return Err(ConfigError::Zero("max_connections"));
        }
        if self.max_in_flight_requests_per_connection == 0 {
            return Err(ConfigError::Zero("max_in_flight_requests_per_connection"));
        }
        if self.max_request_timeout.is_zero()
            || self.max_request_timeout > MAX_SUPPORTED_REQUEST_TIMEOUT
        {
            return Err(ConfigError::InvalidDuration("max_request_timeout"));
        }
        if self.inbound_frame_buffer == 0 {
            return Err(ConfigError::Zero("inbound_frame_buffer"));
        }
        if self.outbound_frame_buffer == 0 {
            return Err(ConfigError::Zero("outbound_frame_buffer"));
        }
        self.frame.validate()
    }
}

/// A layered RPC service and its runtime.
pub struct Server<S, C> {
    service: S,
    codec:   C,
    config:  ServerConfig,
}

impl<S, Req, Resp> Server<S, BincodeCodec<Resp, Req>> {
    /// Creates a server for a service, using the default Bincode codec.
    ///
    /// The codec's parameters are ordered from the server's point of view: it
    /// sends `Resp` and receives `Req`.
    pub const fn new(service: S) -> Self {
        Self {
            service,
            codec: BincodeCodec::new(),
            config: ServerConfig::default_const(),
        }
    }
}

impl ServerConfig {
    /// `Default` in const form, so `Server::new` can be `const`.
    const fn default_const() -> Self {
        Self {
            max_connections: 1_024,
            max_in_flight_requests_per_connection: 1_024,
            max_request_timeout: Duration::from_secs(5 * 60),
            shutdown_grace: Duration::from_secs(10),
            inbound_frame_buffer: 256,
            outbound_frame_buffer: 256,
            frame: FrameConfig {
                max_frame_bytes:      8 * 1024 * 1024,
                initial_buffer_bytes: 16 * 1024,
                read_chunk_bytes:     32 * 1024,
                max_batch_frames:     64,
                max_batch_bytes:      256 * 1024,
            },
        }
    }
}

impl<S, C> Server<S, C> {
    /// Replaces value-only settings.
    pub const fn config(mut self, config: ServerConfig) -> Self {
        self.config = config;
        self
    }

    /// Replaces the codec.
    pub fn codec<NewCodec>(self, codec: NewCodec) -> Server<S, NewCodec> {
        Server {
            service: self.service,
            codec,
            config: self.config,
        }
    }

    /// Wraps the current service with one layer.
    ///
    /// The most recently added layer is outermost, so it sees a request first and
    /// a response last: `latest -> earlier -> service`.
    pub fn layer<L>(self, layer: L) -> Server<L::Service, C>
    where
        L: Layer<S>,
    {
        Server {
            service: layer.layer(self.service),
            codec:   self.codec,
            config:  self.config,
        }
    }

    /// Serves one already-connected transport until either side closes.
    pub async fn serve_transport<Req, Resp, T>(self, transport: T) -> Result<(), ServeError>
    where
        Req: Send + 'static,
        Resp: Send + 'static,
        T: Transport,
        S: Service<ServerContext, Req, Response = Resp> + Send + Sync + 'static,
        S::Error: Into<Status> + Send + 'static,
        C: Codec<Resp, Req>,
    {
        self.config.validate()?;
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        run_connection(
            transport,
            None,
            Arc::new(self.service),
            Arc::new(self.codec),
            self.config,
            shutdown_rx,
        )
        .await?;
        Ok(())
    }

    /// Accepts and serves connections until the incoming source closes.
    pub async fn serve<Req, Resp, MI>(self, make_incoming: MI) -> Result<(), ServeError>
    where
        Req: Send + 'static,
        Resp: Send + 'static,
        S: Service<ServerContext, Req, Response = Resp> + Send + Sync + 'static,
        S::Error: Into<Status> + Send + 'static,
        C: Codec<Resp, Req>,
        MI: MakeIncoming,
    {
        self.serve_with_shutdown::<Req, Resp, MI, _>(make_incoming, std::future::pending())
            .await
    }

    /// Accepts connections until `shutdown` completes, then drains.
    ///
    /// After `shutdown` resolves the listener stops accepting, connections stop
    /// admitting new requests, and in-flight requests have
    /// [`ServerConfig::shutdown_grace`] to finish before they are cancelled.
    pub async fn serve_with_shutdown<Req, Resp, MI, F>(
        self,
        make_incoming: MI,
        shutdown: F,
    ) -> Result<(), ServeError>
    where
        Req: Send + 'static,
        Resp: Send + 'static,
        S: Service<ServerContext, Req, Response = Resp> + Send + Sync + 'static,
        S::Error: Into<Status> + Send + 'static,
        C: Codec<Resp, Req>,
        MI: MakeIncoming,
        F: Future<Output = ()> + Send,
    {
        self.config.validate()?;
        let mut incoming = make_incoming.make_incoming().await?;
        let service = Arc::new(self.service);
        let codec = Arc::new(self.codec);
        let connections = Arc::new(Semaphore::new(self.config.max_connections));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut tasks = JoinSet::new();
        tokio::pin!(shutdown);

        loop {
            while let Some(result) = tasks.try_join_next() {
                log_connection_result(result);
            }

            let permit = tokio::select! {
                _ = &mut shutdown => break,
                permit = connections.clone().acquire_owned() => {
                    permit.expect("the connection semaphore is never closed")
                }
            };
            let accepted = tokio::select! {
                _ = &mut shutdown => break,
                accepted = incoming.accept() => accepted?,
            };
            let Some(accepted) = accepted else {
                break;
            };

            let service = service.clone();
            let codec = codec.clone();
            let config = self.config;
            let shutdown_rx = shutdown_rx.clone();
            tasks.spawn(async move {
                let _permit = permit;
                run_connection(
                    accepted.transport,
                    accepted.peer_addr,
                    service,
                    codec,
                    config,
                    shutdown_rx,
                )
                .await
            });
        }

        // Tell every connection to drain. Each one enforces the grace period
        // itself, so slow connections cannot hold back the others.
        let _ = shutdown_tx.send(true);
        drop(shutdown_tx);
        while let Some(result) = tasks.join_next().await {
            log_connection_result(result);
        }
        Ok(())
    }
}

fn log_connection_result(result: Result<Result<(), ConnectionError>, tokio::task::JoinError>) {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::debug!(%error, "RPC server connection stopped"),
        Err(error) if error.is_cancelled() => {}
        Err(error) => tracing::warn!(%error, "RPC server connection task failed"),
    }
}

async fn run_connection<Req, Resp, T, S, C>(
    transport: T,
    peer_addr: Option<SocketAddr>,
    service: Arc<S>,
    codec: Arc<C>,
    config: ServerConfig,
    shutdown: watch::Receiver<bool>,
) -> Result<(), ConnectionError>
where
    Req: Send + 'static,
    Resp: Send + 'static,
    T: Transport,
    S: Service<ServerContext, Req, Response = Resp> + Send + Sync + 'static,
    S::Error: Into<Status> + Send + 'static,
    C: Codec<Resp, Req>,
{
    let (read_half, write_half) = transport.split();
    let (incoming_tx, incoming_rx) = mpsc::channel(config.inbound_frame_buffer);
    let reader = tokio::spawn(read_frames::<_, ClientFrame>(
        read_half,
        config.frame,
        incoming_tx,
    ));
    let (outgoing_tx, outgoing_rx) = mpsc::channel(config.outbound_frame_buffer);
    let mut writer = tokio::spawn(write_frames::<_, ServerFrame>(
        write_half,
        config.frame,
        outgoing_rx,
    ));

    let result = supervise_connection(
        peer_addr,
        service,
        codec,
        config,
        incoming_rx,
        outgoing_tx,
        &mut writer,
        shutdown,
    )
    .await;

    reader.abort();
    if result.is_ok() {
        // `supervise_connection` drops the last sender after draining request
        // handlers. Let the writer consume that queue before closing the
        // transport, otherwise a response can be lost between `send` and
        // `write_all`.
        match writer.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::debug!(%error, "RPC writer stopped while closing connection")
            }
            Err(error) => {
                tracing::debug!(%error, "RPC writer task failed while closing connection")
            }
        }
    } else if !writer.is_finished() {
        writer.abort();
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn supervise_connection<Req, Resp, S, C>(
    peer_addr: Option<SocketAddr>,
    service: Arc<S>,
    codec: Arc<C>,
    config: ServerConfig,
    mut incoming: mpsc::Receiver<ReadEvent<ClientFrame>>,
    outgoing: mpsc::Sender<ServerFrame>,
    writer: &mut JoinHandle<Result<(), ConnectionError>>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), ConnectionError>
where
    Req: Send + 'static,
    Resp: Send + 'static,
    S: Service<ServerContext, Req, Response = Resp> + Send + Sync + 'static,
    S::Error: Into<Status> + Send + 'static,
    C: Codec<Resp, Req>,
{
    // Handlers report their own id on completion, which keeps the abort table in
    // step with the running tasks without a side channel.
    let mut handlers = JoinSet::<u64>::new();
    let mut aborts = HashMap::<u64, AbortHandle>::with_capacity(
        config.max_in_flight_requests_per_connection.min(1_024),
    );
    let max_response_bytes = config
        .frame
        .max_frame_bytes
        .saturating_sub(RESPONSE_OVERHEAD);

    // Leaving this loop begins the drain: the grace period is enforced in one
    // place, by `drain_handlers`, so no exit path can silently wait forever for
    // an in-flight request.
    let terminal_error = loop {
        if *shutdown.borrow() {
            break None;
        }

        tokio::select! {
            biased;

            // Reap finished handlers first so a saturated connection frees slots
            // before it considers admitting more work.
            joined = handlers.join_next(), if !handlers.is_empty() => {
                match joined {
                    Some(Ok(request_id)) => {
                        aborts.remove(&request_id);
                    }
                    Some(Err(error)) => {
                        // A panicking handler must not take the connection down,
                        // but its slot has to be released. The id is unknown here,
                        // so drop entries whose task is gone.
                        if !error.is_cancelled() {
                            tracing::warn!(%error, "RPC request handler task failed");
                        }
                        aborts.retain(|_, abort| !abort.is_finished());
                    }
                    None => {}
                }
            }
            // A closed channel also means shutdown: the accept loop is gone.
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break None;
                }
            }
            result = &mut *writer => {
                break Some(match result {
                    Ok(Ok(())) => ConnectionError::closed("RPC writer stopped"),
                    Ok(Err(error)) => error,
                    Err(error) => ConnectionError::runtime(format!("RPC writer task failed: {error}")),
                });
            }
            event = incoming.recv() => {
                match event {
                    Some(ReadEvent::Frame(ClientFrame::Cancel { id })) => {
                        if let Some(abort) = aborts.remove(&id) {
                            abort.abort();
                        }
                    }
                    Some(ReadEvent::Frame(ClientFrame::Request {
                        id,
                        timeout_micros,
                        trace_id,
                        body,
                    })) => {
                        let rejection = if aborts.contains_key(&id) {
                            Some(Status::duplicate_request_id())
                        } else if timeout_micros == 0 {
                            Some(Status::deadline_exceeded())
                        } else if timeout_micros > config.max_request_timeout.as_micros() as u64 {
                            Some(Status::timeout_too_large())
                        } else if aborts.len() >= config.max_in_flight_requests_per_connection {
                            Some(Status::resource_exhausted())
                        } else {
                            None
                        };

                        if let Some(status) = rejection {
                            if send_status(&outgoing, id, status).await.is_err() {
                                break Some(ConnectionError::closed("RPC writer stopped"));
                            }
                            continue;
                        }

                        let service = service.clone();
                        let codec = codec.clone();
                        let outgoing = outgoing.clone();
                        let abort = handlers.spawn(async move {
                            let mut context =
                                ServerContext::new(id, timeout_micros, trace_id, peer_addr);
                            let deadline = context.deadline();

                            // Decode, run, and encode on this task: the shared
                            // connection tasks never touch application types.
                            let outcome = match codec.decode(body) {
                                Ok(request) => {
                                    match timeout_at(deadline, service.call(&mut context, request))
                                        .await
                                    {
                                        Ok(Ok(response)) => Ok(response),
                                        Ok(Err(error)) => Err(error.into()),
                                        Err(_) => Err(Status::deadline_exceeded()),
                                    }
                                }
                                Err(error) => Err(Status::invalid_argument(error.to_string())),
                            };

                            let frame = match outcome {
                                Ok(response) => {
                                    let mut scratch = BytesMut::with_capacity(1024);
                                    // A codec failure or an oversized response
                                    // fails this call only. Sending it as a frame
                                    // would end the connection for every other
                                    // call sharing it.
                                    match codec.encode(&response, &mut scratch) {
                                        Ok(body) if body.len() <= max_response_bytes => {
                                            ServerFrame::Response { id, code: None, body }
                                        }
                                        Ok(body) => status_frame(
                                            id,
                                            Status::internal(format!(
                                                "encoded response is {} bytes, limit is {}",
                                                body.len(),
                                                max_response_bytes,
                                            )),
                                        ),
                                        Err(error) => {
                                            status_frame(id, Status::internal(error.to_string()))
                                        }
                                    }
                                }
                                Err(status) => status_frame(id, status),
                            };

                            // Send without a deadline: the response is already
                            // paid for, and dropping it here would strand a
                            // caller that is still waiting.
                            let _ = outgoing.send(frame).await;
                            id
                        });
                        aborts.insert(id, abort);
                    }
                    // The peer is done sending. In-flight work still gets its
                    // grace period so a response already paid for can be written.
                    Some(ReadEvent::Closed) | None => break None,
                    Some(ReadEvent::Failed(error)) => break Some(error),
                }
            }
        }
    };

    if !handlers.is_empty() {
        drain_handlers(&mut handlers, config.shutdown_grace).await;
    }
    drop(outgoing);
    terminal_error.map_or(Ok(()), Err)
}

/// Waits out the grace period, then cancels whatever is still running.
async fn drain_handlers(handlers: &mut JoinSet<u64>, grace: Duration) {
    if !grace.is_zero() {
        let deadline = tokio::time::Instant::now() + grace;
        while !handlers.is_empty() {
            match timeout_at(deadline, handlers.join_next()).await {
                Ok(Some(Err(error))) if !error.is_cancelled() => {
                    tracing::warn!(%error, "RPC request handler task failed during drain");
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {
                    tracing::debug!("RPC shutdown grace elapsed; cancelling in-flight requests");
                    break;
                }
            }
        }
    }

    handlers.abort_all();
    while let Some(result) = handlers.join_next().await {
        if let Err(error) = result
            && !error.is_cancelled()
        {
            tracing::warn!(%error, "RPC request handler task failed during shutdown");
        }
    }
}

fn status_frame(id: u64, status: Status) -> ServerFrame {
    ServerFrame::Response {
        id,
        code: Some(status.code()),
        body: status.into_wire_message(),
    }
}

async fn send_status(
    outgoing: &mpsc::Sender<ServerFrame>,
    id: u64,
    status: Status,
) -> Result<(), mpsc::error::SendError<ServerFrame>> {
    outgoing.send(status_frame(id, status)).await
}
