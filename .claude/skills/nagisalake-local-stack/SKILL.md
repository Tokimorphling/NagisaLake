---
name: nagisalake-local-stack
description: Run, verify and debug a full local Nagisalake stack — Hub with embedded console, PostgreSQL, MinIO, a mock ComfyUI and a Worker — and reproduce known defects. Use when the user wants to start or restart the local services, test the end-to-end job path, check device capacity or queue depth, debug upload failures, workers that will not register or reconnect in a loop, workflows showing as offline, stale workflow versions, or asks to verify a change against a running system.
---

# Nagisalake local stack

`scripts/local-stack/stack.sh` owns the whole local environment. Prefer it over
starting processes by hand: the manual path has several traps that produce
symptoms which look like product bugs.

```bash
./scripts/local-stack/stack.sh up       # build, start everything, register a worker
./scripts/local-stack/stack.sh test     # submit a job, verify output and checksum
./scripts/local-stack/stack.sh status   # processes, endpoints, capacity, quota drift
./scripts/local-stack/stack.sh logs worker
./scripts/local-stack/stack.sh down     # stop processes, keep data
./scripts/local-stack/stack.sh reset    # stop and delete database, bucket, journal
```

`up` is idempotent. Everything it writes lives in `.local-stack/` (gitignored):
`hub.toml`, `worker/worker.toml`, logs, and `env` with the demo credentials.

Requires PostgreSQL, Docker, `pnpm` and `python3`. No GPU and no model weights:
the mock ComfyUI returns a fixed 8×8 PNG.

## After changing code

```bash
./scripts/check.sh --web    # what CI runs: fmt, clippy, tests, frontend
./scripts/local-stack/stack.sh up && ./scripts/local-stack/stack.sh test
```

`up` rebuilds both the console and the Hub, which matters because the console is
compiled into the binary — see the first trap below.

## Traps that look like product bugs

Each of these has produced a convincing false report. Check them before
concluding the code is wrong.

**A change under `web/` has no effect.** The console is embedded in the Hub
binary by the `embed-web` feature. Running `pnpm build` alone leaves the old
bundle inside the running Hub. Confirm which bundle is actually being served:

```bash
curl -s http://127.0.0.1:9091/ | grep -o 'src="/assets/[^"]*"'
ls web/dist/assets/*.js
```

Different hashes mean the Hub needs rebuilding. `stack.sh up` always does both.

**Killing the Worker appears to work but does not.** The command line is
`--config worker.toml`, a relative path, so `pkill -f worker.toml` matches
nothing. Every "restart" then leaves the previous process alive, and several
Workers sharing one identity evict each other's session in a reconnect loop —
which reads as a Hub session bug. Match the binary instead:

```bash
pgrep -f 'release/nagisalake-worker' | wc -l    # expect 1
pkill -9 -f 'release/nagisalake-worker'
```

`stack.sh status` warns when it sees more than one.

**Uploads fail while the Hub looks healthy.** Media never passes through the
Hub: it signs URLs and the client talks to the object store directly. So
`object_store.endpoint_url` has to be an address the client can reach, and the
object store has to be bound to `0.0.0.0`. A DHCP lease change silently
invalidates a hardcoded address. Compare them:

```bash
ipconfig getifaddr en0                          # macOS
grep endpoint_url .local-stack/hub.toml
```

`stack.sh up` regenerates the config with the current address.

**A worker occupies its full capacity right after starting.** The Worker's
SQLite journal outlives the Hub's database. Dropping the database without
clearing `.local-stack/worker/state/` makes the Worker replay old jobs on start;
recovery deliberately bypasses the admission limit, so it can exceed
`parallelism + queue_depth`. Use `reset`, which removes both.

**Quota is exhausted with no jobs running.** `quota_usage.active_jobs` is
released when a job reaches a terminal state. Deleting job rows directly skips
that, so the reservation is stranded. Cancel through the API instead. `status`
prints reserved against actually-unfinished counts so the drift is visible.

**A workflow disappears from the catalog.** Two workers reporting different
manifests for one `(id, version)` mark it `drifted`, and it stops being listed.
Publish a new version rather than editing one in place.

```sql
SELECT workflow_id, version, approval_state FROM workflow_versions;
```

## Capacity, queueing and status

A device advertises two independent numbers in `.local-stack/worker/worker.toml`:

```toml
parallelism = 1   # jobs executing at once; bounded by the engine and VRAM
queue_depth = 3   # additional jobs the Worker will hold waiting
```

The Hub admits while `active + queued < parallelism + queue_depth`, so the above
takes four jobs and rejects the fifth with `unavailable`. `queue_depth = 0`
restores reject-when-busy. Queued jobs wait behind the Worker's semaphore, so
ComfyUI never receives more than `parallelism` prompts.

The console shows four states, and the distinction matters when reading a bug
report: `可用` (a free execution slot), `可排队` (slots full, queue has room —
still submittable), `忙碌` (both full), `离线` (no connected device).

To observe queueing, hold jobs open and submit past `parallelism`:

```bash
MOCK_PENDING_POLLS=400 ./scripts/local-stack/stack.sh up
```

ComfyUI's own queue only becomes non-empty with `parallelism > 1`, because the
wait happens before the prompt is submitted.

## Reproducing known defects

`scripts/repro/` holds one script per confirmed defect, each exiting `2` when it
reproduces, `0` when the behaviour is fixed, and `3` when a prerequisite failed
rather than guessing.

```bash
source .local-stack/env
./scripts/repro/repro_queued_leak.py       # needs max_concurrent_jobs >= 2
./scripts/repro/repro_upload_quota.py      # --commit to really consume quota
./scripts/repro/repro_login_timing.py
./scripts/repro/repro_login_blocking.py
```

Treat exit `3` as "the test told you nothing". A prerequisite that silently
failed — a cancel rejected for a missing scope, for instance — makes the
following steps look like a reproduction when nothing happened.

## Investigating a report

1. `status` first. It answers whether services are up, whether a duplicate
   Worker is fighting for the session, what capacity the device advertises, and
   whether the quota counter drifted.
2. Read the relevant log: `logs hub`, `logs worker`, `logs comfy`.
3. Before blaming the code, ask whether the previous step created this state.
   Most of the traps above are self-inflicted, and they are convincing.
4. Reproduce from a clean slate: `reset && up && test`. If it survives that, the
   defect is real.

## Deliberate limits

- Registration is open and MinIO uses default credentials. Trusted networks only.
- Plain HTTP is not a secure context, so the console falls back to a JS SHA-256
  and `execCommand` for clipboard. That makes LAN testing work; it does not make
  HTTP safe.
- The mock implements only the ComfyUI endpoints the Worker calls. A Worker that
  starts calling something else fails loudly here instead of passing silently.

See [references/deep-dive.md](references/deep-dive.md) for the request paths,
credential types and database tables behind these commands.
