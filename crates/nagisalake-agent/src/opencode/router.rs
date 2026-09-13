use super::{OpenCodeClient, sse::Decoder, wire::WireEvent};
use futures_util::StreamExt;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

type Routes = Arc<Mutex<HashMap<String, broadcast::Sender<Arc<WireEvent>>>>>;

/// One upstream SSE stream per backend instance. A frame is delivered only to
/// its session, not broadcast/deep-cloned into every active execution.
pub(super) struct EventRouter {
    routes:       Routes,
    cancellation: CancellationToken,
    capacity:     usize,
}

impl EventRouter {
    pub async fn start(client: OpenCodeClient) -> Self {
        let routes = Routes::default();
        let cancellation = CancellationToken::new();
        let capacity = client.config().upstream_buffer;
        // Initial SSE failure must not disable reliable polling completion.
        let initial = client.event_stream().await.ok();
        tokio::spawn(event_loop(
            client,
            routes.clone(),
            cancellation.clone(),
            initial,
        ));
        Self {
            routes,
            cancellation,
            capacity,
        }
    }

    pub fn subscribe(&self, session_id: &str) -> Subscription {
        let (sender, receiver) = broadcast::channel(self.capacity);
        self.routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id.to_owned(), sender);
        Subscription {
            session_id: session_id.to_owned(),
            receiver,
            routes: self.routes.clone(),
        }
    }
}

impl Drop for EventRouter {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

pub(super) struct Subscription {
    session_id: String,
    receiver:   broadcast::Receiver<Arc<WireEvent>>,
    routes:     Routes,
}

impl Subscription {
    pub async fn recv(&mut self) -> Result<Arc<WireEvent>, broadcast::error::RecvError> {
        self.receiver.recv().await
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.session_id);
    }
}

async fn event_loop(
    client: OpenCodeClient,
    routes: Routes,
    cancellation: CancellationToken,
    mut response: Option<reqwest::Response>,
) {
    let mut delay = Duration::from_millis(250);
    loop {
        let current = match response.take() {
            Some(response) => response,
            None => {
                tokio::select! {
                    () = cancellation.cancelled() => return,
                    () = tokio::time::sleep(delay) => {}
                }
                let result = tokio::select! {
                    () = cancellation.cancelled() => return,
                    result = client.event_stream() => result,
                };
                match result {
                    Ok(response) => response,
                    Err(error) => {
                        tracing::warn!(
                            code = error.code(),
                            "OpenCode SSE reconnect failed; polling remains active"
                        );
                        delay = (delay * 2).min(Duration::from_secs(5));
                        continue;
                    }
                }
            }
        };
        let mut decoder = Decoder::new(client.config().max_event_bytes);
        let mut stream = current.bytes_stream();
        loop {
            let chunk = tokio::select! {
                () = cancellation.cancelled() => return,
                chunk = tokio::time::timeout(Duration::from_secs(60), stream.next()) => chunk,
            };
            let Ok(Some(Ok(chunk))) = chunk else {
                break;
            };
            let Ok(data) = decoder.push(&chunk) else {
                break;
            };
            if !data.is_empty() {
                delay = Duration::from_millis(250);
            }
            for data in data {
                let Ok(event) = WireEvent::parse(&data) else {
                    // Neither raw SSE text nor errors including raw text may be logged.
                    tracing::warn!("invalid OpenCode SSE event; polling remains active");
                    continue;
                };
                let Some(session_id) = event.session_id() else {
                    continue;
                };
                let sender = routes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(session_id)
                    .cloned();
                if let Some(sender) = sender {
                    let _ = sender.send(Arc::new(event));
                }
            }
        }
    }
}
