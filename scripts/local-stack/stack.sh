#!/usr/bin/env bash
# Brings up a full local Nagisalake stack and runs the end-to-end path:
# Hub + embedded console, PostgreSQL, MinIO, a mock ComfyUI, and a Worker.
#
#   ./scripts/local-stack/stack.sh up       # build, start, register a worker
#   ./scripts/local-stack/stack.sh test     # submit a job and verify the output
#   ./scripts/local-stack/stack.sh status   # what is running, and its capacity
#   ./scripts/local-stack/stack.sh logs [hub|worker|comfy]
#   ./scripts/local-stack/stack.sh down     # stop processes, keep data
#   ./scripts/local-stack/stack.sh reset    # stop and delete all local state
#
# Everything lives under .local-stack/ (gitignored). Nothing here is safe for a
# shared machine: it uses default MinIO credentials and enables registration.
set -euo pipefail

cd "$(dirname "$0")/../.."
ROOT="$PWD"
STATE="$ROOT/.local-stack"
HUB_CONFIG="$STATE/hub.toml"
WORKER_DIR="$STATE/worker"
WORKER_CONFIG="$WORKER_DIR/worker.toml"
ENV_FILE="$STATE/env"

DB_NAME="${NAGISALAKE_LOCAL_DB:-nagisalake_local}"
DB_USER="${NAGISALAKE_LOCAL_DB_USER:-$(whoami)}"
HUB_PORT="${NAGISALAKE_LOCAL_HUB_PORT:-9091}"
COMFY_PORT="${NAGISALAKE_LOCAL_COMFY_PORT:-8188}"
MINIO_PORT="${NAGISALAKE_LOCAL_MINIO_PORT:-9000}"
MINIO_CONSOLE_PORT="${NAGISALAKE_LOCAL_MINIO_CONSOLE_PORT:-9001}"
MINIO_CONTAINER="${NAGISALAKE_LOCAL_MINIO_CONTAINER:-nagisalake-local-minio}"
BUCKET="nagisalake-local"
DEMO_EMAIL="${NAGISALAKE_LOCAL_EMAIL:-local@nagisalake.test}"
DEMO_PASSWORD="${NAGISALAKE_LOCAL_PASSWORD:-local-stack-password}"
PARALLELISM="${NAGISALAKE_LOCAL_PARALLELISM:-1}"
QUEUE_DEPTH="${NAGISALAKE_LOCAL_QUEUE_DEPTH:-3}"

# Worker processes are matched by binary path, never by the config filename.
# `--config worker.toml` is a relative argument, so a pattern like
# `pkill -f worker.toml` silently matches nothing and leaves the old process
# running. Several workers sharing one identity then evict each other's session
# in a reconnect loop that looks exactly like a Hub bug.
WORKER_PATTERN='release/nagisalake-worker'
HUB_PATTERN="nagisalake-hub --config $STATE/hub.toml"
COMFY_PATTERN='local-stack/mock_comfyui.py'

step() { printf '\n\033[1m==> %s\033[0m\n' "$1"; }
info() { printf '     %s\n' "$1"; }
fail() { printf '\n\033[31m%s\033[0m\n' "$1" >&2; exit 1; }

# The object store is reached directly by browsers, SDKs and Workers, so its
# endpoint must be an address other machines can resolve. Using 127.0.0.1 makes
# uploads fail on any LAN device while the Hub itself looks perfectly healthy.
lan_address() {
  if [[ -n "${NAGISALAKE_LOCAL_HOST:-}" ]]; then
    printf '%s' "$NAGISALAKE_LOCAL_HOST"
    return
  fi
  local address=""
  if command -v ipconfig >/dev/null 2>&1; then
    for interface in en0 en1 en2; do
      address=$(ipconfig getifaddr "$interface" 2>/dev/null || true)
      [[ -n "$address" ]] && break
    done
  fi
  if [[ -z "$address" ]] && command -v hostname >/dev/null 2>&1; then
    address=$(hostname -I 2>/dev/null | awk '{print $1}')
  fi
  printf '%s' "${address:-127.0.0.1}"
}

require() { command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"; }

# ------------------------------------------------------------------ helpers

api() {
  local method="$1" path="$2" body="${3:-}" token="${4:-}"
  local args=(-s -m 30 -X "$method" "http://127.0.0.1:$HUB_PORT/api/v1$path")
  [[ -n "$body" ]] && args+=(-H 'Content-Type: application/json' -d "$body")
  [[ -n "$token" ]] && args+=(-H "Authorization: Bearer $token")
  args+=(-H "Idempotency-Key: local-$(date +%s%N)")
  curl "${args[@]}"
}

json_field() { python3 -c "
import json,sys
try:
    print(json.load(sys.stdin).get('$1',''))
except Exception:
    print('')
"; }

wait_for_http() {
  local url="$1" label="$2" attempts="${3:-60}"
  for _ in $(seq 1 "$attempts"); do
    curl -fsS -m 2 "$url" >/dev/null 2>&1 && return 0
    sleep 1
  done
  fail "$label did not become reachable at $url"
}

# ---------------------------------------------------------------------- up

cmd_up() {
  require cargo; require psql; require docker; require python3; require curl
  mkdir -p "$STATE" "$WORKER_DIR/workflows"

  local host
  host=$(lan_address)
  step "Host address: $host"
  info "Console and API will be reachable at http://$host:$HUB_PORT"
  if [[ "$host" == "127.0.0.1" ]]; then
    info "No LAN address detected; other machines will not be able to connect."
  fi

  step "PostgreSQL"
  psql -h 127.0.0.1 -U "$DB_USER" -lqt >/dev/null 2>&1 \
    || fail "cannot reach PostgreSQL at 127.0.0.1 as user $DB_USER"
  if psql -h 127.0.0.1 -U "$DB_USER" -lqt | cut -d'|' -f1 | grep -qw "$DB_NAME"; then
    info "database $DB_NAME exists"
  else
    createdb -h 127.0.0.1 -U "$DB_USER" "$DB_NAME"
    info "created database $DB_NAME"
  fi

  step "MinIO"
  if [[ -n "$(docker ps -q --filter "name=^${MINIO_CONTAINER}$")" ]]; then
    info "container $MINIO_CONTAINER already running"
  else
    docker rm -f "$MINIO_CONTAINER" >/dev/null 2>&1 || true
    # Name the conflict rather than letting docker's raw bind error surface: the
    # usual cause is another MinIO from a previous manual setup.
    local occupant
    occupant=$(docker ps --filter "publish=$MINIO_PORT" --format '{{.Names}}' | head -1)
    if [[ -n "$occupant" ]]; then
      fail "port $MINIO_PORT is already published by container '$occupant'.
Stop it (docker rm -f $occupant) or pick another port with
NAGISALAKE_LOCAL_MINIO_PORT=<port> $0 up"
    fi
    if lsof -nP -iTCP:"$MINIO_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
      fail "port $MINIO_PORT is in use by a non-container process.
Free it, or set NAGISALAKE_LOCAL_MINIO_PORT=<port>."
    fi
    # Bound to 0.0.0.0 so LAN devices can reach it: presigned URLs point here,
    # not at the Hub.
    docker run -d --name "$MINIO_CONTAINER" \
      -p "0.0.0.0:$MINIO_PORT:9000" -p "0.0.0.0:$MINIO_CONSOLE_PORT:9001" \
      -v "${MINIO_CONTAINER}-data:/data" \
      -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
      quay.io/minio/minio:latest server /data --console-address ":9001" >/dev/null
    info "started container $MINIO_CONTAINER"
  fi
  wait_for_http "http://127.0.0.1:$MINIO_PORT/minio/health/live" "MinIO"
  docker exec "$MINIO_CONTAINER" mc alias set local "http://127.0.0.1:9000" \
    minioadmin minioadmin >/dev/null 2>&1
  docker exec "$MINIO_CONTAINER" mc mb --ignore-existing "local/$BUCKET" >/dev/null 2>&1
  info "bucket $BUCKET ready"

  step "Building the console and the Hub"
  # The console is compiled into the binary, so a change under web/ is invisible
  # until the Hub is rebuilt too. Always doing both removes that trap.
  (cd web && pnpm install --frozen-lockfile >/dev/null 2>&1 && pnpm build >/dev/null)
  info "web/dist built"
  cargo build --release -p nagisalake-hub --features embed-web >/dev/null
  cargo build --release -p nagisalake-worker-cli >/dev/null
  info "binaries built"

  step "Writing configuration"
  write_hub_config "$host"
  info "$HUB_CONFIG"

  step "Starting the Hub"
  stop_pattern "$HUB_PATTERN"
  RUST_LOG="${RUST_LOG:-info}" \
  NAGISALAKE_S3_ACCESS_KEY_ID=minioadmin \
  NAGISALAKE_S3_SECRET_ACCESS_KEY=minioadmin \
    nohup "$ROOT/target/release/nagisalake-hub" --config "$HUB_CONFIG" \
    > "$STATE/hub.log" 2>&1 &
  wait_for_http "http://127.0.0.1:$HUB_PORT/healthz" "Hub"
  info "$(curl -s "http://127.0.0.1:$HUB_PORT/healthz")"

  step "Mock ComfyUI"
  stop_pattern "$COMFY_PATTERN"
  MOCK_PORT="$COMFY_PORT" \
  MOCK_PENDING_POLLS="${MOCK_PENDING_POLLS:-2}" \
    nohup python3 "$ROOT/scripts/local-stack/mock_comfyui.py" \
    > "$STATE/comfy.log" 2>&1 &
  wait_for_http "http://127.0.0.1:$COMFY_PORT/system_stats" "mock ComfyUI"
  info "listening on 127.0.0.1:$COMFY_PORT"

  step "Account and worker credential"
  ensure_account
  local token org worker_token
  token=$(login_token)
  org=$(api GET /auth/me '' "$token" | json_field current_organization_id)
  [[ -n "$org" ]] || fail "could not resolve the organization"
  worker_token=$(api POST "/organizations/$org/worker-credentials" \
    "{\"name\":\"local-stack\",\"allowed_namespace\":\"local\"}" "$token" \
    | json_field plaintext)
  [[ "$worker_token" == nwk_* ]] || fail "could not create a worker credential"
  info "organization $org"
  info "worker credential ${worker_token:0:12}…"

  step "Starting the Worker"
  write_worker_config "$host" "$worker_token"
  stop_pattern "$WORKER_PATTERN"
  (cd "$WORKER_DIR" && RUST_LOG="${RUST_LOG:-info}" \
    nohup "$ROOT/target/release/nagisalake-worker" --config worker.toml \
    > "$STATE/worker.log" 2>&1 &)
  for _ in $(seq 1 30); do
    grep -q "worker registered with Hub" "$STATE/worker.log" 2>/dev/null && break
    sleep 1
  done
  grep -q "worker registered with Hub" "$STATE/worker.log" \
    || fail "worker did not register; see $STATE/worker.log"
  info "registered (parallelism=$PARALLELISM queue_depth=$QUEUE_DEPTH)"

  cat > "$ENV_FILE" <<EOF
export NAGISALAKE_BASE_URL=http://127.0.0.1:$HUB_PORT
export NAGISALAKE_EMAIL=$DEMO_EMAIL
export NAGISALAKE_PASSWORD='$DEMO_PASSWORD'
EOF

  step "Ready"
  info "Console  http://$host:$HUB_PORT"
  info "Sign in  $DEMO_EMAIL / $DEMO_PASSWORD"
  info "Verify   ./scripts/local-stack/stack.sh test"
}

write_hub_config() {
  local host="$1"
  cat > "$HUB_CONFIG" <<EOF
# Generated by scripts/local-stack/stack.sh. Local development only.
[server]
listen = "0.0.0.0:$HUB_PORT"

[auth]
worker_token = "local-stack-legacy-worker"
consumer_token = "local-stack-legacy-consumer"

[browser]
# Open registration on a 0.0.0.0 bind: only acceptable on a trusted network.
registration_enabled = true
password_auth_enabled = true
cookie_secure = false
allow_insecure_cookies = true
access_ttl_seconds = 900
refresh_ttl_seconds = 2592000
# The embedded console is same-origin and needs no entry. This is only for a
# separately hosted Vite dev server.
allowed_origins = ["http://localhost:3000", "http://127.0.0.1:3000", "http://$host:3000"]

[database]
url = "postgres://$DB_USER@127.0.0.1:5432/$DB_NAME"
max_connections = 10
run_migrations = true

[transport]
max_frame_bytes = 1048576
accept_timeout_seconds = 15
command_ack_timeout_seconds = 10
heartbeat_interval_seconds = 15
max_artifact_bytes = 5368709120

[object_store]
bucket = "$BUCKET"
region = "us-east-1"
# Must be LAN-reachable: clients and Workers connect here directly. Regenerate
# this file after the host IP changes, or uploads fail while the Hub looks fine.
endpoint_url = "http://$host:$MINIO_PORT"
force_path_style = true
presign_ttl_seconds = 900
access_key_id_env = "NAGISALAKE_S3_ACCESS_KEY_ID"
secret_access_key_env = "NAGISALAKE_S3_SECRET_ACCESS_KEY"
EOF
}

write_worker_config() {
  local host="$1" token="$2"
  cp "$ROOT/examples/workflows/sdxl-txt2img-api.json" "$WORKER_DIR/workflows/"
  # A second workflow with an artifact input, so the upload path is covered.
  python3 - "$WORKER_DIR/workflows" <<'PY'
import json, sys
from pathlib import Path
directory = Path(sys.argv[1])
graph = json.loads((directory / "sdxl-txt2img-api.json").read_text())
graph["10"] = {"class_type": "LoadImage", "inputs": {"image": "placeholder.png", "upload": "image"}}
graph["11"] = {"class_type": "VAEEncode", "inputs": {"pixels": ["10", 0], "vae": ["4", 2]}}
graph["3"]["inputs"]["latent_image"] = ["11", 0]
(directory / "image-edit-api.json").write_text(json.dumps(graph, indent=2))
PY
  cat > "$WORKER_CONFIG" <<EOF
# Generated by scripts/local-stack/stack.sh. Local development only.
work_dir = "./work"

[hub]
url = "ws://$host:$HUB_PORT/v1/worker/connect"
reconnect_max_seconds = 30
connect_timeout_seconds = 15
max_frame_bytes = 1048576
token = "$token"

[worker]
# Must match the credential's allowed_namespace.
namespace = "local"
node_name = "mock-comfyui"
# How many jobs run at once. Bounded by the engine and by VRAM.
parallelism = $PARALLELISM
# How many more the Worker will hold waiting. 0 restores reject-when-busy.
queue_depth = $QUEUE_DEPTH

[worker.labels]
engine = "mock"

[state]
sqlite_url = "sqlite://state/worker.db"

[comfyui]
base_url = "http://127.0.0.1:$COMFY_PORT"
poll_interval_ms = 500
request_timeout_seconds = 60
max_output_bytes = 5368709120

[[workflows]]
id = "local-txt2img"
version = "v1"
file = "./workflows/sdxl-txt2img-api.json"
output_types = ["image/png"]

[workflows.parameters]
prompt = "/6/inputs/text"
negative_prompt = "/5/inputs/text"
seed = "/3/inputs/seed"
steps = "/3/inputs/steps"

[[workflows]]
id = "local-image-edit"
version = "v1"
file = "./workflows/image-edit-api.json"
output_types = ["image/png"]

[workflows.parameters]
prompt = "/6/inputs/text"

[[workflows.inputs]]
index = 0
name = "source_image"
content_type = "image/*"
pointer = "/10/inputs/image"
EOF
}

ensure_account() {
  local response
  response=$(api POST /auth/register \
    "{\"email\":\"$DEMO_EMAIL\",\"password\":\"$DEMO_PASSWORD\",\"organization_name\":\"Local stack\"}")
  if [[ -n "$(printf '%s' "$response" | json_field access_token)" ]]; then
    info "registered $DEMO_EMAIL"
  else
    info "account already exists, signing in"
  fi
}

login_token() {
  api POST /auth/login "{\"email\":\"$DEMO_EMAIL\",\"password\":\"$DEMO_PASSWORD\"}" \
    | json_field access_token
}

# -------------------------------------------------------------------- test

cmd_test() {
  [[ -f "$ENV_FILE" ]] || fail "stack is not up; run: $0 up"
  step "End-to-end job through the full chain"
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  "$ROOT/scripts/smoke_test.py" \
    --base-url "$NAGISALAKE_BASE_URL" \
    --email "$NAGISALAKE_EMAIL" \
    --password "$NAGISALAKE_PASSWORD" \
    --workflow local-image-edit \
    --param prompt "local stack end to end" \
    --output-dir "$STATE/outputs"
}

# ------------------------------------------------------------------ status

cmd_status() {
  step "Processes"
  printf '     %-14s %s\n' "hub" "$(count_pattern "$HUB_PATTERN")"
  printf '     %-14s %s\n' "worker" "$(count_pattern "$WORKER_PATTERN")"
  printf '     %-14s %s\n' "mock comfyui" "$(count_pattern "$COMFY_PATTERN")"
  printf '     %-14s %s\n' "minio" "$(docker ps -q --filter "name=^${MINIO_CONTAINER}$" | wc -l | tr -d ' ')"
  # More than one worker means duplicate identities fighting over the session.
  if [[ "$(count_pattern "$WORKER_PATTERN")" -gt 1 ]]; then
    info "WARNING: multiple workers share one identity and will evict each other"
  fi

  step "Endpoints"
  printf '     %-14s %s\n' "hub" "$(curl -s -m 3 "http://127.0.0.1:$HUB_PORT/healthz" || echo unreachable)"
  printf '     %-14s %s\n' "console" "$(curl -s -m 3 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$HUB_PORT/" || echo '-')"
  printf '     %-14s %s\n' "minio" "$(curl -s -m 3 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$MINIO_PORT/minio/health/live" || echo '-')"
  printf '     %-14s %s\n' "comfyui" "$(curl -s -m 3 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$COMFY_PORT/system_stats" || echo '-')"

  if [[ -f "$ENV_FILE" ]]; then
    step "Capacity"
    local token
    token=$(login_token)
    api GET /workflows '' "$token" | python3 -c "
import json,sys
data = json.load(sys.stdin)
if not isinstance(data, list):
    print('     could not read the catalog:', data); raise SystemExit
for workflow in data:
    workers = workflow['workers']
    state = 'offline' if not workers else ('available' if workflow['available'] else 'saturated')
    print(f\"     {workflow['id']}@{workflow['version']:4s} {state:10s} devices={len(workers)}\")
    for worker in workers:
        print(f\"       {worker['worker_id']} parallelism={worker['parallelism']} \"
              f\"queue_depth={worker['queue_depth']} active={worker['active_jobs']} \"
              f\"queued={worker['queued_jobs']}\")
"
    step "Quota"
    # A count above the real number of unfinished jobs means the counter leaked,
    # usually from deleting job rows directly instead of cancelling them.
    psql -h 127.0.0.1 -U "$DB_USER" -d "$DB_NAME" -tAF' ' -c "
      SELECT 'reserved=' || u.active_jobs,
             'unfinished=' || (SELECT count(*) FROM jobs j
                               WHERE j.organization_id = u.organization_id
                                 AND j.state NOT IN ('completed','failed','cancelled'))
      FROM quota_usage u
      WHERE u.active_jobs > 0
         OR EXISTS (SELECT 1 FROM jobs j WHERE j.organization_id = u.organization_id
                    AND j.state NOT IN ('completed','failed','cancelled'));" \
      2>/dev/null | sed 's/^/     /' || true
  fi
}

count_pattern() { pgrep -f "$1" 2>/dev/null | wc -l | tr -d ' '; }
stop_pattern() { pkill -f "$1" 2>/dev/null || true; }

# -------------------------------------------------------------- logs / down

cmd_logs() {
  case "${1:-hub}" in
    hub) tail -n "${2:-40}" "$STATE/hub.log" ;;
    worker) tail -n "${2:-40}" "$STATE/worker.log" ;;
    comfy|comfyui) tail -n "${2:-40}" "$STATE/comfy.log" ;;
    *) fail "unknown log: ${1:-}. Use hub, worker or comfy." ;;
  esac
}

cmd_down() {
  step "Stopping"
  # Worker first: SIGKILL because a queued job holds it in a wait.
  pkill -9 -f "$WORKER_PATTERN" 2>/dev/null || true
  stop_pattern "$HUB_PATTERN"
  stop_pattern "$COMFY_PATTERN"
  sleep 2
  info "hub=$(count_pattern "$HUB_PATTERN") worker=$(count_pattern "$WORKER_PATTERN") comfy=$(count_pattern "$COMFY_PATTERN")"
  info "MinIO container left running; use reset to remove it"
}

cmd_reset() {
  cmd_down
  step "Removing local state"
  docker rm -f "$MINIO_CONTAINER" >/dev/null 2>&1 || true
  docker volume rm "${MINIO_CONTAINER}-data" >/dev/null 2>&1 || true
  info "MinIO container and volume removed"
  dropdb -h 127.0.0.1 -U "$DB_USER" --if-exists "$DB_NAME" 2>/dev/null || true
  info "database $DB_NAME dropped"
  # The Worker's SQLite journal outlives the database. Leaving it behind makes
  # the Worker replay old jobs on the next start and occupy its whole capacity.
  rm -rf "$STATE"
  info "$STATE removed"
}

case "${1:-}" in
  up) shift; cmd_up "$@" ;;
  test) shift; cmd_test "$@" ;;
  status) shift; cmd_status "$@" ;;
  logs) shift; cmd_logs "$@" ;;
  down) shift; cmd_down "$@" ;;
  reset) shift; cmd_reset "$@" ;;
  *) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//' ;;
esac
