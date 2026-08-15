# NagisaLake load generator

`nagisalake-loadgen` is a bounded, production-safe control-plane load tool. It
connects mock workers through the real WebSocket/SMUX protocol, completes jobs
without producing artifacts, and drives authenticated HTTP reads and job
submissions with an open-loop arrival rate.

The input contains bearer secrets. Prefer stdin or the
`NAGISALAKE_LOADGEN_STATE_JSON` environment variable; the tool never includes a
secret in logs or its JSON report.

```json
{
  "base_url": "http://127.0.0.1:9091",
  "tenants": [
    {
      "organization_id": "load-test-organization-id-1",
      "api_keys": ["nsk_REDACTED"],
      "worker_tokens": ["nwk_REDACTED"],
      "worker_namespace": "loadtest-20260814-1"
    },
    {
      "organization_id": "load-test-organization-id-2",
      "api_keys": ["nsk_REDACTED"],
      "worker_tokens": ["nwk_REDACTED"],
      "worker_namespace": "loadtest-20260814-2"
    }
  ]
}
```

```bash
# Local example. `--state -` reads stdin.
cargo run -p nagisalake-loadgen -- \
  --state - --workers 4 --users 2 --rate 20 --duration-seconds 30 \
  < /secure/path/load-state.json

# Every non-loopback host needs an exact, explicit hostname confirmation.
NAGISALAKE_LOADGEN_STATE_JSON="$LOAD_STATE" \
cargo run -p nagisalake-loadgen -- \
  --state - --confirm-production-host hub.example.com \
  --scenario mixed --workers 8 --users 4 --rate 25 --duration-seconds 60 \
  --job-drain-seconds 90
```

`--workers` and `--users` apply to every tenant; HTTP requests rotate across
tenants so an organization-level limiter does not disguise host capacity. Use
`--help` for scenarios and limits. The hard caps cannot be disabled. Two
consecutive failed `/readyz` checks, a mock-worker control failure, or a rolling
30-second HTTP failure rate above 2% stops the run. HTTP 4xx/5xx responses remain
in the report so a staged test can find the saturation point.

After the HTTP phase stops (including a safety stop), mock workers remain
connected while the tool waits for every accepted job to receive an
acknowledged `Completed` event. Every submit receives a client-generated stable
`Idempotency-Key`. If the original send or response body fails, or an HTTP task
must be cancelled at the drain deadline, the tool replays the exact same
principal, organization, body, and key for at most 20 seconds to recover the
authoritative job ID before any worker is stopped. Reconciliation attempts,
resolved submissions, and unresolved failures are included in the JSON report.
An unresolved submit aborts the run rather than silently orphaning a server-side
job.

That cleanup phase has one shared 20-second deadline, at most four in-flight
requests, and a global rate no higher than the configured load or 10 requests
per second. A safety stop therefore cannot turn reconciliation into a second,
unbounded load wave.

That cleanup phase has one shared 20-second deadline, at most four in-flight
requests, and a global rate no higher than the configured load or 10 requests
per second. A safety stop therefore cannot turn reconciliation into a second,
unbounded load wave.

The bounded job drain defaults to 60 seconds and has a hard 120-second cap. A
timeout is reported as
`accepted_job_drain_timeout`, exits non-zero, and includes accepted/completed
counts without printing job credentials.
