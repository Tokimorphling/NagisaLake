use super::*;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Error;

struct BackpressuredSocket {
    incoming: mpsc::Receiver<TungsteniteMessage>,
    blocked:  Arc<Notify>,
}
impl Stream for BackpressuredSocket {
    type Item = Result<TungsteniteMessage, Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.incoming.poll_recv(cx).map(|message| message.map(Ok))
    }
}
impl Sink<TungsteniteMessage> for BackpressuredSocket {
    type Error = Error;
    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.blocked.notify_one();
        Poll::Pending
    }
    fn start_send(self: Pin<&mut Self>, _item: TungsteniteMessage) -> Result<(), Error> {
        unreachable!()
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Pending
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Pending
    }
}

#[tokio::test]
async fn blocked_websocket_send_does_not_stop_incoming_control_bytes() {
    let blocked = Arc::new(Notify::new());
    let (tx, rx) = mpsc::channel(4);
    let (mut smux, task) = bridge(BackpressuredSocket {
        incoming: rx,
        blocked:  blocked.clone(),
    });
    smux.write_all(b"outbound").await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), blocked.notified())
        .await
        .unwrap();
    tx.send(TungsteniteMessage::Binary(Bytes::from_static(b"ack")))
        .await
        .unwrap();
    let mut result = [0_u8; 3];
    tokio::time::timeout(Duration::from_secs(1), smux.read_exact(&mut result))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&result, b"ack");
    task.abort();
}
