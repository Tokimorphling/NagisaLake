use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactView {
    pub id:           String,
    pub job_id:       Option<String>,
    pub name:         String,
    pub content_type: String,
    pub size_bytes:   u64,
    pub sha256:       String,
    pub state:        ArtifactState,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactState {
    PendingUpload,
    Ready,
}

#[derive(Debug, Clone)]
pub(super) struct ArtifactRecord {
    pub(super) organization_id: String,
    pub(super) view:            ArtifactView,
    pub(super) object_key:      String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct JobEventView {
    pub(super) sequence: u64,
    pub(super) kind:     JobEventKind,
    pub(super) progress: Option<f32>,
    pub(super) message:  String,
    pub(super) unix_ms:  i64,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct JobView {
    pub(super) id:                  String,
    pub(super) workflow_id:         String,
    pub(super) workflow_version:    String,
    pub(super) parameters:          JsonValue,
    pub(super) input_artifact_ids:  Vec<String>,
    pub(super) output_artifact_ids: Vec<String>,
    pub(super) worker_id:           String,
    pub(super) session_id:          String,
    pub(super) state:               JobState,
    pub(super) progress:            Option<f32>,
    pub(super) prompt_id:           Option<String>,
    pub(super) error:               Option<String>,
    pub(super) events:              Vec<JobEventView>,
    pub(super) created_at_unix_ms:  i64,
    pub(super) updated_at_unix_ms:  i64,
}

/// One row of the job list.
///
/// Deliberately omits `events` rather than sending an empty vector: a list of
/// 100k jobs carried 500k inlined events and made a single `GET /jobs` response
/// 120 MiB, while the list only renders state and progress. An empty vector
/// would also read as "this job has no events", which is a different claim.
/// Fetch the job itself for its timeline.
#[derive(Debug, Clone, Serialize)]
pub(super) struct JobSummary {
    pub(super) id:                  String,
    pub(super) workflow_id:         String,
    pub(super) workflow_version:    String,
    pub(super) parameters:          JsonValue,
    pub(super) input_artifact_ids:  Vec<String>,
    pub(super) output_artifact_ids: Vec<String>,
    pub(super) worker_id:           String,
    pub(super) session_id:          String,
    pub(super) state:               JobState,
    pub(super) progress:            Option<f32>,
    pub(super) prompt_id:           Option<String>,
    pub(super) error:               Option<String>,
    pub(super) created_at_unix_ms:  i64,
    pub(super) updated_at_unix_ms:  i64,
}

impl From<&JobView> for JobSummary {
    fn from(view: &JobView) -> Self {
        Self {
            id:                  view.id.clone(),
            workflow_id:         view.workflow_id.clone(),
            workflow_version:    view.workflow_version.clone(),
            parameters:          view.parameters.clone(),
            input_artifact_ids:  view.input_artifact_ids.clone(),
            output_artifact_ids: view.output_artifact_ids.clone(),
            worker_id:           view.worker_id.clone(),
            session_id:          view.session_id.clone(),
            state:               view.state,
            progress:            view.progress,
            prompt_id:           view.prompt_id.clone(),
            error:               view.error.clone(),
            created_at_unix_ms:  view.created_at_unix_ms,
            updated_at_unix_ms:  view.updated_at_unix_ms,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct JobRecord {
    pub(super) organization_id:        String,
    pub(super) actor_id:               String,
    pub(super) actor_kind:             String,
    pub(super) actor_user_id:          Option<String>,
    pub(super) worker_organization_id: String,
    pub(super) view:                   JobView,
    pub(super) dispatch:               DispatchJob,
    pub(super) last_event:             u64,
}

const READ_CACHE_TTL: Duration = Duration::from_secs(10);
const READ_CACHE_CAPACITY: usize = 1_024;
const DEVICE_ACCESS_CACHE_TTL: Duration = Duration::from_secs(2);
const DEVICE_ACCESS_CACHE_CAPACITY: usize = 2_048;

#[derive(Debug)]
struct ReadCacheEntry<T> {
    value:      T,
    expires_at: Instant,
}

#[derive(Debug, Default)]
struct ReadCache {
    terminal_jobs:   HashMap<String, ReadCacheEntry<JobView>>,
    ready_artifacts: HashMap<String, ReadCacheEntry<ArtifactRecord>>,
    device_access:   HashMap<String, ReadCacheEntry<Vec<nagisalake_hub_store::DeviceAccess>>>,
}

impl ReadCache {
    fn get_job(&mut self, key: &str) -> Option<JobView> {
        let now = Instant::now();
        let entry = self.terminal_jobs.get(key)?;
        if entry.expires_at <= now {
            self.terminal_jobs.remove(key);
            return None;
        }
        Some(entry.value.clone())
    }

    fn insert_job(&mut self, key: String, value: JobView) {
        let now = Instant::now();
        self.terminal_jobs.retain(|_, entry| entry.expires_at > now);
        if self.terminal_jobs.len() >= READ_CACHE_CAPACITY
            && !self.terminal_jobs.contains_key(&key)
            && let Some(oldest) = self
                .terminal_jobs
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(key, _)| key.clone())
        {
            self.terminal_jobs.remove(&oldest);
        }
        self.terminal_jobs.insert(key, ReadCacheEntry {
            value,
            expires_at: now + READ_CACHE_TTL,
        });
    }

    fn get_artifact(&mut self, key: &str) -> Option<ArtifactRecord> {
        let now = Instant::now();
        let entry = self.ready_artifacts.get(key)?;
        if entry.expires_at <= now {
            self.ready_artifacts.remove(key);
            return None;
        }
        Some(entry.value.clone())
    }

    fn insert_artifact(&mut self, key: String, value: ArtifactRecord) {
        let now = Instant::now();
        self.ready_artifacts
            .retain(|_, entry| entry.expires_at > now);
        if self.ready_artifacts.len() >= READ_CACHE_CAPACITY
            && !self.ready_artifacts.contains_key(&key)
            && let Some(oldest) = self
                .ready_artifacts
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(key, _)| key.clone())
        {
            self.ready_artifacts.remove(&oldest);
        }
        self.ready_artifacts.insert(key, ReadCacheEntry {
            value,
            expires_at: now + READ_CACHE_TTL,
        });
    }

    fn remove_artifact(&mut self, key: &str) {
        self.ready_artifacts.remove(key);
    }

    fn get_device_access(&mut self, key: &str) -> Option<Vec<nagisalake_hub_store::DeviceAccess>> {
        let now = Instant::now();
        let entry = self.device_access.get(key)?;
        if entry.expires_at <= now {
            self.device_access.remove(key);
            return None;
        }
        Some(entry.value.clone())
    }

    fn insert_device_access(
        &mut self,
        key: String,
        value: Vec<nagisalake_hub_store::DeviceAccess>,
    ) {
        let now = Instant::now();
        self.device_access.retain(|_, entry| entry.expires_at > now);
        if self.device_access.len() >= DEVICE_ACCESS_CACHE_CAPACITY
            && !self.device_access.contains_key(&key)
            && let Some(oldest) = self
                .device_access
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(key, _)| key.clone())
        {
            self.device_access.remove(&oldest);
        }
        self.device_access.insert(key, ReadCacheEntry {
            value,
            expires_at: now + DEVICE_ACCESS_CACHE_TTL,
        });
    }

    fn remove_device_access_for_user(&mut self, user_id: &str) {
        let suffix = format!("\0{user_id}");
        self.device_access.retain(|key, _| !key.ends_with(&suffix));
    }

    fn remove_device_access_for_organization(&mut self, organization_id: &str) {
        let prefix = format!("{organization_id}\0");
        self.device_access
            .retain(|key, _| !key.starts_with(&prefix));
    }
}

/// Single-instance serialization for quota mutations. PostgreSQL remains the
/// durable authority, while requests for one tenant are serialized before
/// they can pile up on the same database row lock.
#[derive(Clone, Default)]
pub(super) struct QuotaGate {
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl QuotaGate {
    async fn acquire(&self, organization_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.locks.lock().await;
            // Drop idle entries so a long-lived single-instance Hub does not
            // retain one mutex forever for every organization ever observed.
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            locks
                .entry(organization_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }
}

/// Scheduling state held in memory.
///
/// PostgreSQL is authoritative — writes land there first — so this is a cache,
/// and it only needs to hold what dispatch decisions read. It is deliberately
/// **not** a full mirror: hydrating every job and event made startup scale with
/// total history (218 s and 558 MB resident at 100k jobs) even though terminal
/// jobs carry no scheduling value.
///
/// Anything not resident is fetched on demand. Read paths that can hit a
/// terminal job or a completed artifact must therefore fall back to the store;
/// see [`job_for_principal`] and [`artifact_record`]. Those two read-through
/// paths have a separate bounded TTL cache, so repeated history/media reads do
/// not turn into an unbounded in-memory mirror.
#[derive(Debug, Default)]
pub(super) struct HubData {
    /// Only `pending_upload`; `ready` artifacts are read from the store.
    pub(super) artifacts:       HashMap<String, ArtifactRecord>,
    pub(super) pending_uploads: HashMap<String, String>,
    /// Only non-terminal jobs.
    pub(super) jobs:            HashMap<String, JobRecord>,
    pub(super) idempotency:     HashMap<String, String>,
}

impl HubData {
    pub(super) fn remove_organization(&mut self, organization_id: &str) {
        self.artifacts
            .retain(|_, artifact| artifact.organization_id != organization_id);
        self.pending_uploads
            .retain(|key, _| !key.starts_with(&format!("{organization_id}\0")));
        self.jobs
            .retain(|_, job| job.organization_id != organization_id);
        self.idempotency
            .retain(|key, _| !key.starts_with(&format!("{organization_id}\0")));
    }
}

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub(super) config:          Arc<HubConfig>,
    pub(super) sessions:        SessionRegistry,
    pub(super) data:            Arc<RwLock<HubData>>,
    read_cache:                 Arc<RwLock<ReadCache>>,
    pub(super) quota_gate:      QuotaGate,
    pub(super) objects:         ObjectStore,
    pub(super) store:           Option<PgStore>,
    /// Resolved at startup, so a missing secret fails there rather than on a
    /// user's first click.
    pub(super) oauth_providers: Arc<BTreeMap<String, crate::oauth::Provider>>,
    /// Shared client for provider token and userinfo calls.
    pub(super) http_client:     reqwest::Client,
    pub(super) rate_limiter:    crate::ratelimit::RateLimiter,
    pub(super) metrics:         Arc<HubMetrics>,
}

#[derive(Debug, Default)]
pub(super) struct HubMetrics {
    pub(super) http: std::sync::Mutex<HttpMetrics>,
    pub(super) http_requests_in_flight: AtomicU64,
    pub(super) scheduler_passes_total: AtomicU64,
    pub(super) scheduler_claimed_jobs_total: AtomicU64,
    pub(super) scheduler_dispatched_jobs_total: AtomicU64,
    pub(super) scheduler_unassigned_jobs_total: AtomicU64,
    pub(super) scheduler_errors_total: AtomicU64,
    pub(super) scheduler_last_pass_duration_nanoseconds: AtomicU64,
    pub(super) scheduler_queue_depth: AtomicU64,
    pub(super) scheduler_queue_oldest_ready_lag_milliseconds: AtomicU64,
    pub(super) dispatch_outbox_passes_total: AtomicU64,
    pub(super) dispatch_outbox_claimed_total: AtomicU64,
    pub(super) dispatch_outbox_delivered_total: AtomicU64,
    pub(super) dispatch_outbox_errors_total: AtomicU64,
    pub(super) dispatch_outbox_last_pass_duration_nanoseconds: AtomicU64,
    pub(super) dispatch_outbox_pending_depth: AtomicU64,
    pub(super) dispatch_outbox_claimed_depth: AtomicU64,
    pub(super) dispatch_outbox_oldest_ready_lag_milliseconds: AtomicU64,
    pub(super) backlog_metrics_sample_errors_total: AtomicU64,
    pub(super) backlog_metrics_last_success_unix_seconds: AtomicU64,
    pub(super) expired_upload_reaper_runs_total: AtomicU64,
    pub(super) expired_upload_reaper_errors_total: AtomicU64,
    pub(super) expired_uploads_reclaimed_total: AtomicU64,
    pub(super) expired_upload_bytes_reclaimed_total: AtomicU64,
    pub(super) expired_upload_delete_errors_total: AtomicU64,
    pub(super) quota_reconcile_runs_total: AtomicU64,
    pub(super) quota_reconcile_errors_total: AtomicU64,
    pub(super) quota_reconcile_corrected_organizations_total: AtomicU64,
    pub(super) quota_reconcile_failed_jobs_total: AtomicU64,
    pub(super) read_cache_job_hits_total: AtomicU64,
    pub(super) read_cache_job_misses_total: AtomicU64,
    pub(super) read_cache_artifact_hits_total: AtomicU64,
    pub(super) read_cache_artifact_misses_total: AtomicU64,
    pub(super) read_cache_device_access_hits_total: AtomicU64,
    pub(super) read_cache_device_access_misses_total: AtomicU64,
}

impl HubMetrics {
    pub(super) fn counter(&self, counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    pub(super) fn render(&self, connected_workers: usize, store: Option<&PgStore>) -> String {
        let mut output = String::from(
            "# HELP nagisalake_connected_workers Current connected worker sessions.\n# TYPE \
             nagisalake_connected_workers gauge\n",
        );
        output.push_str(&format!(
            "nagisalake_connected_workers {connected_workers}\n"
        ));
        output.push_str(
            "# HELP nagisalake_http_requests_in_flight HTTP requests currently being handled \
             (excluding /metrics).\n# TYPE nagisalake_http_requests_in_flight gauge\n",
        );
        output.push_str(&format!(
            "nagisalake_http_requests_in_flight {}\n",
            self.counter(&self.http_requests_in_flight)
        ));

        if let Some(store) = store {
            let pool = store.pool();
            output.push_str(
                "# HELP nagisalake_database_pool_connections Current SQLx PostgreSQL pool \
                 connections by state.\n# TYPE nagisalake_database_pool_connections gauge\n",
            );
            let size = pool.size();
            let idle = u32::try_from(pool.num_idle()).unwrap_or(u32::MAX);
            output.push_str(&format!(
                "nagisalake_database_pool_connections{{state=\"idle\"}} \
                 {idle}\nnagisalake_database_pool_connections{{state=\"in_use\"}} {}\n",
                size.saturating_sub(idle)
            ));
            output.push_str(
                "# HELP nagisalake_database_pool_max_connections Configured SQLx PostgreSQL pool \
                 limit.\n# TYPE nagisalake_database_pool_max_connections gauge\n",
            );
            output.push_str(&format!(
                "nagisalake_database_pool_max_connections {}\n",
                pool.options().get_max_connections()
            ));
        }

        for (name, value) in [
            (
                "nagisalake_scheduler_passes_total",
                self.counter(&self.scheduler_passes_total),
            ),
            (
                "nagisalake_scheduler_claimed_jobs_total",
                self.counter(&self.scheduler_claimed_jobs_total),
            ),
            (
                "nagisalake_scheduler_dispatched_jobs_total",
                self.counter(&self.scheduler_dispatched_jobs_total),
            ),
            (
                "nagisalake_scheduler_unassigned_jobs_total",
                self.counter(&self.scheduler_unassigned_jobs_total),
            ),
            (
                "nagisalake_scheduler_errors_total",
                self.counter(&self.scheduler_errors_total),
            ),
            (
                "nagisalake_dispatch_outbox_passes_total",
                self.counter(&self.dispatch_outbox_passes_total),
            ),
            (
                "nagisalake_dispatch_outbox_claimed_total",
                self.counter(&self.dispatch_outbox_claimed_total),
            ),
            (
                "nagisalake_dispatch_outbox_delivered_total",
                self.counter(&self.dispatch_outbox_delivered_total),
            ),
            (
                "nagisalake_dispatch_outbox_errors_total",
                self.counter(&self.dispatch_outbox_errors_total),
            ),
            (
                "nagisalake_backlog_metrics_sample_errors_total",
                self.counter(&self.backlog_metrics_sample_errors_total),
            ),
            (
                "nagisalake_expired_upload_reaper_runs_total",
                self.counter(&self.expired_upload_reaper_runs_total),
            ),
            (
                "nagisalake_expired_upload_reaper_errors_total",
                self.counter(&self.expired_upload_reaper_errors_total),
            ),
            (
                "nagisalake_expired_uploads_reclaimed_total",
                self.counter(&self.expired_uploads_reclaimed_total),
            ),
            (
                "nagisalake_expired_upload_bytes_reclaimed_total",
                self.counter(&self.expired_upload_bytes_reclaimed_total),
            ),
            (
                "nagisalake_expired_upload_delete_errors_total",
                self.counter(&self.expired_upload_delete_errors_total),
            ),
            (
                "nagisalake_quota_reconcile_runs_total",
                self.counter(&self.quota_reconcile_runs_total),
            ),
            (
                "nagisalake_quota_reconcile_errors_total",
                self.counter(&self.quota_reconcile_errors_total),
            ),
            (
                "nagisalake_quota_reconcile_corrected_organizations_total",
                self.counter(&self.quota_reconcile_corrected_organizations_total),
            ),
            (
                "nagisalake_quota_reconcile_failed_jobs_total",
                self.counter(&self.quota_reconcile_failed_jobs_total),
            ),
            (
                "nagisalake_read_cache_job_hits_total",
                self.counter(&self.read_cache_job_hits_total),
            ),
            (
                "nagisalake_read_cache_job_misses_total",
                self.counter(&self.read_cache_job_misses_total),
            ),
            (
                "nagisalake_read_cache_artifact_hits_total",
                self.counter(&self.read_cache_artifact_hits_total),
            ),
            (
                "nagisalake_read_cache_artifact_misses_total",
                self.counter(&self.read_cache_artifact_misses_total),
            ),
            (
                "nagisalake_read_cache_device_access_hits_total",
                self.counter(&self.read_cache_device_access_hits_total),
            ),
            (
                "nagisalake_read_cache_device_access_misses_total",
                self.counter(&self.read_cache_device_access_misses_total),
            ),
        ] {
            output.push_str(&format!("# TYPE {name} counter\n{name} {value}\n"));
        }

        for (name, value) in [
            (
                "nagisalake_scheduler_last_pass_duration_seconds",
                self.counter(&self.scheduler_last_pass_duration_nanoseconds) as f64 / 1e9,
            ),
            (
                "nagisalake_scheduler_queue_depth",
                self.counter(&self.scheduler_queue_depth) as f64,
            ),
            (
                "nagisalake_scheduler_queue_oldest_ready_lag_seconds",
                self.counter(&self.scheduler_queue_oldest_ready_lag_milliseconds) as f64 / 1e3,
            ),
            (
                "nagisalake_dispatch_outbox_last_pass_duration_seconds",
                self.counter(&self.dispatch_outbox_last_pass_duration_nanoseconds) as f64 / 1e9,
            ),
            (
                "nagisalake_dispatch_outbox_pending_depth",
                self.counter(&self.dispatch_outbox_pending_depth) as f64,
            ),
            (
                "nagisalake_dispatch_outbox_claimed_depth",
                self.counter(&self.dispatch_outbox_claimed_depth) as f64,
            ),
            (
                "nagisalake_dispatch_outbox_oldest_ready_lag_seconds",
                self.counter(&self.dispatch_outbox_oldest_ready_lag_milliseconds) as f64 / 1e3,
            ),
            (
                "nagisalake_backlog_metrics_last_success_unixtime_seconds",
                self.counter(&self.backlog_metrics_last_success_unix_seconds) as f64,
            ),
        ] {
            output.push_str(&format!("# TYPE {name} gauge\n{name} {value}\n"));
        }

        let http = self
            .http
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        output.push_str(
            "# HELP nagisalake_http_requests_total Completed HTTP requests by method, route \
             template, and status family.\n# TYPE nagisalake_http_requests_total counter\n",
        );
        for (key, value) in &http.requests {
            output.push_str(&format!(
                "nagisalake_http_requests_total{{method=\"{}\",route=\"{}\",status_family=\"{}\"\
                 }} {value}\n",
                key.method,
                escape_prometheus_label(&key.route),
                key.status_family,
            ));
        }
        output.push_str(
            "# HELP nagisalake_http_request_duration_seconds HTTP request handling duration by \
             method and route template.\n# TYPE nagisalake_http_request_duration_seconds \
             histogram\n",
        );
        for (key, histogram) in &http.durations {
            let route = escape_prometheus_label(&key.route);
            let mut cumulative = 0_u64;
            for (upper_bound, count) in HTTP_DURATION_BUCKETS_SECONDS.iter().zip(histogram.buckets)
            {
                cumulative = cumulative.saturating_add(count);
                output.push_str(&format!(
                    "nagisalake_http_request_duration_seconds_bucket{{method=\"{}\",route=\"\
                     {route}\",le=\"{upper_bound}\"}} {cumulative}\n",
                    key.method,
                ));
            }
            output.push_str(&format!(
                "nagisalake_http_request_duration_seconds_bucket{{method=\"{}\",route=\"{route}\",\
                 le=\"+Inf\"}} {}\n",
                key.method, histogram.count,
            ));
            output.push_str(&format!(
                "nagisalake_http_request_duration_seconds_sum{{method=\"{}\",route=\"{route}\"}} \
                 {}\n",
                key.method,
                histogram.total_nanos as f64 / 1e9,
            ));
            output.push_str(&format!(
                "nagisalake_http_request_duration_seconds_count{{method=\"{}\",route=\"{route}\"\
                 }} {}\n",
                key.method, histogram.count,
            ));
        }
        output
    }
}

impl AppState {
    pub(super) async fn new(config: HubConfig) -> Result<Self, HubError> {
        config.validate()?;
        let objects = ObjectStore::from_s3_config(config.object_store.clone())
            .await
            .map_err(|error| HubError::ObjectStore(error.to_string()))?;
        let store = match &config.database {
            Some(database) => {
                let store = PgStore::connect(database).await.map_err(HubError::Store)?;
                store
                    .ensure_organization(&config.auth.legacy_organization_id, "Legacy API")
                    .await
                    .map_err(HubError::Store)?;
                Some(store)
            }
            None => None,
        };
        let data = match store.as_ref() {
            Some(store) => hydrate_hub_data(store).await?,
            None => HubData::default(),
        };
        // `validate` already resolved these once; doing it again here keeps the
        // secrets out of the config struct so they cannot be logged with it.
        let oauth_providers = match &config.oauth {
            Some(oauth) => oauth
                .resolve()
                .map_err(|error| HubError::InvalidConfig(error.to_string()))?,
            None => BTreeMap::new(),
        };
        let http_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // A provider that hangs must not hold a request open indefinitely.
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|error| HubError::InvalidConfig(error.to_string()))?;
        // Without a control plane there are no accounts to protect, and the
        // legacy token path has no per-account notion at all.
        let rate_limiter = if config.rate_limit.enabled && store.is_some() {
            crate::ratelimit::RateLimiter::new(crate::ratelimit::Limits::default())
        } else {
            crate::ratelimit::RateLimiter::disabled()
        };
        Ok(Self {
            config: Arc::new(config),
            sessions: SessionRegistry::default(),
            data: Arc::new(RwLock::new(data)),
            read_cache: Arc::new(RwLock::new(ReadCache::default())),
            quota_gate: QuotaGate::default(),
            objects,
            store,
            oauth_providers: Arc::new(oauth_providers),
            http_client,
            rate_limiter,
            metrics: Arc::new(HubMetrics::default()),
        })
    }

    pub(super) async fn quota_guard(
        &self,
        organization_id: &str,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        self.quota_gate.acquire(organization_id).await
    }

    pub(super) async fn reserve_storage(
        &self,
        organization_id: &str,
        bytes: i64,
    ) -> Result<(), StoreError> {
        let _guard = self.quota_guard(organization_id).await;
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| StoreError::NotFound("quota policy".into()))?;
        store.reserve_storage(organization_id, bytes).await
    }

    pub(super) async fn release_storage(
        &self,
        organization_id: &str,
        bytes: i64,
    ) -> Result<(), StoreError> {
        let _guard = self.quota_guard(organization_id).await;
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| StoreError::NotFound("quota policy".into()))?;
        store.release_storage(organization_id, bytes).await
    }

    pub(super) async fn cached_job(&self, organization_id: &str, job_id: &str) -> Option<JobView> {
        let key = read_cache_key(organization_id, job_id);
        let cached = self.read_cache.write().await.get_job(&key);
        let counter = if cached.is_some() {
            &self.metrics.read_cache_job_hits_total
        } else {
            &self.metrics.read_cache_job_misses_total
        };
        counter.fetch_add(1, Ordering::Relaxed);
        cached
    }

    pub(super) async fn cache_job(&self, organization_id: &str, job_id: &str, value: JobView) {
        self.read_cache
            .write()
            .await
            .insert_job(read_cache_key(organization_id, job_id), value);
    }

    pub(super) async fn cached_artifact(
        &self,
        organization_id: &str,
        artifact_id: &str,
    ) -> Option<ArtifactRecord> {
        let key = read_cache_key(organization_id, artifact_id);
        let cached = self.read_cache.write().await.get_artifact(&key);
        let counter = if cached.is_some() {
            &self.metrics.read_cache_artifact_hits_total
        } else {
            &self.metrics.read_cache_artifact_misses_total
        };
        counter.fetch_add(1, Ordering::Relaxed);
        cached
    }

    pub(super) async fn cache_artifact(
        &self,
        organization_id: &str,
        artifact_id: &str,
        value: ArtifactRecord,
    ) {
        self.read_cache
            .write()
            .await
            .insert_artifact(read_cache_key(organization_id, artifact_id), value);
    }

    pub(super) async fn invalidate_cached_artifact(
        &self,
        organization_id: &str,
        artifact_id: &str,
    ) {
        self.read_cache
            .write()
            .await
            .remove_artifact(&read_cache_key(organization_id, artifact_id));
    }

    pub(super) async fn cached_device_access(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Option<Vec<nagisalake_hub_store::DeviceAccess>> {
        let key = read_cache_key(organization_id, user_id);
        let cached = self.read_cache.write().await.get_device_access(&key);
        let counter = if cached.is_some() {
            &self.metrics.read_cache_device_access_hits_total
        } else {
            &self.metrics.read_cache_device_access_misses_total
        };
        counter.fetch_add(1, Ordering::Relaxed);
        cached
    }

    pub(super) async fn cache_device_access(
        &self,
        organization_id: &str,
        user_id: &str,
        value: Vec<nagisalake_hub_store::DeviceAccess>,
    ) {
        self.read_cache
            .write()
            .await
            .insert_device_access(read_cache_key(organization_id, user_id), value);
    }

    pub(super) async fn invalidate_cached_device_access_for_user(&self, user_id: &str) {
        self.read_cache
            .write()
            .await
            .remove_device_access_for_user(user_id);
    }

    pub(super) async fn invalidate_cached_device_access_for_organization(
        &self,
        organization_id: &str,
    ) {
        self.read_cache
            .write()
            .await
            .remove_device_access_for_organization(organization_id);
    }

    /// Applies a per-address limit, mapping a denial to a retryable error.
    pub(super) async fn rate_limit_ip(
        &self,
        headers: &HeaderMap,
        peer: Option<std::net::IpAddr>,
        scope: &str,
        quota: crate::ratelimit::Quota,
    ) -> Result<(), HubError> {
        let address = crate::ratelimit::client_address(
            headers,
            peer,
            self.config.rate_limit.trust_forwarded_for,
        );
        match self.rate_limiter.check(scope, &address, quota).await {
            crate::ratelimit::Decision::Allow => Ok(()),
            crate::ratelimit::Decision::Deny {
                retry_after_seconds,
            } => Err(HubError::RateLimited {
                retry_after_seconds,
            }),
        }
    }

    /// Applies a per-key limit, for accounts and organizations.
    pub(super) async fn rate_limit_key(
        &self,
        scope: &str,
        key: &str,
        quota: crate::ratelimit::Quota,
    ) -> Result<(), HubError> {
        match self.rate_limiter.check(scope, key, quota).await {
            crate::ratelimit::Decision::Allow => Ok(()),
            crate::ratelimit::Decision::Deny {
                retry_after_seconds,
            } => Err(HubError::RateLimited {
                retry_after_seconds,
            }),
        }
    }
}

fn read_cache_key(organization_id: &str, resource_id: &str) -> String {
    format!("{organization_id}\0{resource_id}")
}

pub(super) async fn hydrate_hub_data(store: &PgStore) -> Result<HubData, HubError> {
    let mut data = HubData::default();
    for artifact in store.pending_artifacts().await? {
        let size_bytes = u64::try_from(artifact.size_bytes).map_err(|_| {
            HubError::InvalidConfig(format!(
                "artifact {} has a negative persisted size",
                artifact.id
            ))
        })?;
        let state = parse_artifact_state(&artifact.state)?;
        data.artifacts.insert(artifact.id.clone(), ArtifactRecord {
            organization_id: artifact.organization_id,
            view:            ArtifactView {
                id: artifact.id,
                job_id: artifact.job_id,
                name: artifact.name,
                content_type: artifact.content_type,
                size_bytes,
                sha256: artifact.sha256,
                state,
            },
            object_key:      artifact.object_key,
        });
    }
    let events = store.events_for_unfinished_jobs().await?;
    for job in store.unfinished_jobs().await? {
        let state = parse_job_state(&job.state)?;
        let worker_organization_id = job.worker_organization_id.clone().unwrap_or_default();
        let worker_id = job.worker_id.clone().unwrap_or_default();
        let session_id = job.session_id.clone().unwrap_or_default();
        if state != JobState::Queued
            && (worker_organization_id.is_empty() || worker_id.is_empty() || session_id.is_empty())
        {
            return Err(HubError::InvalidConfig(format!(
                "persisted {} job {} has no complete worker binding",
                job.state, job.id
            )));
        }
        let input_artifact_ids: Vec<String> = serde_json::from_str(&job.input_artifact_ids_json)
            .map_err(|error| {
                HubError::InvalidConfig(format!("invalid persisted job inputs: {error}"))
            })?;
        let output_artifact_ids: Vec<String> = serde_json::from_str(&job.output_artifact_ids_json)
            .map_err(|error| {
                HubError::InvalidConfig(format!("invalid persisted job outputs: {error}"))
            })?;
        let parameters: JsonValue =
            serde_json::from_str(&job.parameters_json).map_err(|error| {
                HubError::InvalidConfig(format!("invalid persisted job parameters: {error}"))
            })?;
        let attempt = u32::try_from(job.attempt)
            .map_err(|_| HubError::InvalidConfig("persisted job attempt is invalid".into()))?;
        let mut job_events = events
            .iter()
            .filter(|event| event.organization_id == job.organization_id && event.job_id == job.id)
            .map(|event| {
                Ok(JobEventView {
                    sequence: u64::try_from(event.sequence).map_err(|_| {
                        HubError::InvalidConfig("persisted event sequence is invalid".into())
                    })?,
                    kind:     parse_event_kind(&event.kind)?,
                    progress: event.progress,
                    message:  event.message.clone(),
                    unix_ms:  event.unix_ms,
                })
            })
            .collect::<Result<Vec<_>, HubError>>()?;
        if job_events.len() > 256 {
            job_events.drain(..job_events.len() - 256);
        }
        let record = JobRecord {
            organization_id: job.organization_id,
            actor_id: job.actor_id,
            actor_kind: job.actor_kind,
            actor_user_id: job.actor_user_id,
            worker_organization_id,
            view: JobView {
                id: job.id.clone(),
                workflow_id: job.workflow_id.clone(),
                workflow_version: job.workflow_version.clone(),
                parameters: parameters.clone(),
                input_artifact_ids,
                output_artifact_ids,
                worker_id,
                session_id,
                state,
                progress: job.progress,
                prompt_id: job.prompt_id,
                error: job.error,
                events: job_events,
                created_at_unix_ms: job.created_at,
                updated_at_unix_ms: job.updated_at,
            },
            dispatch: DispatchJob {
                command_id: Uuid::new_v4().to_string(),
                job_id: job.id.clone(),
                attempt,
                workflow_id: job.workflow_id,
                workflow_version: job.workflow_version,
                parameters,
                inputs: Vec::new(),
            },
            last_event: u64::try_from(job.last_event).unwrap_or_default(),
        };
        data.jobs.insert(job.id, record);
    }
    for upload in store.all_upload_requests().await? {
        data.pending_uploads.insert(
            pending_upload_key(&upload.organization_id, &upload.request_id),
            upload.artifact_id,
        );
    }
    Ok(data)
}

pub(super) fn parse_artifact_state(value: &str) -> Result<ArtifactState, HubError> {
    match value {
        "pending_upload" => Ok(ArtifactState::PendingUpload),
        "ready" => Ok(ArtifactState::Ready),
        other => Err(HubError::InvalidConfig(format!(
            "unknown persisted artifact state {other}"
        ))),
    }
}

pub(super) fn parse_job_state(value: &str) -> Result<JobState, HubError> {
    match value {
        "queued" => Ok(JobState::Queued),
        "received" => Ok(JobState::Received),
        "accepted" => Ok(JobState::Accepted),
        "running" => Ok(JobState::Running),
        "uploading" => Ok(JobState::Uploading),
        "completed" => Ok(JobState::Completed),
        "failed" => Ok(JobState::Failed),
        "cancelled" => Ok(JobState::Cancelled),
        other => Err(HubError::InvalidConfig(format!(
            "unknown persisted job state {other}"
        ))),
    }
}

pub(super) fn parse_event_kind(value: &str) -> Result<JobEventKind, HubError> {
    match value {
        "accepted" => Ok(JobEventKind::Accepted),
        "running" => Ok(JobEventKind::Running),
        "progress" => Ok(JobEventKind::Progress),
        "uploading" => Ok(JobEventKind::Uploading),
        "completed" => Ok(JobEventKind::Completed),
        "failed" => Ok(JobEventKind::Failed),
        "cancelled" => Ok(JobEventKind::Cancelled),
        other => Err(HubError::InvalidConfig(format!(
            "unknown persisted event kind {other}"
        ))),
    }
}

/// How long a reserved-but-unused upload may hold quota, as a multiple of the
/// presigned URL's own lifetime.
///
/// Once the URL expires the client can no longer complete the upload, so the
/// reservation is already dead. Doubling only avoids racing a client that is
/// finishing right at the deadline, and it keeps the window proportional: a
/// deployment that shortens `presign_ttl_seconds` for testing also gets a short
/// reclaim window.
pub(super) const PENDING_UPLOAD_TTL_FACTOR: u32 = 2;

pub(super) async fn persist_artifact(
    state: &AppState,
    artifact: &ArtifactRecord,
) -> Result<(), HubError> {
    let Some(store) = state.store.as_ref() else {
        return Ok(());
    };
    let now = now_unix_ms();
    // Only a pending upload gets a deadline. A ready artifact is real data and
    // must never be swept.
    let expires_at = match artifact.view.state {
        ArtifactState::PendingUpload => Some(
            now + i64::try_from(
                state
                    .objects
                    .presign_ttl()
                    .saturating_mul(PENDING_UPLOAD_TTL_FACTOR)
                    .as_millis(),
            )
            .unwrap_or(i64::MAX),
        ),
        ArtifactState::Ready => None,
    };
    store
        .create_artifact(ArtifactUpsert {
            organization_id: &artifact.organization_id,
            id: &artifact.view.id,
            job_id: artifact.view.job_id.as_deref(),
            name: &artifact.view.name,
            content_type: &artifact.view.content_type,
            size_bytes: artifact.view.size_bytes,
            sha256: &artifact.view.sha256,
            state: artifact_state_name(artifact.view.state),
            object_key: &artifact.object_key,
            now,
            expires_at,
        })
        .await?;
    Ok(())
}

pub(super) async fn commit_job_record(
    state: &AppState,
    job: &JobRecord,
    endpoint: &str,
    idempotency_key: Option<&str>,
    request_hash: &str,
    device_admission: Option<DeviceUseAdmission<'_>>,
) -> Result<CommitJobResult, HubError> {
    let Some(store) = state.store.as_ref() else {
        return Ok(CommitJobResult::Created);
    };
    let parameters = serde_json::to_string(&job.view.parameters)
        .map_err(|error| HubError::InvalidRequest(error.to_string()))?;
    let input_ids = serde_json::to_string(&job.view.input_artifact_ids)
        .map_err(|error| HubError::InvalidRequest(error.to_string()))?;
    let output_ids = serde_json::to_string(&job.view.output_artifact_ids)
        .map_err(|error| HubError::InvalidRequest(error.to_string()))?;
    let idempotency = idempotency_key.map(|key| IdempotencyInsert {
        organization_id: &job.organization_id,
        actor_kind: &job.actor_kind,
        actor_id: &job.actor_id,
        endpoint,
        key,
        request_hash,
        job_id: &job.view.id,
        now: job.view.created_at_unix_ms,
    });
    let _quota_guard = state.quota_guard(&job.organization_id).await;
    store
        .commit_new_job(
            JobUpsert {
                organization_id: &job.organization_id,
                id: &job.view.id,
                actor_id: &job.actor_id,
                actor_kind: &job.actor_kind,
                actor_user_id: job.actor_user_id.as_deref(),
                workflow_id: &job.view.workflow_id,
                workflow_version: &job.view.workflow_version,
                parameters_json: &parameters,
                input_artifact_ids_json: &input_ids,
                output_artifact_ids_json: &output_ids,
                worker_id: &job.view.worker_id,
                worker_organization_id: &job.worker_organization_id,
                session_id: &job.view.session_id,
                attempt: i64::from(job.dispatch.attempt),
                state: job_state_name(job.view.state),
                progress: job.view.progress,
                prompt_id: job.view.prompt_id.as_deref(),
                error: job.view.error.as_deref(),
                last_event: job.last_event.min(i64::MAX as u64) as i64,
                now: job.view.created_at_unix_ms,
            },
            &job.view.input_artifact_ids,
            idempotency,
            device_admission,
        )
        .await
        .map_err(map_store_error)
}

pub(super) fn artifact_state_name(state: ArtifactState) -> &'static str {
    match state {
        ArtifactState::PendingUpload => "pending_upload",
        ArtifactState::Ready => "ready",
    }
}

pub(super) fn job_state_name(state: JobState) -> &'static str {
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

pub(super) fn event_kind_name(kind: JobEventKind) -> &'static str {
    match kind {
        JobEventKind::Accepted => "accepted",
        JobEventKind::Running => "running",
        JobEventKind::Progress => "progress",
        JobEventKind::Uploading => "uploading",
        JobEventKind::Completed => "completed",
        JobEventKind::Failed => "failed",
        JobEventKind::Cancelled => "cancelled",
    }
}

pub(super) fn map_store_error(error: StoreError) -> HubError {
    match error {
        StoreError::NotFound(value) => HubError::NotFound(value),
        StoreError::Conflict(value) => HubError::Conflict(value),
        StoreError::QuotaExceeded(value) => HubError::QuotaExceeded(value),
        other => HubError::Store(other),
    }
}

#[cfg(test)]
mod read_cache_tests {
    use super::*;

    fn terminal_job(id: &str) -> JobView {
        JobView {
            id:                  id.into(),
            workflow_id:         "image".into(),
            workflow_version:    "v1".into(),
            parameters:          JsonValue::Null,
            input_artifact_ids:  Vec::new(),
            output_artifact_ids: Vec::new(),
            worker_id:           "worker".into(),
            session_id:          "session".into(),
            state:               JobState::Completed,
            progress:            Some(1.0),
            prompt_id:           None,
            error:               None,
            events:              Vec::new(),
            created_at_unix_ms:  1,
            updated_at_unix_ms:  1,
        }
    }

    #[test]
    fn terminal_cache_is_tenant_scoped_and_expires() {
        let mut cache = ReadCache::default();
        cache.insert_job("tenant-a\0job-1".into(), terminal_job("job-1"));
        assert!(cache.get_job("tenant-a\0job-1").is_some());
        assert!(cache.get_job("tenant-b\0job-1").is_none());

        cache
            .terminal_jobs
            .get_mut("tenant-a\0job-1")
            .expect("test entry exists")
            .expires_at = Instant::now() - Duration::from_secs(1);
        assert!(cache.get_job("tenant-a\0job-1").is_none());
    }

    #[test]
    fn device_access_cache_is_scoped_and_can_be_invalidated() {
        let mut cache = ReadCache::default();
        cache.insert_device_access("org-a\0user-1".into(), vec![
            nagisalake_hub_store::DeviceAccess {
                device_organization_id: "org-a".into(),
                device_id:              "device-a".into(),
                access_kind:            "organization_device".into(),
                allowed_workflows:      Vec::new(),
                max_concurrent_jobs:    None,
                grant_expires_at:       None,
            },
        ]);
        cache.insert_device_access("org-b\0user-1".into(), vec![
            nagisalake_hub_store::DeviceAccess {
                device_organization_id: "org-b".into(),
                device_id:              "device-b".into(),
                access_kind:            "organization_device".into(),
                allowed_workflows:      Vec::new(),
                max_concurrent_jobs:    None,
                grant_expires_at:       None,
            },
        ]);

        assert_eq!(cache.get_device_access("org-a\0user-1").unwrap().len(), 1);
        assert!(cache.get_device_access("org-a\0user-2").is_none());

        cache.remove_device_access_for_user("user-1");
        assert!(cache.get_device_access("org-a\0user-1").is_none());
        assert!(cache.get_device_access("org-b\0user-1").is_none());
    }
}
