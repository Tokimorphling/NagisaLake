# Local stack internals

Background for debugging the stack that `stack.sh` builds. Product-level docs
live in [`docs/`](../../../../docs); this covers only what helps when something
misbehaves locally.

## What talks to what

```
浏览器 / SDK ──HTTP──> Hub :9091 ──WSS/SMUX──> Worker ──HTTP──> ComfyUI :8188
     │                   │                       │
     └───────────────────┴───预签名 PUT/GET──────┴──> MinIO :9000
```

Three services must be reachable, not one. Media never passes through the Hub:
it signs URLs and the client transfers bytes to the object store directly. That
is why `object_store.endpoint_url` has to resolve from the client's machine, and
why binding MinIO to loopback breaks every LAN device while the Hub stays green.

ComfyUI only needs to be reachable from the Worker on the same host, so loopback
is correct there.

## Credential types

Never interchangeable. The prefix identifies which one you are holding.

| Prefix | Purpose | Where the stack puts it |
| --- | --- | --- |
| `nss_` | short-lived browser access token | frontend memory only |
| `nsr_` | rotating refresh token | `HttpOnly` cookie, `/api/v1/auth` |
| `nsc_` | CSRF token for refresh | response body and a readable cookie |
| `nsk_` | programmatic API key | created on demand; `smoke_test.py --api-key` |
| `nwk_` | Worker enrolment token | `.local-stack/worker/worker.toml` |
| `ndi_` | device invite code | handed to another account by the operator |

`stack.sh up` creates an `nwk_` credential per run. Plaintext is returned once,
so a lost token means creating a new one.

`/devices` and `/workflows` are filtered per user. A Worker enrolled with another
account's credential is invisible to yours even while it counts toward
`connected_workers` in `/healthz` — that discrepancy is expected, not a bug.

## Capacity accounting

Three places track the same work, and a mismatch is the usual sign of trouble.

**Worker.** `parallelism + queue_depth` is the admission limit; `parallelism`
alone is the execution semaphore. A job holds a `JobSlot` charged as queued,
which flips to active when it takes a permit. `Drop` releases whichever is
charged, so cancellation while queued, failure, and a panic all balance.

**Hub.** Selecting a worker reserves a slot atomically, otherwise two concurrent
submissions both see the same free slot. A reservation is released on rejection,
held until the next heartbeat on acceptance, and also held when the dispatch
times out — the Worker may have accepted without answering in time, and
releasing there would over-admit. The heartbeat is authoritative because the
Worker increments its counters before sending the ACK.

**PostgreSQL.** `quota_usage.active_jobs` is per organization and is released on
a terminal transition. It is the only one of the three that survives a restart,
and the only one that can drift permanently.

```sql
SELECT u.organization_id, u.active_jobs,
       (SELECT count(*) FROM jobs j
        WHERE j.organization_id = u.organization_id
          AND j.state NOT IN ('completed','failed','cancelled')) AS unfinished
FROM quota_usage u;
```

`active_jobs > unfinished` means a reservation leaked. There is no reconciler,
so a job removed outside the API — or a Worker that vanishes permanently — leaves
it stranded.

## Job states

Protocol terminal states are `completed`, `failed` and `cancelled`.

| State | Meaning |
| --- | --- |
| `received` | persisted, not yet acknowledged |
| `accepted` | Worker owns it; may be waiting for a permit |
| `running` | submitted to ComfyUI |
| `uploading` | outputs being pushed to object storage |

Only non-terminal jobs stay in the Hub's memory. A finished job is served from
PostgreSQL, so `GET /jobs/{id}` still works while the list is paginated from the
store. Cancelling a finished job answers `409`, not `404`.

## Tables worth knowing

| Table | Note |
| --- | --- |
| `workers` | one row per `namespace/node_name`; `capabilities_json` is the last registration |
| `worker_workflows` | which worker offers which version; reconciled on every registration |
| `workflow_versions` | the contract; kept even after no worker offers it, since jobs reference it |
| `artifacts` | `pending_upload` expires and is reclaimed; `ready` is real data |
| `quota_usage` | see above |
| `dispatch_outbox` | has `pending`/`available_at`/`attempts` but **no consumer** |

`dispatch_outbox` is the reason Hub-side queueing is not implemented: the schema
is ready, the consumer is not. Today queueing happens inside the Worker.

## Useful checks

```bash
# Which bundle the running Hub actually serves
curl -s http://127.0.0.1:9091/ | grep -o 'src="/assets/[^"]*"'

# Exactly one worker process
pgrep -f 'release/nagisalake-worker' | wc -l

# Advertised capacity and live load
source .local-stack/env
curl -s -H "Authorization: Bearer $(curl -s -X POST \
  -H 'Content-Type: application/json' \
  -d "{\"email\":\"$NAGISALAKE_EMAIL\",\"password\":\"$NAGISALAKE_PASSWORD\"}" \
  "$NAGISALAKE_BASE_URL/api/v1/auth/login" | python3 -c 'import sys,json;print(json.load(sys.stdin)["access_token"])')" \
  "$NAGISALAKE_BASE_URL/api/v1/workflows" | python3 -m json.tool | head -40

# Objects actually stored, versus quota charged
docker exec nagisalake-local-minio mc du local/nagisalake-local
```

## Mock ComfyUI knobs

```bash
MOCK_PENDING_POLLS=400   # hold prompts running, to observe queueing
MOCK_FAIL_PROMPTS=2      # fail the 2nd prompt, to exercise the failure path
MOCK_PORT=8189
```

The mock reports `/queue` with the oldest live prompt running and the rest
pending, matching ComfyUI's serial execution. Entries are
`[number, prompt_id, prompt, extra_data, outputs]` because the Worker reads the
prompt id positionally.
