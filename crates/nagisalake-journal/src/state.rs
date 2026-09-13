//! Shared state-machine and replay invariants.
use crate::JournalError;
use nagisalake_core::JobState;
use nagisalake_protocol::DispatchJob;

/// Takes the two fields it reads rather than a record type, so the durable and
/// in-memory journals share one implementation without the SQLite path having
/// to materialize a full [`nagisalake_core::JobRecord`].
pub(super) fn validate_event_sequence(
    current_sequence: u64,
    pending_event: Option<&nagisalake_protocol::JobEvent>,
    event: &nagisalake_protocol::JobEvent,
) -> Result<(), JournalError> {
    if event.sequence < current_sequence
        || (event.sequence == current_sequence && pending_event != Some(event))
    {
        Err(JournalError::StaleEventSequence {
            current:  current_sequence,
            received: event.sequence,
        })
    } else {
        Ok(())
    }
}

pub(super) fn same_dispatch_identity(previous: &DispatchJob, next: &DispatchJob) -> bool {
    previous.job_id == next.job_id
        && previous.attempt == next.attempt
        && previous.workflow_id == next.workflow_id
        && previous.workflow_version == next.workflow_version
        && previous.parameters == next.parameters
        && previous.inputs.len() == next.inputs.len()
        && previous
            .inputs
            .iter()
            .zip(&next.inputs)
            .all(|(left, right)| {
                left.artifact_id == right.artifact_id
                    && left.name == right.name
                    && left.content_type == right.content_type
                    && left.size_bytes == right.size_bytes
                    && left.sha256.eq_ignore_ascii_case(&right.sha256)
            })
}

pub(super) const fn state_str(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Received => "received",
        JobState::Accepted => "accepted",
        JobState::Running => "running",
        JobState::Uploading => "uploading",
        JobState::Completed => "completed",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

pub(super) fn parse_state(value: &str) -> Result<JobState, JournalError> {
    match value {
        "queued" => Ok(JobState::Queued),
        "received" => Ok(JobState::Received),
        "accepted" => Ok(JobState::Accepted),
        "running" => Ok(JobState::Running),
        "uploading" => Ok(JobState::Uploading),
        "completed" => Ok(JobState::Completed),
        "failed" => Ok(JobState::Failed),
        "cancelled" => Ok(JobState::Cancelled),
        other => Err(JournalError::InvalidState(other.into())),
    }
}
