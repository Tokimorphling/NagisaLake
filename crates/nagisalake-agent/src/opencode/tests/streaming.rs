use super::*;
use axum::response::sse::{Event, Sse};
use futures_util::{StreamExt, stream};
use std::convert::Infallible;

pub(super) fn response(sender: broadcast::Sender<String>) -> Response {
    let initial = stream::once(async {
        Ok::<_, Infallible>(Event::default().data(r#"{"type":"server.connected","properties":{}}"#))
    });
    let events = stream::unfold(sender.subscribe(), |mut receiver| async move {
        let data = receiver.recv().await.ok()?;
        Some((Ok::<_, Infallible>(Event::default().data(data)), receiver))
    });
    // End the first connection after the six test frames. The router must
    // reconnect while polling still waits for a final assistant completion.
    Sse::new(initial.chain(events).take(7)).into_response()
}

pub(super) fn publish(sender: &broadcast::Sender<String>, session_id: &str) {
    for event in [
        json!({"type":"message.updated","properties":{"info":{"id":"msg_stream","role":"assistant","sessionID":session_id}}}),
        json!({"type":"message.part.updated","properties":{"part":{"id":"part_stream","messageID":"msg_stream","sessionID":session_id,"type":"text","text":""}}}),
        json!({"type":"message.part.delta","properties":{"sessionID":session_id,"partID":"part_stream","field":"text","delta":"streamed"}}),
        json!({"type":"message.part.updated","properties":{"part":{"id":"part_reason","messageID":"msg_stream","sessionID":session_id,"type":"reasoning","text":""}}}),
        json!({"type":"message.part.delta","properties":{"sessionID":session_id,"partID":"part_reason","field":"text","delta":"SECRET reasoning"}}),
        json!({"type":"message.part.delta","properties":{"sessionID":"ses_other","partID":"part_stream","field":"text","delta":"SECRET other tenant"}}),
    ] {
        sender.send(event.to_string()).unwrap();
    }
}

#[tokio::test]
async fn live_sse_routes_only_own_text_reconnects_and_reconciles_final_output() {
    let (config, state, task) = spawn_mock(false, false).await;
    *state.events.lock().unwrap() = Some(broadcast::channel(32).0);
    let service = service(config);
    let mut run = service.call(request()).await.unwrap();
    let mut all = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(event) = run.recv().await {
            all.push(event);
        }
    })
    .await
    .unwrap();
    assert!(
        all.iter()
            .any(|event| matches!(event, RunEvent::TextDelta {text} if text == "streamed"))
    );
    assert!(matches!(all.last(), Some(RunEvent::Completed {text,..}) if text == "result:ses_0"));
    assert!(!serde_json::to_string(&all).unwrap().contains("SECRET"));
    assert!(state.streams.load(Ordering::Relaxed) >= 2);
    task.abort();
}
