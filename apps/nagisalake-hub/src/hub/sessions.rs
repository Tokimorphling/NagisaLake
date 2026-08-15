use super::*;

/// Current worker connection metadata returned by the health API.
///
/// The ids are `FastStr` rather than `String`: a view is cloned on every list,
/// dispatch and job event, and at 36 bytes a uuid is past the inline threshold,
/// so each clone would otherwise allocate and copy every one of these fields.
#[derive(Debug, Clone, Serialize)]
pub struct WorkerView {
    pub organization_id: FastStr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_user_id:   Option<FastStr>,
    pub worker_id:       FastStr,
    pub session_id:      FastStr,
    pub namespace:       FastStr,
    pub node_name:       FastStr,
    pub capabilities:    WorkerCapabilities,
    pub active_jobs:     u16,
    pub queued_jobs:     u16,
    pub connected_at:    i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowView {
    pub id:                  String,
    pub version:             String,
    pub output_types:        Vec<String>,
    pub manifest:            Option<WorkflowManifest>,
    pub manifest_consistent: bool,
    pub workers:             Vec<WorkflowWorkerView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowWorkerView {
    pub organization_id: FastStr,
    pub worker_id:       FastStr,
    pub session_id:      FastStr,
    pub labels:          BTreeMap<String, String>,
    pub parallelism:     u16,
    pub queue_depth:     u16,
    pub active_jobs:     u16,
    pub queued_jobs:     u16,
    pub available:       bool,
}

#[derive(Debug, Clone)]
pub(super) struct WorkerSession {
    pub(super) view: WorkerView,
    pub(super) credential_id: Option<String>,
    pub(super) outbound: mpsc::Sender<HubMessage>,
    pub(super) pending: Arc<Mutex<HashMap<String, oneshot::Sender<CommandAck>>>>,
    pub(super) pending_capacity_reservations: HashSet<String>,
    pub(super) confirmed_capacity_reservations: HashSet<String>,
    pub(super) disconnect: CancellationToken,
    /// Monotonic time of the last accepted control message from this worker.
    ///
    /// Deliberately `Instant` rather than a wall clock: a clock adjustment must
    /// not make a live session look stale, nor keep a dead one alive.
    pub(super) last_seen: Instant,
}

/// In-memory single-instance worker session directory.
#[derive(Debug, Clone, Default)]
pub struct SessionRegistry {
    pub(super) inner:               Arc<RwLock<HashMap<String, WorkerSession>>>,
    pub(super) revoked_credentials: Arc<RwLock<HashSet<String>>>,
}

impl SessionRegistry {
    /// Registers a session, refusing one whose credential was already revoked.
    ///
    /// The read guard deliberately spans the write into `inner` below.
    /// `disconnect_credential` marks the credential under the write side and only
    /// then sweeps `inner`, so releasing the guard early would open the very gap
    /// that pairing exists to close: the check passes, a revocation sweeps an
    /// `inner` that does not hold this session yet, and the insert lands after the
    /// sweep — leaving a revoked credential connected with nothing left to remove
    /// it. Both paths take `revoked_credentials` before `inner` and never the
    /// reverse, so holding across the write cannot deadlock.
    pub(super) async fn insert(&self, session: WorkerSession) -> bool {
        let revoked_credentials = self.revoked_credentials.read().await;
        if session
            .credential_id
            .as_ref()
            .is_some_and(|credential_id| revoked_credentials.contains(credential_id))
        {
            return false;
        }
        let key = session_key(&session.view.organization_id, &session.view.worker_id);
        let previous = self.inner.write().await.insert(key, session);
        // `inner` holds the session now, so any later revocation will find it.
        drop(revoked_credentials);
        if let Some(previous) = previous {
            previous.disconnect.cancel();
            let _ = previous.outbound.try_send(HubMessage::Error(ProtocolError {
                code:      "session_replaced".into(),
                message:   "a newer connection replaced this worker session".into(),
                retryable: true,
            }));
        }
        true
    }

    pub(super) async fn disconnect_credential(&self, credential_id: &str) {
        let mut removed = Vec::new();
        {
            let mut revoked_credentials = self.revoked_credentials.write().await;
            revoked_credentials.insert(credential_id.into());
            let mut sessions = self.inner.write().await;
            sessions.retain(|_, session| {
                if session.credential_id.as_deref() == Some(credential_id) {
                    removed.push((session.outbound.clone(), session.disconnect.clone()));
                    false
                } else {
                    true
                }
            });
        }
        for (outbound, disconnect) in removed {
            disconnect.cancel();
            let _ = outbound.try_send(HubMessage::Error(ProtocolError {
                code:      "credential_revoked".into(),
                message:   "the worker credential has been revoked".into(),
                retryable: false,
            }));
        }
    }

    /// Drops revoked credentials that no longer have a live session.
    ///
    /// `revoked_credentials` is append-only: without this sweep a long-running
    /// Hub accumulates one entry per revocation forever, and each new
    /// connection pays an O(N) `contains` check against the growing set. The
    /// revocation still reaches the DB (`revoked_at`), so a credential that
    /// reappears here is rejected by `insert` only if it is still listed;
    /// otherwise it falls back to the store check that already existed before
    /// the in-memory set was introduced.
    pub(super) async fn reap_revoked_credentials(&self) -> usize {
        let mut revoked = self.revoked_credentials.write().await;
        if revoked.is_empty() {
            return 0;
        }
        let sessions = self.inner.read().await;
        let live: std::collections::HashSet<&str> = sessions
            .values()
            .filter_map(|session| session.credential_id.as_deref())
            .collect();
        let before = revoked.len();
        revoked.retain(|credential_id| live.contains(credential_id.as_str()));
        before - revoked.len()
    }

    pub(super) async fn disconnect_organization(&self, organization_id: &str) {
        let removed = {
            let mut sessions = self.inner.write().await;
            let mut removed = Vec::new();
            sessions.retain(|_, session| {
                if session.view.organization_id == organization_id {
                    removed.push((session.outbound.clone(), session.disconnect.clone()));
                    false
                } else {
                    true
                }
            });
            removed
        };
        for (outbound, disconnect) in removed {
            disconnect.cancel();
            let _ = outbound.try_send(HubMessage::Error(ProtocolError {
                code:      "organization_deleted".into(),
                message:   "the worker organization has been deleted".into(),
                retryable: false,
            }));
        }
    }

    pub(super) async fn remove_if_current(
        &self,
        organization_id: &str,
        worker_id: &str,
        session_id: &str,
    ) {
        let mut sessions = self.inner.write().await;
        let key = session_key(organization_id, worker_id);
        if sessions
            .get(&key)
            .is_some_and(|session| session.view.session_id == session_id)
        {
            sessions.remove(&key);
        }
    }

    /// The session id a worker is connected under right now, if any.
    ///
    /// Job records carry the session they were dispatched to, and that id goes
    /// stale the moment the worker reconnects. Anything that wants to reach the
    /// worker, or to decide whether a frame really came from it, has to ask the
    /// registry rather than trust the recorded id.
    pub(super) async fn current_session_id(
        &self,
        organization_id: &str,
        worker_id: &str,
    ) -> Option<FastStr> {
        self.inner
            .read()
            .await
            .get(&session_key(organization_id, worker_id))
            .map(|session| session.view.session_id.clone())
    }

    pub(super) async fn guard_current_session(
        &self,
        organization_id: &str,
        worker_id: &str,
        session_id: &str,
    ) -> Option<tokio::sync::RwLockReadGuard<'_, HashMap<String, WorkerSession>>> {
        let sessions = self.inner.read().await;
        if sessions
            .get(&session_key(organization_id, worker_id))
            .is_some_and(|session| session.view.session_id == session_id)
        {
            Some(sessions)
        } else {
            None
        }
    }

    pub(super) async fn update_heartbeat(
        &self,
        organization_id: &str,
        worker_id: &str,
        session_id: &str,
        active_jobs: u16,
        queued_jobs: u16,
    ) -> Result<(), HubError> {
        let mut sessions = self.inner.write().await;
        let key = session_key(organization_id, worker_id);
        let session = sessions
            .get_mut(&key)
            .filter(|session| session.view.session_id == session_id)
            .ok_or_else(|| HubError::Conflict("worker session is stale".into()))?;
        session.view.active_jobs = active_jobs;
        session.view.queued_jobs = queued_jobs;
        // Worker ACKs are sent before the connection loop can emit its next
        // heartbeat. Therefore every confirmed reservation is either reflected
        // in this report or the corresponding job has already finished.
        session.confirmed_capacity_reservations.clear();
        session.last_seen = Instant::now();
        Ok(())
    }

    /// Atomically selects a matching worker and reserves one admission slot.
    pub(super) async fn reserve_capacity<F>(
        &self,
        command_id: &str,
        predicate: F,
    ) -> Option<WorkerView>
    where
        F: Fn(&WorkerView) -> bool,
    {
        let mut sessions = self.inner.write().await;
        let selected = sessions
            .iter()
            .filter(|(_key, session)| predicate(&session.view))
            .filter(|(_key, session)| {
                worker_capacity_load(session) < session.view.capabilities.total_capacity()
            })
            .min_by_key(|(_key, session)| worker_capacity_load(session))
            .map(|(key, _session)| key.clone())?;
        let session = sessions.get_mut(&selected)?;
        if !session
            .pending_capacity_reservations
            .insert(command_id.to_owned())
        {
            return None;
        }
        Some(worker_view_with_reservations(session))
    }

    /// Ensures one slot is reserved on an exact session for a durable command.
    ///
    /// The scheduler may already have made this reservation. After a Hub
    /// restart or Worker reconnect it will be absent, so the outbox consumer
    /// must recreate it without broadening the binding to another device.
    pub(super) async fn ensure_capacity_reservation(
        &self,
        organization_id: &str,
        worker_id: &str,
        session_id: &str,
        command_id: &str,
    ) -> bool {
        let mut sessions = self.inner.write().await;
        let Some(session) = sessions
            .get_mut(&session_key(organization_id, worker_id))
            .filter(|session| session.view.session_id == session_id)
        else {
            return false;
        };
        if session.pending_capacity_reservations.contains(command_id)
            || session.confirmed_capacity_reservations.contains(command_id)
        {
            return true;
        }
        if worker_capacity_load(session) >= session.view.capabilities.total_capacity() {
            return false;
        }
        session
            .pending_capacity_reservations
            .insert(command_id.to_owned())
    }

    /// Releases a dispatch that never reached an accepted worker ACK.
    pub(super) async fn release_capacity_reservation(
        &self,
        organization_id: &str,
        worker_id: &str,
        session_id: &str,
        command_id: &str,
    ) {
        let mut sessions = self.inner.write().await;
        let Some(session) = sessions
            .get_mut(&session_key(organization_id, worker_id))
            .filter(|session| session.view.session_id == session_id)
        else {
            return;
        };
        session.pending_capacity_reservations.remove(command_id);
        session.confirmed_capacity_reservations.remove(command_id);
    }

    /// Keeps a timed-out dispatch charged until a heartbeat resolves whether
    /// the worker accepted it. This avoids opening a capacity gap on an ACK race.
    pub(super) async fn mark_capacity_reservation_uncertain(
        &self,
        organization_id: &str,
        worker_id: &str,
        session_id: &str,
        command_id: &str,
    ) {
        let mut sessions = self.inner.write().await;
        let Some(session) = sessions
            .get_mut(&session_key(organization_id, worker_id))
            .filter(|session| session.view.session_id == session_id)
        else {
            return;
        };
        if session.pending_capacity_reservations.remove(command_id) {
            session
                .confirmed_capacity_reservations
                .insert(command_id.to_owned());
        }
    }

    /// Reconciles the reservation before waking the HTTP request waiting for
    /// this ACK. Accepted jobs remain charged until the next heartbeat.
    pub(super) async fn settle_capacity_reservation(
        &self,
        organization_id: &str,
        worker_id: &str,
        session_id: &str,
        command_id: &str,
        accepted: bool,
    ) {
        let mut sessions = self.inner.write().await;
        let Some(session) = sessions
            .get_mut(&session_key(organization_id, worker_id))
            .filter(|session| session.view.session_id == session_id)
        else {
            return;
        };
        if session.pending_capacity_reservations.remove(command_id) && accepted {
            session
                .confirmed_capacity_reservations
                .insert(command_id.to_owned());
        } else if !accepted {
            // A rejection may arrive just after the caller timed out and moved
            // the reservation into the conservative confirmed set.
            session.confirmed_capacity_reservations.remove(command_id);
        }
    }

    /// Views of the sessions satisfying `predicate`, cloned only for matches.
    ///
    /// Filtering after `list` is the expensive way round: a `WorkerView` clone
    /// copies the capability list, every workflow manifest hanging off it and
    /// the label map, so the discarded devices are paid for in full.
    pub(super) async fn list_matching<F>(&self, predicate: F) -> Vec<WorkerView>
    where
        F: Fn(&WorkerView) -> bool,
    {
        self.inner
            .read()
            .await
            .values()
            .filter(|session| predicate(&session.view))
            .map(worker_view_with_reservations)
            .collect()
    }

    /// Views of matching sessions with their workflow list filtered before
    /// cloning. Shared-pool grants often expose one workflow from a large
    /// ComfyUI catalog; cloning the full catalog and then retaining one entry
    /// paid for every hidden manifest.
    pub(super) async fn list_matching_workflows<F, G>(
        &self,
        predicate: F,
        workflow_filter: G,
    ) -> Vec<WorkerView>
    where
        F: Fn(&WorkerView) -> bool,
        G: Fn(&WorkerView, &WorkflowCapability) -> bool,
    {
        self.inner
            .read()
            .await
            .values()
            .filter(|session| predicate(&session.view))
            .filter_map(|session| {
                let workflows = session
                    .view
                    .capabilities
                    .workflows
                    .iter()
                    .filter(|workflow| workflow_filter(&session.view, workflow))
                    .cloned()
                    .collect::<Vec<_>>();
                (!workflows.is_empty())
                    .then(|| worker_view_with_reservations_and_workflows(session, workflows))
            })
            .collect()
    }

    pub(super) async fn list_for_org(&self, organization_id: &str) -> Vec<WorkerView> {
        self.list_matching(|view| view.organization_id == organization_id)
            .await
    }

    /// Connected worker ids grouped by organization.
    ///
    /// Callers that only want to know *whether* a device is connected went
    /// through `list`, which deep-clones every view — manifests included — for
    /// the whole Hub just to keep two ids per row. Nested rather than a set of
    /// pairs so a lookup borrows both halves instead of allocating a tuple of
    /// owned ids per row it tests.
    pub(super) async fn connected_identities(&self) -> HashMap<FastStr, HashSet<FastStr>> {
        let mut connected: HashMap<FastStr, HashSet<FastStr>> = HashMap::new();
        for session in self.inner.read().await.values() {
            connected
                .entry(session.view.organization_id.clone())
                .or_default()
                .insert(session.view.worker_id.clone());
        }
        connected
    }

    /// Drops sessions that have not sent a control message within `max_silence`.
    ///
    /// Without this a half-open TCP connection stays in the registry: the peer
    /// is gone but nothing tells us, so the device keeps reading as connected
    /// and keeps being handed jobs that can never run. The transport answers
    /// pings but never initiates them, so silence is the only signal available.
    ///
    /// Returns the ids of the sessions that were reaped.
    pub(super) async fn reap_stale(&self, max_silence: Duration) -> Vec<FastStr> {
        let now = Instant::now();
        let mut sessions = self.inner.write().await;
        let stale = sessions
            .iter()
            .filter(|(_key, session)| {
                now.saturating_duration_since(session.last_seen) > max_silence
            })
            .map(|(key, session)| (key.clone(), session.view.worker_id.clone()))
            .collect::<Vec<_>>();
        let mut reaped = Vec::with_capacity(stale.len());
        for (key, worker_id) in stale {
            if let Some(session) = sessions.remove(&key) {
                // Wakes the connection actor so it tears down the socket and
                // fails any in-flight command waiters.
                session.disconnect.cancel();
                warn!(
                    %worker_id,
                    session_id = %session.view.session_id,
                    silence_seconds = now.saturating_duration_since(session.last_seen).as_secs(),
                    "dropping worker session that stopped sending heartbeats"
                );
                reaped.push(worker_id);
            }
        }
        reaped
    }

    pub(super) async fn count(&self) -> usize {
        self.inner.read().await.len()
    }

    pub(super) async fn send_command(
        &self,
        organization_id: &str,
        worker_id: &str,
        session_id: &str,
        command_id: &str,
        message: HubMessage,
        timeout: Duration,
    ) -> Result<CommandAck, HubError> {
        let session = self
            .inner
            .read()
            .await
            .get(&session_key(organization_id, worker_id))
            .filter(|session| session.view.session_id == session_id)
            .cloned()
            .ok_or_else(|| HubError::Conflict("worker session is not connected".into()))?;
        let (sender, receiver) = oneshot::channel();
        session
            .pending
            .lock()
            .await
            .insert(command_id.into(), sender);
        if session.outbound.send(message).await.is_err() {
            session.pending.lock().await.remove(command_id);
            self.release_capacity_reservation(organization_id, worker_id, session_id, command_id)
                .await;
            return Err(HubError::Unavailable("worker socket is closed".into()));
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(ack)) => Ok(ack),
            Ok(Err(_)) => {
                self.mark_capacity_reservation_uncertain(
                    organization_id,
                    worker_id,
                    session_id,
                    command_id,
                )
                .await;
                Err(HubError::Unavailable(
                    "worker closed before command ACK".into(),
                ))
            }
            Err(_) => {
                session.pending.lock().await.remove(command_id);
                self.mark_capacity_reservation_uncertain(
                    organization_id,
                    worker_id,
                    session_id,
                    command_id,
                )
                .await;
                Err(HubError::Unavailable("worker command ACK timed out".into()))
            }
        }
    }
}

pub(super) fn capacity_reservation_count(session: &WorkerSession) -> u32 {
    u32::try_from(
        session.pending_capacity_reservations.len() + session.confirmed_capacity_reservations.len(),
    )
    .unwrap_or(u32::MAX)
}

pub(super) fn worker_capacity_load(session: &WorkerSession) -> u32 {
    u32::from(session.view.active_jobs)
        .saturating_add(u32::from(session.view.queued_jobs))
        .saturating_add(capacity_reservation_count(session))
}

pub(super) fn worker_view_with_reservations(session: &WorkerSession) -> WorkerView {
    let mut view = session.view.clone();
    view.queued_jobs = u16::try_from(
        u32::from(view.queued_jobs).saturating_add(capacity_reservation_count(session)),
    )
    .unwrap_or(u16::MAX);
    view
}

fn worker_view_with_reservations_and_workflows(
    session: &WorkerSession,
    workflows: Vec<WorkflowCapability>,
) -> WorkerView {
    WorkerView {
        organization_id: session.view.organization_id.clone(),
        owner_user_id:   session.view.owner_user_id.clone(),
        worker_id:       session.view.worker_id.clone(),
        session_id:      session.view.session_id.clone(),
        namespace:       session.view.namespace.clone(),
        node_name:       session.view.node_name.clone(),
        capabilities:    WorkerCapabilities {
            workflows,
            parallelism: session.view.capabilities.parallelism,
            queue_depth: session.view.capabilities.queue_depth,
            supports_queued_job_cancellation: session
                .view
                .capabilities
                .supports_queued_job_cancellation,
            labels: session.view.capabilities.labels.clone(),
        },
        active_jobs:     session.view.active_jobs,
        queued_jobs:     u16::try_from(
            u32::from(session.view.queued_jobs).saturating_add(capacity_reservation_count(session)),
        )
        .unwrap_or(u16::MAX),
        connected_at:    session.view.connected_at,
    }
}

pub(super) fn session_key(organization_id: &str, worker_id: &str) -> String {
    format!("{organization_id}\0{worker_id}")
}

pub(super) fn pending_upload_key(organization_id: &str, request_id: &str) -> String {
    format!("{organization_id}\0{request_id}")
}
