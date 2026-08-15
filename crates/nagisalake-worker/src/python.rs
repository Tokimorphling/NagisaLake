use crate::{Worker, WorkerConfig};
use pyo3::{
    exceptions::{PyRuntimeError, PyValueError},
    prelude::*,
    types::PyModule,
};
use std::{
    env,
    sync::{Arc, Mutex, Once, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
static INIT_LOGGING: Once = Once::new();

#[derive(Debug)]
enum EmbeddedWorkerState {
    Starting,
    Running,
    Stopped,
    Failed(String),
}

type SharedState = Arc<Mutex<EmbeddedWorkerState>>;
type SharedThread = Arc<Mutex<Option<JoinHandle<()>>>>;

/// A Nagisalake worker running on a dedicated Tokio runtime.
#[pyclass(module = "nagisalake_comfyui._nagisalake_worker")]
struct WorkerHandle {
    config_path: String,
    shutdown:    CancellationToken,
    state:       SharedState,
    thread:      SharedThread,
}

#[pymethods]
impl WorkerHandle {
    /// Requests shutdown and waits for the embedded Tokio runtime to exit.
    fn stop(&self, py: Python<'_>) -> PyResult<()> {
        self.shutdown.cancel();
        let thread = self
            .thread
            .lock()
            .map_err(|_| PyRuntimeError::new_err("worker thread lock is poisoned"))?
            .take();
        if let Some(thread) = thread
            && py.detach(move || thread.join()).is_err()
        {
            set_state(
                &self.state,
                EmbeddedWorkerState::Failed("worker thread panicked".into()),
            );
            return Err(PyRuntimeError::new_err("worker thread panicked"));
        }
        Ok(())
    }

    /// Returns `starting`, `running`, `stopped`, or `failed`.
    fn status(&self) -> String {
        match self.state.lock().as_deref() {
            Ok(EmbeddedWorkerState::Starting) => "starting",
            Ok(EmbeddedWorkerState::Running) => "running",
            Ok(EmbeddedWorkerState::Stopped) => "stopped",
            Ok(EmbeddedWorkerState::Failed(_)) | Err(_) => "failed",
        }
        .into()
    }

    /// Returns the last terminal error, if any.
    fn last_error(&self) -> Option<String> {
        match self.state.lock().as_deref() {
            Ok(EmbeddedWorkerState::Failed(message)) => Some(message.clone()),
            Ok(_) => None,
            Err(_) => Some("worker state lock is poisoned".into()),
        }
    }

    fn is_running(&self) -> bool {
        matches!(
            self.state.lock().as_deref(),
            Ok(EmbeddedWorkerState::Starting | EmbeddedWorkerState::Running)
        )
    }

    #[getter]
    fn config_path(&self) -> &str {
        &self.config_path
    }

    fn __repr__(&self) -> String {
        format!(
            "WorkerHandle(config_path={:?}, status={:?})",
            self.config_path,
            self.status()
        )
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Starts the worker in a background thread owned by this Python process.
#[pyfunction]
#[pyo3(signature = (config_path=None))]
fn start_worker(py: Python<'_>, config_path: Option<String>) -> PyResult<WorkerHandle> {
    init_logging();
    let config_path = config_path
        .filter(|path| !path.trim().is_empty())
        .or_else(|| env::var("NAGISALAKE_WORKER_CONFIG").ok())
        .ok_or_else(|| {
            PyValueError::new_err(
                "config_path or NAGISALAKE_WORKER_CONFIG must point to worker TOML",
            )
        })?;
    let path_for_load = config_path.clone();
    let config = py
        .detach(move || WorkerConfig::load(path_for_load))
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    let shutdown = CancellationToken::new();
    let state = Arc::new(Mutex::new(EmbeddedWorkerState::Starting));
    let thread_slot = Arc::new(Mutex::new(None));
    let (ready_sender, ready_receiver) = mpsc::sync_channel::<Result<(), String>>(1);
    let thread_shutdown = shutdown.clone();
    let thread_state = Arc::clone(&state);
    info!(
        config_path = %config_path,
        namespace = %config.worker.namespace,
        node_name = %config.worker.node_name,
        hub_url = %config.hub.url,
        "starting embedded Nagisalake worker"
    );
    let thread = thread::Builder::new()
        .name("nagisalake-worker".into())
        .spawn(move || {
            run_embedded_worker(config, thread_shutdown, thread_state, ready_sender);
        })
        .map_err(|error| PyRuntimeError::new_err(format!("start worker thread: {error}")))?;
    *thread_slot
        .lock()
        .map_err(|_| PyRuntimeError::new_err("worker thread lock is poisoned"))? = Some(thread);

    let startup = py.detach(move || ready_receiver.recv_timeout(STARTUP_TIMEOUT));
    match startup {
        Ok(Ok(())) => {
            info!(config_path = %config_path, "embedded Nagisalake worker started");
            Ok(WorkerHandle {
                config_path,
                shutdown,
                state,
                thread: thread_slot,
            })
        }
        Ok(Err(message)) => {
            shutdown.cancel();
            warn!(error = %message, "embedded Nagisalake worker failed during startup");
            Err(PyRuntimeError::new_err(message))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            shutdown.cancel();
            warn!(
                timeout_seconds = STARTUP_TIMEOUT.as_secs(),
                "embedded Nagisalake worker initialization timed out"
            );
            Err(PyRuntimeError::new_err(
                "worker initialization timed out after 30 seconds",
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            shutdown.cancel();
            warn!("embedded Nagisalake worker thread exited before initialization completed");
            Err(PyRuntimeError::new_err(
                "worker thread exited before initialization completed",
            ))
        }
    }
}

fn init_logging() {
    INIT_LOGGING.call_once(|| {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .try_init();
    });
}

fn run_embedded_worker(
    config: WorkerConfig,
    shutdown: CancellationToken,
    state: SharedState,
    ready: mpsc::SyncSender<Result<(), String>>,
) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let message = format!("build Tokio runtime: {error}");
            warn!(error = %message, "embedded Nagisalake worker startup failed");
            fail_startup(&state, &ready, message);
            return;
        }
    };
    let worker = match runtime.block_on(Worker::from_config(config)) {
        Ok(worker) => worker,
        Err(error) => {
            warn!(?error, "embedded Nagisalake worker startup failed");
            fail_startup(&state, &ready, error.to_string());
            return;
        }
    };
    set_state(&state, EmbeddedWorkerState::Running);
    if ready.send(Ok(())).is_err() {
        warn!("embedded Nagisalake worker start was abandoned before readiness ack");
        shutdown.cancel();
    }
    match runtime.block_on(worker.run_until_cancelled(shutdown)) {
        Ok(()) => {
            info!("embedded Nagisalake worker stopped");
            set_state(&state, EmbeddedWorkerState::Stopped);
        }
        Err(error) => {
            warn!(?error, "embedded Nagisalake worker stopped with error");
            set_state(&state, EmbeddedWorkerState::Failed(error.to_string()));
        }
    }
}

fn fail_startup(
    state: &SharedState,
    ready: &mpsc::SyncSender<Result<(), String>>,
    message: String,
) {
    set_state(state, EmbeddedWorkerState::Failed(message.clone()));
    let _ = ready.send(Err(message));
}

fn set_state(state: &SharedState, value: EmbeddedWorkerState) {
    if let Ok(mut state) = state.lock() {
        *state = value;
    }
}

#[pyfunction]
fn worker_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[pymodule]
fn _nagisalake_worker(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<WorkerHandle>()?;
    module.add_function(wrap_pyfunction!(start_worker, module)?)?;
    module.add_function(wrap_pyfunction!(worker_version, module)?)?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
