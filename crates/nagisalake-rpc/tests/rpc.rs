use futures_util::{StreamExt, stream::FuturesUnordered};
use nagisalake_rpc::{
    Client, ClientBuilder, ClientConfig, ClientContext, Code, ConnectionErrorKind, FrameConfig,
    Layer, RpcError, Server, ServerConfig, ServerContext, Service, SplitDuplex, Status,
    TcpConnector, TcpIncoming, TraceId,
};
use serde::{Deserialize, Serialize};
use std::{
    future::pending,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{DuplexStream, duplex},
    net::TcpListener,
    sync::Notify,
};

#[derive(Debug, Serialize, Deserialize)]
enum TestRequest {
    Echo(u64),
    Sleep { millis: u64, value: u64 },
    Block,
    WaitForCancellation,
    TraceByte,
    Blob(Vec<u8>),
}

#[derive(Clone, Default)]
struct TestService {
    block_started:   Arc<Notify>,
    block_release:   Arc<Notify>,
    cancel_started:  Arc<Notify>,
    cancel_observed: Arc<AtomicBool>,
}

impl Service<ServerContext, TestRequest> for TestService {
    type Response = u64;
    type Error = Status;

    async fn call(
        &self,
        context: &mut ServerContext,
        request: TestRequest,
    ) -> Result<Self::Response, Self::Error> {
        match request {
            TestRequest::Echo(value) => Ok(value),
            TestRequest::Sleep { millis, value } => {
                tokio::time::sleep(Duration::from_millis(millis)).await;
                Ok(value)
            }
            TestRequest::Block => {
                self.block_started.notify_one();
                self.block_release.notified().await;
                Ok(1)
            }
            TestRequest::WaitForCancellation => {
                self.cancel_started.notify_one();
                let _guard = CancellationObserved(self.cancel_observed.clone());
                pending::<()>().await;
                unreachable!()
            }
            TestRequest::TraceByte => Ok(context.trace_id().into_bytes()[0] as u64),
            TestRequest::Blob(bytes) => Ok(bytes.len() as u64),
        }
    }
}

struct CancellationObserved(Arc<AtomicBool>);

impl Drop for CancellationObserved {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn start_duplex(
    service: TestService,
    server_config: ServerConfig,
    client_config: ClientConfig,
) -> (
    Client<TestRequest, u64>,
    tokio::task::JoinHandle<Result<(), nagisalake_rpc::ServeError>>,
) {
    let (client_io, server_io) = duplex(1024 * 1024);
    let server = tokio::spawn(
        Server::new(service)
            .config(server_config)
            .serve_transport::<TestRequest, u64, SplitDuplex<DuplexStream>>(SplitDuplex(server_io)),
    );
    let client = ClientBuilder::<TestRequest, u64>::new()
        .config(client_config)
        .connect_transport(SplitDuplex(client_io))
        .unwrap();
    (client, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiplexes_many_concurrent_calls() {
    let (client, server) = start_duplex(
        TestService::default(),
        ServerConfig::default(),
        ClientConfig::default(),
    );
    let mut calls = FuturesUnordered::new();
    for value in 0..512 {
        let client = client.clone();
        calls.push(async move {
            client
                .call(ClientContext::default(), TestRequest::Echo(value))
                .await
        });
    }

    let mut sum = 0;
    while let Some(result) = calls.next().await {
        sum += result.unwrap();
    }
    assert_eq!(sum, (0..512).sum());

    drop(client);
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn propagates_trace_and_enforces_deadline() {
    let (client, server) = start_duplex(
        TestService::default(),
        ServerConfig::default(),
        ClientConfig::default(),
    );
    let context = ClientContext::with_timeout(Duration::from_secs(1))
        .with_trace_id(TraceId::from_bytes([7; 16]));
    assert_eq!(
        client.call(context, TestRequest::TraceByte).await.unwrap(),
        7
    );

    let error = client
        .call(
            ClientContext::with_timeout(Duration::from_millis(10)),
            TestRequest::Sleep {
                millis: 200,
                value:  1,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        RpcError::DeadlineExceeded | RpcError::Remote(Status { .. })
    ));
    if let RpcError::Remote(status) = error {
        assert_eq!(status.code(), Code::DeadlineExceeded);
    }

    drop(client);
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_call_cancels_server_handler() {
    let service = TestService::default();
    let cancel_started = service.cancel_started.clone();
    let cancel_observed = service.cancel_observed.clone();
    let (client, server) = start_duplex(service, ServerConfig::default(), ClientConfig::default());

    let call_client = client.clone();
    let call = tokio::spawn(async move {
        call_client
            .call(
                ClientContext::with_timeout(Duration::from_secs(30)),
                TestRequest::WaitForCancellation,
            )
            .await
    });
    cancel_started.notified().await;
    call.abort();

    tokio::time::timeout(Duration::from_secs(1), async {
        while !cancel_observed.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    drop(client);
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_requests_above_connection_capacity() {
    let service = TestService::default();
    let block_started = service.block_started.clone();
    let block_release = service.block_release.clone();
    let server_config = ServerConfig {
        max_in_flight_requests_per_connection: 1,
        ..ServerConfig::default()
    };
    let (client, server) = start_duplex(service, server_config, ClientConfig::default());

    let blocked_client = client.clone();
    let blocked = tokio::spawn(async move {
        blocked_client
            .call(ClientContext::default(), TestRequest::Block)
            .await
    });
    block_started.notified().await;

    let error = client
        .call(ClientContext::default(), TestRequest::Echo(2))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        RpcError::Remote(ref status) if status.code() == Code::ResourceExhausted
    ));

    block_release.notify_one();
    assert_eq!(blocked.await.unwrap().unwrap(), 1);
    drop(client);
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_deadlines_above_server_limit() {
    let server_config = ServerConfig {
        max_request_timeout: Duration::from_millis(50),
        ..ServerConfig::default()
    };
    let (client, server) = start_duplex(
        TestService::default(),
        server_config,
        ClientConfig::default(),
    );

    let error = client
        .call(
            ClientContext::with_timeout(Duration::from_secs(1)),
            TestRequest::Echo(1),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        RpcError::Remote(ref status) if status.code() == Code::InvalidArgument
    ));

    drop(client);
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_request_fails_only_that_call() {
    let client_config = ClientConfig {
        frame: FrameConfig {
            max_frame_bytes: 1024,
            ..FrameConfig::default()
        },
        ..ClientConfig::default()
    };
    let (client, server) = start_duplex(
        TestService::default(),
        ServerConfig::default(),
        client_config,
    );

    let error = client
        .call(ClientContext::default(), TestRequest::Blob(vec![0; 4096]))
        .await
        .unwrap_err();
    assert!(
        matches!(error, RpcError::RequestTooLarge { .. }),
        "expected a per-call rejection, got {error:?}"
    );

    // The connection must still serve other calls.
    assert_eq!(
        client
            .call(ClientContext::default(), TestRequest::Echo(7))
            .await
            .unwrap(),
        7
    );

    drop(client);
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_inbound_frame_ends_connection() {
    // A frame that arrives over the limit cannot be skipped: the reader has no
    // way to resynchronize mid-stream, so the connection must end.
    let server_config = ServerConfig {
        frame: FrameConfig {
            max_frame_bytes: 512,
            ..FrameConfig::default()
        },
        ..ServerConfig::default()
    };
    let (client, server) = start_duplex(
        TestService::default(),
        server_config,
        ClientConfig::default(),
    );

    let error = client
        .call(ClientContext::default(), TestRequest::Blob(vec![0; 8192]))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            RpcError::Connection(ref connection)
                if connection.kind() == ConnectionErrorKind::Closed
        ),
        "expected the connection to end, got {error:?}"
    );

    drop(client);
    let served = tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
    let error = served.unwrap_err();
    assert!(error.to_string().contains("limit"), "unexpected: {error}");
}

#[derive(Clone)]
struct OrderService(Arc<Mutex<Vec<&'static str>>>);

impl Service<ServerContext, TestRequest> for OrderService {
    type Response = u64;
    type Error = Status;

    async fn call(
        &self,
        _context: &mut ServerContext,
        _request: TestRequest,
    ) -> Result<Self::Response, Self::Error> {
        self.0.lock().unwrap().push("handler");
        Ok(1)
    }
}

#[derive(Clone)]
struct RecordLayer {
    name:    &'static str,
    records: Arc<Mutex<Vec<&'static str>>>,
}

impl<S> Layer<S> for RecordLayer {
    type Service = RecordService<S>;

    fn layer(self, inner: S) -> Self::Service {
        RecordService {
            name: self.name,
            records: self.records,
            inner,
        }
    }
}

#[derive(Clone)]
struct RecordService<S> {
    name:    &'static str,
    records: Arc<Mutex<Vec<&'static str>>>,
    inner:   S,
}

impl<S> Service<ServerContext, TestRequest> for RecordService<S>
where
    S: Service<ServerContext, TestRequest, Response = u64, Error = Status> + Send + Sync,
{
    type Response = u64;
    type Error = Status;

    async fn call(
        &self,
        context: &mut ServerContext,
        request: TestRequest,
    ) -> Result<Self::Response, Self::Error> {
        self.records.lock().unwrap().push(self.name);
        let result = self.inner.call(context, request).await;
        self.records.lock().unwrap().push(match self.name {
            "outer" => "outer-after",
            _ => "inner-after",
        });
        result
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn latest_server_layer_is_outermost() {
    let records = Arc::new(Mutex::new(Vec::new()));
    let (client_io, server_io) = duplex(64 * 1024);
    let server = tokio::spawn(
        Server::new(OrderService(records.clone()))
            .layer(RecordLayer {
                name:    "inner",
                records: records.clone(),
            })
            .layer(RecordLayer {
                name:    "outer",
                records: records.clone(),
            })
            .serve_transport::<TestRequest, u64, _>(SplitDuplex(server_io)),
    );
    let client = ClientBuilder::<TestRequest, u64>::new()
        .connect_transport(SplitDuplex(client_io))
        .unwrap();
    assert_eq!(
        client
            .call(ClientContext::default(), TestRequest::Echo(1))
            .await
            .unwrap(),
        1
    );
    assert_eq!(records.lock().unwrap().as_slice(), [
        "outer",
        "inner",
        "handler",
        "inner-after",
        "outer-after"
    ]);

    drop(client);
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_makers_complete_round_trip() {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpIncoming::new(listener);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(
        Server::new(TestService::default()).serve_with_shutdown::<TestRequest, u64, _, _>(
            incoming,
            async move {
                let _ = shutdown_rx.await;
            },
        ),
    );

    let client = ClientBuilder::<TestRequest, u64>::new()
        .transport(TcpConnector::new(addr).with_connect_timeout(Duration::from_secs(1)))
        .connect()
        .await
        .unwrap();
    assert_eq!(
        client
            .call(ClientContext::default(), TestRequest::Echo(42))
            .await
            .unwrap(),
        42
    );

    drop(client);
    let _ = shutdown_tx.send(());
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_drains_in_flight_requests() {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(
        Server::new(TestService::default())
            .config(ServerConfig {
                shutdown_grace: Duration::from_secs(5),
                ..ServerConfig::default()
            })
            .serve_with_shutdown::<TestRequest, u64, _, _>(
                TcpIncoming::new(listener),
                async move {
                    let _ = shutdown_rx.await;
                },
            ),
    );

    let client = ClientBuilder::<TestRequest, u64>::new()
        .transport(TcpConnector::new(addr))
        .connect()
        .await
        .unwrap();

    // Start work, then shut down while it is still running.
    let call_client = client.clone();
    let call = tokio::spawn(async move {
        call_client
            .call(
                ClientContext::with_timeout(Duration::from_secs(10)),
                TestRequest::Sleep {
                    millis: 300,
                    value:  99,
                },
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _ = shutdown_tx.send(());

    // The in-flight call completes rather than being cancelled.
    assert_eq!(call.await.unwrap().unwrap(), 99);

    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_grace_zero_cancels_in_flight_requests() {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(
        Server::new(TestService::default())
            .config(ServerConfig {
                shutdown_grace: Duration::ZERO,
                ..ServerConfig::default()
            })
            .serve_with_shutdown::<TestRequest, u64, _, _>(
                TcpIncoming::new(listener),
                async move {
                    let _ = shutdown_rx.await;
                },
            ),
    );

    let client = ClientBuilder::<TestRequest, u64>::new()
        .transport(TcpConnector::new(addr))
        .connect()
        .await
        .unwrap();

    let call_client = client.clone();
    let call = tokio::spawn(async move {
        call_client
            .call(
                ClientContext::with_timeout(Duration::from_secs(10)),
                TestRequest::Sleep {
                    millis: 2_000,
                    value:  1,
                },
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _ = shutdown_tx.send(());

    let error = tokio::time::timeout(Duration::from_secs(2), call)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(error, RpcError::Connection(_)),
        "expected the call to fail with the connection, got {error:?}"
    );

    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// A request type whose encoding always fails.
#[derive(Debug)]
struct Unencodable;

impl Serialize for Unencodable {
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("this type never encodes"))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encode_failure_fails_only_that_call() {
    #[derive(Debug, Serialize, Deserialize)]
    enum MaybeEncodable {
        Fine(u64),
        #[serde(skip_deserializing)]
        Broken(Unencodable),
    }

    struct EchoService;

    impl Service<ServerContext, MaybeEncodable> for EchoService {
        type Response = u64;
        type Error = Status;

        async fn call(
            &self,
            _context: &mut ServerContext,
            request: MaybeEncodable,
        ) -> Result<Self::Response, Self::Error> {
            match request {
                MaybeEncodable::Fine(value) => Ok(value),
                MaybeEncodable::Broken(_) => Ok(0),
            }
        }
    }

    let (client_io, server_io) = duplex(64 * 1024);
    let server = tokio::spawn(
        Server::new(EchoService).serve_transport::<MaybeEncodable, u64, _>(SplitDuplex(server_io)),
    );
    let client = ClientBuilder::<MaybeEncodable, u64>::new()
        .connect_transport(SplitDuplex(client_io))
        .unwrap();

    let error = client
        .call(
            ClientContext::default(),
            MaybeEncodable::Broken(Unencodable),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, RpcError::Codec(_)),
        "expected a codec failure, got {error:?}"
    );

    // The connection is untouched: nothing was ever written for the failed call.
    assert_eq!(
        client
            .call(ClientContext::default(), MaybeEncodable::Fine(5))
            .await
            .unwrap(),
        5
    );

    drop(client);
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn many_calls_share_one_connection_across_clones() {
    // Exercises the read path's frame batching: many small responses arrive
    // together and must each land on the right caller.
    let (client, server) = start_duplex(
        TestService::default(),
        ServerConfig::default(),
        ClientConfig::default(),
    );

    let mut calls = FuturesUnordered::new();
    for value in 0..2_048u64 {
        let client = client.clone();
        calls.push(async move {
            let observed = client
                .call(ClientContext::default(), TestRequest::Echo(value))
                .await
                .unwrap();
            assert_eq!(observed, value, "response routed to the wrong caller");
            observed
        });
    }
    let mut count = 0u64;
    while calls.next().await.is_some() {
        count += 1;
    }
    assert_eq!(count, 2_048);

    drop(client);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
