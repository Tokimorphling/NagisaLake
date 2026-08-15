//! Measures round-trip throughput and latency over one TCP connection.
//!
//! ```text
//! cargo run --release -p nagisalake-rpc --example throughput -- [payload_bytes] [concurrency]
//! ```

use nagisalake_rpc::{
    Client, ClientBuilder, ClientContext, Server, ServerContext, Service, Status, TcpConnector,
    TcpIncoming,
};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::{Duration, Instant},
};

#[derive(Debug, Serialize, Deserialize)]
struct Echo {
    payload: Vec<u8>,
}

struct EchoService;

impl Service<ServerContext, Echo> for EchoService {
    type Response = usize;
    type Error = Status;

    async fn call(
        &self,
        _cx: &mut ServerContext,
        req: Echo,
    ) -> Result<Self::Response, Self::Error> {
        Ok(req.payload.len())
    }
}

const CALLS: usize = 200_000;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let payload_bytes: usize = args.next().map_or(Ok(64), |a| a.parse())?;
    let concurrency: usize = args.next().map_or(Ok(64), |a| a.parse())?;

    // Bind first so the client has a concrete port to dial.
    let listener =
        tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
    let addr = listener.local_addr()?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(
        Server::new(EchoService).serve_with_shutdown::<Echo, usize, _, _>(
            TcpIncoming::new(listener),
            async move {
                let _ = shutdown_rx.await;
            },
        ),
    );

    let client: Client<Echo, usize> = ClientBuilder::new()
        .transport(TcpConnector::new(addr))
        .connect()
        .await?;

    // Warm up the connection and the allocator before timing.
    for _ in 0..1_000 {
        client
            .call(ClientContext::default(), Echo {
                payload: vec![0; payload_bytes],
            })
            .await?;
    }

    let mut latencies = Vec::with_capacity(CALLS);
    let started = Instant::now();
    let mut remaining = CALLS;
    let mut in_flight = futures_util::stream::FuturesUnordered::new();

    while remaining > 0 || !in_flight.is_empty() {
        while in_flight.len() < concurrency && remaining > 0 {
            remaining -= 1;
            let client = client.clone();
            in_flight.push(async move {
                let call_started = Instant::now();
                let result = client
                    .call(ClientContext::default(), Echo {
                        payload: vec![0; payload_bytes],
                    })
                    .await;
                (result, call_started.elapsed())
            });
        }
        if let Some((result, latency)) = futures_util::StreamExt::next(&mut in_flight).await {
            result?;
            latencies.push(latency);
        }
    }
    let elapsed = started.elapsed();

    latencies.sort_unstable();
    let percentile = |p: f64| -> Duration {
        let index = ((latencies.len() as f64 - 1.0) * p).round() as usize;
        latencies[index]
    };

    println!("payload {payload_bytes} B, concurrency {concurrency}");
    println!(
        "{CALLS} calls in {:.2?}  =>  {:.0} calls/s",
        elapsed,
        CALLS as f64 / elapsed.as_secs_f64()
    );
    println!(
        "latency  p50 {:?}  p90 {:?}  p99 {:?}  p999 {:?}",
        percentile(0.50),
        percentile(0.90),
        percentile(0.99),
        percentile(0.999),
    );

    drop(client);
    let _ = shutdown_tx.send(());
    let _ = server.await;
    Ok(())
}
