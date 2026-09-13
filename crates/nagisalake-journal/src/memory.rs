//! In-memory journal with the same transition/replay contract as SQLite.
use crate::{
    JournalError,
    state::{same_dispatch_identity, validate_event_sequence},
};
use nagisalake_core::{
    ClearPendingEvent, GetJob, JobRecord, ListUnfinished, SetJobState, SetPendingEvent,
    SetPromptId, UpsertDispatch,
};
use service_async::Service;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Default)]
pub struct MemoryJournal {
    records: Arc<RwLock<BTreeMap<String, JobRecord>>>,
}

impl Service<UpsertDispatch> for MemoryJournal {
    type Response = JobRecord;
    type Error = JournalError;

    async fn call(&self, request: UpsertDispatch) -> Result<Self::Response, Self::Error> {
        let mut records = self.records.write().await;
        if let Some(record) = records.get_mut(&request.0.job_id) {
            if !same_dispatch_identity(&record.dispatch, &request.0) {
                return Err(JournalError::DispatchConflict(request.0.job_id));
            }
            if !record.state.is_terminal() {
                record.dispatch = request.0;
            }
            return Ok(record.clone());
        }
        let record = JobRecord::received(request.0);
        records.insert(record.dispatch.job_id.clone(), record.clone());
        Ok(record)
    }
}

impl Service<GetJob> for MemoryJournal {
    type Response = Option<JobRecord>;
    type Error = JournalError;

    async fn call(&self, request: GetJob) -> Result<Self::Response, Self::Error> {
        Ok(self.records.read().await.get(&request.0).cloned())
    }
}

impl Service<ListUnfinished> for MemoryJournal {
    type Response = Vec<JobRecord>;
    type Error = JournalError;

    async fn call(&self, _request: ListUnfinished) -> Result<Self::Response, Self::Error> {
        Ok(self
            .records
            .read()
            .await
            .values()
            .filter(|record| !record.state.is_terminal() || record.pending_event.is_some())
            .cloned()
            .collect())
    }
}

impl Service<SetJobState> for MemoryJournal {
    type Response = ();
    type Error = JournalError;

    async fn call(&self, request: SetJobState) -> Result<Self::Response, Self::Error> {
        let mut records = self.records.write().await;
        let record = records
            .get_mut(&request.job_id)
            .ok_or_else(|| JournalError::NotFound(request.job_id.clone()))?;
        if !record.state.can_transition_to(request.state) {
            return Err(JournalError::InvalidTransition {
                current: record.state,
                next:    request.state,
            });
        }
        record.state = request.state;
        Ok(())
    }
}

impl Service<SetPromptId> for MemoryJournal {
    type Response = ();
    type Error = JournalError;

    async fn call(&self, request: SetPromptId) -> Result<Self::Response, Self::Error> {
        self.records
            .write()
            .await
            .get_mut(&request.job_id)
            .ok_or_else(|| JournalError::NotFound(request.job_id.clone()))?
            .prompt_id = Some(request.prompt_id);
        Ok(())
    }
}

impl Service<SetPendingEvent> for MemoryJournal {
    type Response = ();
    type Error = JournalError;

    async fn call(&self, request: SetPendingEvent) -> Result<Self::Response, Self::Error> {
        let mut records = self.records.write().await;
        let record = records
            .get_mut(&request.event.job_id)
            .ok_or_else(|| JournalError::NotFound(request.event.job_id.clone()))?;
        validate_event_sequence(
            record.event_sequence,
            record.pending_event.as_ref(),
            &request.event,
        )?;
        if let Some(state) = request.state {
            if !record.state.can_transition_to(state) {
                return Err(JournalError::InvalidTransition {
                    current: record.state,
                    next:    state,
                });
            }
            record.state = state;
        }
        record.event_sequence = request.event.sequence;
        record.pending_event = Some(request.event);
        Ok(())
    }
}

impl Service<ClearPendingEvent> for MemoryJournal {
    type Response = ();
    type Error = JournalError;

    async fn call(&self, request: ClearPendingEvent) -> Result<Self::Response, Self::Error> {
        let mut records = self.records.write().await;
        if let Some(record) = records.get_mut(&request.job_id)
            && record.event_sequence == request.sequence
        {
            record.pending_event = None;
        }
        Ok(())
    }
}
