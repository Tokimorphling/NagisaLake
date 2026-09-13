//! Hub-side SMUX session acceptance.
use crate::{HubControl, TransportError, bridge::bridge};
use axum::extract::ws::WebSocket as AxumWebSocket;
use std::{sync::Arc, time::Duration};
use tokilake_core::tunnel::TunnelSession;
use tokilake_smux::{Config as SmuxConfig, Session as SmuxSession, Stream as SmuxStream};
use tokio::{sync::Mutex, task::JoinHandle};

pub struct HubTransport {
    session: Arc<Mutex<SmuxSession>>,
    control: HubControl<SmuxStream>,
    bridge:  JoinHandle<Result<(), TransportError>>,
}

impl HubTransport {
    pub async fn accept(
        socket: AxumWebSocket,
        max_frame_bytes: usize,
        accept_timeout: Duration,
    ) -> Result<Self, TransportError> {
        if max_frame_bytes == 0 {
            return Err(TransportError::InvalidConfig(
                "max_frame_bytes must be greater than zero",
            ));
        }
        if accept_timeout.is_zero() {
            return Err(TransportError::InvalidConfig(
                "accept_timeout must be greater than zero",
            ));
        }
        let (io, bridge) = bridge(socket);
        let mut session = SmuxSession::server(io, SmuxConfig::default());
        let stream = tokio::time::timeout(accept_timeout, session.accept())
            .await
            .map_err(|_| TransportError::ConnectTimeout)?
            .ok_or(TransportError::Closed)?;
        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            control: HubControl::new(stream, max_frame_bytes)?,
            bridge,
        })
    }

    pub fn control_mut(&mut self) -> &mut HubControl<SmuxStream> {
        &mut self.control
    }

    pub async fn open_stream(&self) -> Result<SmuxStream, TransportError> {
        self.session
            .lock()
            .await
            .open_stream()
            .await
            .map_err(Into::into)
    }

    pub fn is_alive(&self) -> bool {
        !self.bridge.is_finished()
    }
}

impl Drop for HubTransport {
    fn drop(&mut self) {
        self.bridge.abort();
    }
}
