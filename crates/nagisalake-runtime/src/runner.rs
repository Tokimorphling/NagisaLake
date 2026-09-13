//! ComfyUI job state machine and execution configuration.
use crate::{
    JobSlot, RuntimeError, WorkerRuntime,
    artifacts::{
        cleanup_job_dir, download_to_file, safe_filename, stream_response_to_file,
        upload_file_with_retry,
    },
    error::http_failure,
    util::{now_unix_ms, truncate},
};
use nagisalake_comfyui::{ComfyUiService, PollUntilCompleteService, WaitForCompletion};
use nagisalake_core::{
    ComfyPromptRequest, ComfyPromptStatus, ComfyUploadImageRequest, ComfyViewRequest, JobRecord,
    JobState, OutputRef, SetPromptId,
};
use nagisalake_journal::SqliteJournal;
use nagisalake_protocol::{ArtifactReady, ArtifactUploaded, DispatchJob, JobEvent, JobEventKind};
use nagisalake_workflow::{RenderWorkflow, WorkflowService};
use reqwest::Client;
use service_async::Service;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    fs,
    sync::{Semaphore, watch},
    time::Instant,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

const COMFY_STATUS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// Runtime configuration for local job execution.
#[derive(Debug, Clone)]
pub struct WorkerExecutionConfig {
    pub work_dir:         PathBuf,
    pub poll_interval:    Duration,
    pub max_output_bytes: u64,
    pub parallelism:      usize,
}

impl WorkerExecutionConfig {
    /// Validates limits before starting a worker.
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.work_dir.as_os_str().is_empty() {
            return Err(RuntimeError::InvalidConfig(
                "work_dir must not be empty".into(),
            ));
        }
        if self.poll_interval.is_zero() {
            return Err(RuntimeError::InvalidConfig(
                "poll_interval must be greater than zero".into(),
            ));
        }
        if self.max_output_bytes == 0 {
            return Err(RuntimeError::InvalidConfig(
                "max_output_bytes must be greater than zero".into(),
            ));
        }
        if self.parallelism == 0 {
            return Err(RuntimeError::InvalidConfig(
                "parallelism must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

/// Executes dispatches against one local ComfyUI instance.
#[derive(Clone)]
pub struct JobRunner {
    config:        WorkerExecutionConfig,
    workflows:     Arc<WorkflowService>,
    comfy:         PollUntilCompleteService<ComfyUiService>,
    journal:       SqliteJournal,
    runtime:       WorkerRuntime,
    client:        Client,
    /// Uploads can legitimately spend longer than the ordinary 60-second read
    /// timeout sending a large body before R2 returns its response headers.
    /// Each PUT is instead bounded by the presigned request's own expiry.
    upload_client: Client,
    slots:         Arc<Semaphore>,
}

struct EventUpdate {
    kind:      JobEventKind,
    progress:  Option<f32>,
    prompt_id: Option<String>,
    message:   String,
    state:     JobState,
}

impl JobRunner {
    /// Builds a runner from the already validated service stack.
    pub fn new(
        config: WorkerExecutionConfig,
        workflows: Arc<WorkflowService>,
        comfy: PollUntilCompleteService<ComfyUiService>,
        journal: SqliteJournal,
        runtime: WorkerRuntime,
    ) -> Result<Self, RuntimeError> {
        config.validate()?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|error| http_failure("artifact_client_build", 1, error))?;
        let upload_client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| http_failure("artifact_upload_client_build", 1, error))?;
        Ok(Self {
            slots: Arc::new(Semaphore::new(config.parallelism)),
            config,
            workflows,
            comfy,
            journal,
            runtime,
            client,
            upload_client,
        })
    }

    /// Runs one journal record until a terminal state is emitted.
    ///
    /// `slot` carries this job's capacity charge and is released when this
    /// function returns, on every path.
    pub async fn execute(
        &self,
        mut record: JobRecord,
        cancellation: CancellationToken,
        mut slot: JobSlot,
    ) {
        let job_id = record.dispatch.job_id.clone();
        let attempt = record.dispatch.attempt;
        let mut sequence = record.event_sequence;
        let result = self
            .execute_inner(&mut record, &mut sequence, cancellation.clone(), &mut slot)
            .await;
        if let Err(error) = result {
            if cancellation.is_cancelled() && record.state != JobState::Uploading {
                let _ = self
                    .emit(&job_id, attempt, &mut sequence, EventUpdate {
                        kind:      JobEventKind::Cancelled,
                        progress:  None,
                        prompt_id: record.prompt_id.clone(),
                        message:   "job cancelled".into(),
                        state:     JobState::Cancelled,
                    })
                    .await;
            } else {
                warn!(%job_id, ?error, "ComfyUI job failed");
                let _ = self
                    .emit(&job_id, attempt, &mut sequence, EventUpdate {
                        kind:      JobEventKind::Failed,
                        progress:  None,
                        prompt_id: record.prompt_id.clone(),
                        message:   truncate(&error.to_string(), 1_000),
                        state:     JobState::Failed,
                    })
                    .await;
            }
        }
        self.runtime.finish_job(&job_id).await;
    }

    async fn execute_inner(
        &self,
        record: &mut JobRecord,
        sequence: &mut u64,
        cancellation: CancellationToken,
        slot: &mut JobSlot,
    ) -> Result<(), RuntimeError> {
        let dispatch = record.dispatch.clone();
        if record.state.is_terminal() {
            return Ok(());
        }
        if let Some(pending) = record.pending_event.clone() {
            self.runtime
                .send_job_event(&self.journal, pending, None)
                .await?;
        }
        if record.state == JobState::Received {
            self.emit(&dispatch.job_id, dispatch.attempt, sequence, EventUpdate {
                kind:      JobEventKind::Accepted,
                progress:  None,
                prompt_id: None,
                message:   String::new(),
                state:     JobState::Accepted,
            })
            .await?;
            record.state = JobState::Accepted;
        }
        let permit = tokio::select! {
            permit = self.slots.clone().acquire_owned() => permit.map_err(|_| RuntimeError::ConnectionClosed)?,
            // Returning here keeps the slot charged as queued; the caller's
            // `JobSlot` drop releases it.
            _ = cancellation.cancelled() => return Err(RuntimeError::Cancelled),
        };
        slot.activate();
        let _permit = permit;
        if cancellation.is_cancelled() {
            return Err(RuntimeError::Cancelled);
        }

        let prompt_id = if let Some(prompt_id) = record.prompt_id.clone() {
            prompt_id
        } else {
            let input_names = self.prepare_inputs(&dispatch, &cancellation).await?;
            let workflow = self
                .workflows
                .call(RenderWorkflow {
                    dispatch: dispatch.clone(),
                    input_names,
                })
                .await
                .map_err(|error| RuntimeError::Workflow(error.to_string()))?;
            if cancellation.is_cancelled() {
                return Err(RuntimeError::Cancelled);
            }
            let response = self
                .comfy
                .call(ComfyPromptRequest {
                    job_id: dispatch.job_id.clone(),
                    client_id: format!("nagisalake-{}", dispatch.workflow_id),
                    workflow,
                })
                .await
                .map_err(|error| RuntimeError::Comfy(error.to_string()))?;
            Service::<SetPromptId>::call(&self.journal, SetPromptId {
                job_id:    dispatch.job_id.clone(),
                prompt_id: response.prompt_id.clone(),
            })
            .await
            .map_err(|error| RuntimeError::Journal(error.to_string()))?;
            response.prompt_id
        };

        if record.state != JobState::Running && record.state != JobState::Uploading {
            self.emit(&dispatch.job_id, dispatch.attempt, sequence, EventUpdate {
                kind:      JobEventKind::Running,
                progress:  None,
                prompt_id: Some(prompt_id.clone()),
                message:   String::new(),
                state:     JobState::Running,
            })
            .await?;
            record.state = JobState::Running;
        }
        if cancellation.is_cancelled() {
            self.cancel_prompt(&prompt_id).await;
            return Err(RuntimeError::Cancelled);
        }
        let (status_tx, mut status_rx) = watch::channel(ComfyPromptStatus::Unknown);
        let wait = self.comfy.call(WaitForCompletion {
            prompt_id: prompt_id.clone(),
            cancellation: cancellation.clone(),
            status_tx,
        });
        tokio::pin!(wait);
        let mut last_status = ComfyPromptStatus::Unknown;
        let mut status_since = Instant::now();
        let mut last_status_event = None;
        let mut status_channel_open = true;
        let outputs = loop {
            tokio::select! {
                result = &mut wait => {
                    match result {
                        Ok(outputs) => break outputs,
                        Err(error) => {
                            // If the wait was cancelled and we have a prompt_id,
                            // best-effort remove the prompt from ComfyUI's queue.
                            // Without this the engine keeps running a prompt the
                            // user asked to stop.
                            if cancellation.is_cancelled() {
                                self.cancel_prompt(&prompt_id).await;
                            }
                            return Err(RuntimeError::Comfy(error.to_string()));
                        }
                    }
                }
                changed = status_rx.changed(), if status_channel_open => {
                    if changed.is_err() {
                        status_channel_open = false;
                        continue;
                    }
                    let status = *status_rx.borrow_and_update();
                    if status == ComfyPromptStatus::Unknown {
                        continue;
                    }
                    let now = Instant::now();
                    let changed = status != last_status;
                    if changed {
                        last_status = status;
                        status_since = now;
                    }
                    let due = last_status_event.is_none_or(|last: Instant| {
                        now.duration_since(last) >= COMFY_STATUS_HEARTBEAT_INTERVAL
                    });
                    if changed || due {
                        self.emit(&dispatch.job_id, dispatch.attempt, sequence, EventUpdate {
                            kind:      JobEventKind::Progress,
                            progress:  None,
                            prompt_id: Some(prompt_id.clone()),
                            message:   comfy_status_message(
                                status,
                                now.duration_since(status_since),
                            ),
                            state:     JobState::Running,
                        }).await?;
                        last_status_event = Some(Instant::now());
                    }
                }
            }
        };
        self.emit(&dispatch.job_id, dispatch.attempt, sequence, EventUpdate {
            kind:      JobEventKind::Uploading,
            progress:  Some(0.95),
            prompt_id: Some(prompt_id.clone()),
            message:   String::new(),
            state:     JobState::Uploading,
        })
        .await?;
        record.state = JobState::Uploading;
        let output_dir = self.config.work_dir.join(&dispatch.job_id).join("outputs");
        fs::create_dir_all(&output_dir).await?;
        for (index, output) in outputs.iter().enumerate() {
            self.upload_output(&dispatch, index, output, &output_dir, &cancellation)
                .await?;
        }
        self.emit(&dispatch.job_id, dispatch.attempt, sequence, EventUpdate {
            kind:      JobEventKind::Completed,
            progress:  Some(1.0),
            prompt_id: Some(prompt_id),
            message:   String::new(),
            state:     JobState::Completed,
        })
        .await?;
        cleanup_job_dir(&self.config.work_dir, &dispatch.job_id).await;
        info!(job_id = %dispatch.job_id, "ComfyUI job completed");
        Ok(())
    }

    async fn emit(
        &self,
        job_id: &str,
        attempt: u32,
        sequence: &mut u64,
        update: EventUpdate,
    ) -> Result<(), RuntimeError> {
        *sequence = sequence.saturating_add(1);
        self.runtime
            .send_job_event(
                &self.journal,
                JobEvent {
                    job_id: job_id.into(),
                    attempt,
                    sequence: *sequence,
                    kind: update.kind,
                    progress: update.progress,
                    prompt_id: update.prompt_id,
                    message: update.message,
                    unix_ms: now_unix_ms(),
                },
                Some(update.state),
            )
            .await
    }

    async fn prepare_inputs(
        &self,
        dispatch: &DispatchJob,
        cancellation: &CancellationToken,
    ) -> Result<Vec<String>, RuntimeError> {
        let input_dir = self.config.work_dir.join(&dispatch.job_id).join("inputs");
        fs::create_dir_all(&input_dir).await?;
        let mut names = Vec::with_capacity(dispatch.inputs.len());
        for (index, input) in dispatch.inputs.iter().enumerate() {
            if cancellation.is_cancelled() {
                return Err(RuntimeError::Cancelled);
            }
            // Job-scoped upload name prevents two concurrent jobs with the same
            // original filename from clobbering each other via overwrite=true.
            let local_name = format!(
                "{:03}-{}-{}",
                index,
                safe_filename(&dispatch.job_id),
                safe_filename(&input.name),
            );
            let path = input_dir.join(&local_name);
            download_to_file(
                &self.client,
                &input.download,
                &path,
                input.size_bytes,
                &input.sha256,
            )
            .await
            .map_err(|error| RuntimeError::Artifact(format!("{}: {error}", input.artifact_id)))?;
            let response = self
                .comfy
                .call(ComfyUploadImageRequest {
                    path,
                    file_name: local_name,
                })
                .await
                .map_err(|error| RuntimeError::Comfy(error.to_string()))?;
            names.push(response.name);
        }
        Ok(names)
    }

    async fn upload_output(
        &self,
        dispatch: &DispatchJob,
        index: usize,
        output: &OutputRef,
        output_dir: &std::path::Path,
        cancellation: &CancellationToken,
    ) -> Result<(), RuntimeError> {
        let path = output_dir.join(format!("{index:03}-{}", safe_filename(&output.filename)));
        let response = self
            .comfy
            .call(ComfyViewRequest {
                output: output.clone(),
            })
            .await
            .map_err(|error| RuntimeError::Comfy(error.to_string()))?;
        let (size_bytes, sha256) = stream_response_to_file(
            response,
            &path,
            self.config.max_output_bytes,
            "comfy_output_body",
        )
        .await?;
        let request_id = Uuid::new_v4().to_string();
        let ready = ArtifactReady {
            request_id: request_id.clone(),
            job_id: dispatch.job_id.clone(),
            attempt: dispatch.attempt,
            name: safe_filename(&output.filename),
            content_type: output.content_type.clone(),
            size_bytes,
            sha256,
        };
        let artifact_id = upload_file_with_retry(
            &self.upload_client,
            &self.runtime,
            &ready,
            &path,
            cancellation,
        )
        .await?;
        self.runtime
            .confirm_artifact(ArtifactUploaded {
                request_id,
                artifact_id,
                job_id: dispatch.job_id.clone(),
                attempt: dispatch.attempt,
            })
            .await?;
        let _ = fs::remove_file(path).await;
        Ok(())
    }

    async fn cancel_prompt(&self, prompt_id: &str) {
        let result = self
            .comfy
            .call(nagisalake_core::ComfyQueueDeleteRequest {
                prompt_id: prompt_id.into(),
            })
            .await;
        if let Err(error) = result {
            warn!(%prompt_id, ?error, "failed to remove ComfyUI queue item");
        }
    }
}

fn comfy_status_message(status: ComfyPromptStatus, elapsed: Duration) -> String {
    let elapsed = elapsed.as_secs();
    match status {
        ComfyPromptStatus::Unknown => {
            format!("ComfyUI prompt status is unavailable (elapsed {elapsed}s)")
        }
        ComfyPromptStatus::Queued { position } => {
            format!("ComfyUI queued this prompt at position {position} (elapsed {elapsed}s)")
        }
        ComfyPromptStatus::Running => {
            format!("ComfyUI is running this prompt (elapsed {elapsed}s)")
        }
    }
}
