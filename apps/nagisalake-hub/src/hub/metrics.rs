use super::*;

pub(super) const HTTP_DURATION_BUCKETS_SECONDS: [f64; 12] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct HttpRequestKey {
    pub(super) method:        &'static str,
    pub(super) route:         String,
    pub(super) status_family: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct HttpRouteKey {
    pub(super) method: &'static str,
    pub(super) route:  String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct HttpDurationHistogram {
    /// Non-cumulative buckets. Rendering converts these to Prometheus's
    /// cumulative representation.
    pub(super) buckets:     [u64; HTTP_DURATION_BUCKETS_SECONDS.len()],
    pub(super) count:       u64,
    pub(super) total_nanos: u128,
}

impl HttpDurationHistogram {
    fn observe(&mut self, elapsed: Duration) {
        let seconds = elapsed.as_secs_f64();
        if let Some(bucket) = HTTP_DURATION_BUCKETS_SECONDS
            .iter()
            .position(|upper_bound| seconds <= *upper_bound)
        {
            self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
        }
        self.count = self.count.saturating_add(1);
        self.total_nanos = self.total_nanos.saturating_add(elapsed.as_nanos());
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct HttpMetrics {
    pub(super) requests:  BTreeMap<HttpRequestKey, u64>,
    pub(super) durations: BTreeMap<HttpRouteKey, HttpDurationHistogram>,
}

impl HubMetrics {
    fn begin_http_request(&self) -> HttpInFlightGuard<'_> {
        self.http_requests_in_flight.fetch_add(1, Ordering::Relaxed);
        HttpInFlightGuard { metrics: self }
    }

    fn observe_http(
        &self,
        method: &'static str,
        route: String,
        status_family: &'static str,
        elapsed: Duration,
    ) {
        let mut http = self
            .http
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = http
            .requests
            .entry(HttpRequestKey {
                method,
                route: route.clone(),
                status_family,
            })
            .or_default();
        *count = count.saturating_add(1);
        http.durations
            .entry(HttpRouteKey { method, route })
            .or_default()
            .observe(elapsed);
    }
}

struct HttpInFlightGuard<'a> {
    metrics: &'a HubMetrics,
}

impl Drop for HttpInFlightGuard<'_> {
    fn drop(&mut self) {
        self.metrics
            .http_requests_in_flight
            .fetch_sub(1, Ordering::Relaxed);
    }
}

/// Records bounded-cardinality HTTP metrics. `MatchedPath` contains the route
/// template registered by the application, never a user-supplied path. The
/// fallback is collapsed into one label and `/metrics` is excluded so scraping
/// does not recursively change the series it is reading.
pub(super) async fn observe_http_request(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if request.uri().path() == "/metrics" {
        return next.run(request).await;
    }
    let method = http_method_label(request.method());
    let route = request.extensions().get::<MatchedPath>().map_or_else(
        || "__fallback__".to_owned(),
        |path| path.as_str().to_owned(),
    );
    let started = Instant::now();
    let in_flight = state.metrics.begin_http_request();
    let response = next.run(request).await;
    drop(in_flight);
    state.metrics.observe_http(
        method,
        route,
        status_family_label(response.status()),
        started.elapsed(),
    );
    response
}

fn http_method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::HEAD => "HEAD",
        Method::OPTIONS => "OPTIONS",
        Method::CONNECT => "CONNECT",
        Method::TRACE => "TRACE",
        _ => "OTHER",
    }
}

fn status_family_label(status: StatusCode) -> &'static str {
    match status.as_u16() / 100 {
        1 => "1xx",
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        5 => "5xx",
        _ => "other",
    }
}

pub(super) fn escape_prometheus_label(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_methods_are_bounded_and_escaped() {
        assert_eq!(http_method_label(&Method::GET), "GET");
        assert_eq!(
            http_method_label(&Method::from_bytes(b"PURGE").unwrap()),
            "OTHER"
        );
        assert_eq!(escape_prometheus_label("a\\\"b\nc"), "a\\\\\\\"b\\nc");
    }

    #[test]
    fn duration_histogram_renders_cumulative_buckets() {
        let metrics = HubMetrics::default();
        metrics.observe_http(
            "GET",
            "/v1/jobs/{job_id}".into(),
            "2xx",
            Duration::from_millis(7),
        );
        metrics.observe_http(
            "GET",
            "/v1/jobs/{job_id}".into(),
            "4xx",
            Duration::from_millis(60),
        );
        let rendered = metrics.render(0, None);
        assert!(rendered.contains(
            "nagisalake_http_requests_total{method=\"GET\",route=\"/v1/jobs/{job_id}\",\
             status_family=\"2xx\"} 1"
        ));
        assert!(rendered.contains(
            "nagisalake_http_request_duration_seconds_bucket{method=\"GET\",route=\"/v1/jobs/\
             {job_id}\",le=\"0.01\"} 1"
        ));
        assert!(rendered.contains(
            "nagisalake_http_request_duration_seconds_bucket{method=\"GET\",route=\"/v1/jobs/\
             {job_id}\",le=\"0.1\"} 2"
        ));
        assert!(rendered.contains(
            "nagisalake_http_request_duration_seconds_count{method=\"GET\",route=\"/v1/jobs/\
             {job_id}\"} 2"
        ));
    }
}
