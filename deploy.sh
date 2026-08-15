#!/usr/bin/env bash
# Deploy the NagisaLake Hub image to the production AWS host.
#
# The script deliberately keeps production secrets and configuration on the
# server. It pulls before stopping the current container, keeps that container
# available for rollback until the replacement passes /healthz and /readyz,
# and verifies the public HTTPS endpoint after the rollout.
# Do not inherit tracing or non-interactive shell startup hooks from the caller:
# the optional GHCR credential must never be expanded into trace output.
case "$-" in
    *x*) set +x ;;
esac
unset BASH_ENV ENV
set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
target_host="${NAGISALAKE_DEPLOY_HOST:-binance-test-3}"
image_ref="${NAGISALAKE_IMAGE:-ghcr.io/tokimorphling/nagisalake-hub:latest}"
container_name="${NAGISALAKE_CONTAINER:-nagisalake-hub}"
remote_dir="${NAGISALAKE_REMOTE_DIR:-/home/ubuntu/nagisalake}"
env_file="${NAGISALAKE_ENV_FILE:-${remote_dir}/.env}"
config_file="${NAGISALAKE_CONFIG_FILE:-${remote_dir}/hub.toml}"
container_user="${NAGISALAKE_CONTAINER_USER:-nagisalake:nagisalake}"
port_bind="${NAGISALAKE_PORT_BIND:-127.0.0.1:9091:9091}"
health_url="${NAGISALAKE_HEALTH_URL:-http://127.0.0.1:9091}"
# An explicitly empty NAGISALAKE_PUBLIC_URL disables the public check.
public_url="${NAGISALAKE_PUBLIC_URL-https://nagisalake.tokilake.abrdns.com}"
ssh_identity_file="${NAGISALAKE_SSH_IDENTITY_FILE:-}"
readiness_timeout="${NAGISALAKE_READINESS_TIMEOUT:-90}"
deploy_mode="${NAGISALAKE_DEPLOY_MODE:-auto}"
dry_run=false
force=false

usage() {
    cat <<'EOF'
Usage: ./deploy.sh [options]

Pull and deploy a NagisaLake Hub image on the production AWS instance.

Options:
  --image IMAGE              Image tag or immutable digest
                             (default: ghcr.io/tokimorphling/nagisalake-hub:latest)
  --mode auto|fresh|upgrade  Allow either state, require no container, or require one
                             (default: auto)
  --host HOST                SSH host or alias (default: binance-test-3)
  --identity-file PATH       SSH private key to use
  --public-url URL           Public base URL checked after local readiness
  --skip-public-check        Do not check the public HTTPS endpoint
  --force                    Recreate the container even if that image is running
  --dry-run                  Run the read-only preflight and print the resolved plan
  -h, --help                 Show this help

Environment overrides:
  NAGISALAKE_DEPLOY_HOST, NAGISALAKE_IMAGE, NAGISALAKE_CONTAINER,
  NAGISALAKE_REMOTE_DIR, NAGISALAKE_ENV_FILE, NAGISALAKE_CONFIG_FILE,
  NAGISALAKE_CONTAINER_USER, NAGISALAKE_PORT_BIND, NAGISALAKE_HEALTH_URL,
  NAGISALAKE_PUBLIC_URL, NAGISALAKE_SSH_IDENTITY_FILE,
  NAGISALAKE_READINESS_TIMEOUT, NAGISALAKE_DEPLOY_MODE

The script uses the existing remote .env and hub.toml; it never prints or
uploads them. If GHCR authentication is required, export GITHUB_PAT locally.
The token is used only through docker login --password-stdin after an
unauthenticated pull fails.

Examples:
  ./deploy.sh
  ./deploy.sh --dry-run
  ./deploy.sh --image ghcr.io/tokimorphling/nagisalake-hub:v0.2.0
  ./deploy.sh --image ghcr.io/tokimorphling/nagisalake-hub@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
EOF
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

while (($# > 0)); do
    case "$1" in
        --image)
            [[ $# -ge 2 ]] || die "--image requires a value"
            image_ref="$2"
            shift 2
            ;;
        --mode)
            [[ $# -ge 2 ]] || die "--mode requires a value"
            deploy_mode="$2"
            shift 2
            ;;
        --host)
            [[ $# -ge 2 ]] || die "--host requires a value"
            target_host="$2"
            shift 2
            ;;
        --identity-file)
            [[ $# -ge 2 ]] || die "--identity-file requires a value"
            ssh_identity_file="$2"
            shift 2
            ;;
        --public-url)
            [[ $# -ge 2 ]] || die "--public-url requires a value"
            public_url="$2"
            shift 2
            ;;
        --skip-public-check)
            public_url=''
            shift
            ;;
        --force)
            force=true
            shift
            ;;
        --dry-run)
            dry_run=true
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            printf 'unknown option: %s\n\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$deploy_mode" in
    auto|fresh|upgrade) ;;
    *) die "invalid deployment mode: $deploy_mode" ;;
esac

[[ "$readiness_timeout" =~ ^[1-9][0-9]*$ ]] \
    || die "NAGISALAKE_READINESS_TIMEOUT must be an integer from 5 to 300"
(( readiness_timeout >= 5 && readiness_timeout <= 300 )) \
    || die "NAGISALAKE_READINESS_TIMEOUT must be an integer from 5 to 300"
[[ "$target_host" != -* && "$target_host" =~ ^[A-Za-z0-9_.:@-]+$ ]] \
    || die "invalid SSH host: $target_host"
[[ "$image_ref" =~ ^ghcr\.io/tokimorphling/nagisalake-hub(:[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}|@sha256:[a-fA-F0-9]{64})$ ]] \
    || die "image must be a tag or sha256 digest under ghcr.io/tokimorphling/nagisalake-hub"
[[ "$container_name" =~ ^[A-Za-z0-9][A-Za-z0-9_.-]+$ ]] \
    || die "invalid container name: $container_name"
[[ "$container_user" =~ ^[A-Za-z0-9_-]+:[A-Za-z0-9_-]+$ ]] \
    || die "invalid container user: $container_user"
for remote_path in "$remote_dir" "$env_file" "$config_file"; do
    [[ "$remote_path" =~ ^/[A-Za-z0-9_./-]+$ ]] \
        || die "invalid remote path: $remote_path"
    [[ "$remote_path" != *'/../'* && "$remote_path" != */.. \
        && "$remote_path" != *'/./'* && "$remote_path" != */. ]] \
        || die "remote paths must not contain . or .. segments: $remote_path"
done
case "$env_file" in
    "$remote_dir"/*/*) die "NAGISALAKE_ENV_FILE must be a direct child of NAGISALAKE_REMOTE_DIR" ;;
    "$remote_dir"/*) ;;
    *) die "NAGISALAKE_ENV_FILE must be a direct child of NAGISALAKE_REMOTE_DIR" ;;
esac
case "$config_file" in
    "$remote_dir"/*/*) die "NAGISALAKE_CONFIG_FILE must be a direct child of NAGISALAKE_REMOTE_DIR" ;;
    "$remote_dir"/*) ;;
    *) die "NAGISALAKE_CONFIG_FILE must be a direct child of NAGISALAKE_REMOTE_DIR" ;;
esac
[[ "$port_bind" == 127.0.0.1:9091:9091 ]] \
    || die "NAGISALAKE_PORT_BIND must remain 127.0.0.1:9091:9091 for production Caddy"
[[ "$health_url" == http://127.0.0.1:9091 \
    || "$health_url" == http://127.0.0.1:9091/ ]] \
    || die "NAGISALAKE_HEALTH_URL must remain http://127.0.0.1:9091"
if [[ -n "$public_url" ]]; then
    [[ "$public_url" =~ ^https://[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?(:[0-9]{1,5})?/?$ \
        && "$public_url" != *'..'* ]] \
        || die "the public URL must be an HTTPS origin without credentials, query, or fragment"
fi
command -v ssh >/dev/null 2>&1 || die "ssh is required"
if [[ -n "$public_url" ]]; then
    command -v curl >/dev/null 2>&1 || die "curl is required for the public check"
fi
if [[ -n "$ssh_identity_file" && ! -r "$ssh_identity_file" ]]; then
    die "SSH identity file is not readable: $ssh_identity_file"
fi

local_config_hash=''
if [[ -r "$script_dir/deploy/prod/hub.toml" ]]; then
    if command -v shasum >/dev/null 2>&1; then
        local_config_hash="$(shasum -a 256 "$script_dir/deploy/prod/hub.toml" | awk '{print $1}')"
    elif command -v sha256sum >/dev/null 2>&1; then
        local_config_hash="$(sha256sum "$script_dir/deploy/prod/hub.toml" | awk '{print $1}')"
    fi
fi

ssh_options=(
    -o BatchMode=yes
    -o ConnectTimeout=10
    -o ServerAliveInterval=15
    -o ServerAliveCountMax=3
)
if [[ -n "$ssh_identity_file" ]]; then
    ssh_options+=(-i "$ssh_identity_file")
fi

# OpenSSH sends a remote command through the remote login shell. Quote every
# argument explicitly so environment overrides cannot alter that command.
remote_bash() {
    local remote_command='env -u BASH_ENV -u ENV bash --noprofile --norc -s --'
    local argument quoted
    for argument in "$@"; do
        printf -v quoted '%q' "$argument"
        remote_command+=" $quoted"
    done
    ssh "${ssh_options[@]}" "$target_host" "$remote_command"
}

printf 'Preflight: %s\n' "$target_host"
remote_bash \
    "$remote_dir" "$env_file" "$config_file" "$container_name" "$deploy_mode" \
    "$local_config_hash" <<'REMOTE_PREFLIGHT'
set -Eeuo pipefail
remote_dir="$1"
env_file="$2"
config_file="$3"
container_name="$4"
deploy_mode="$5"
local_config_hash="$6"
previous_name="${container_name}-previous"
lock_file="${remote_dir}/.deploy.lock"

command -v docker >/dev/null
command -v curl >/dev/null
command -v flock >/dev/null
command -v sha256sum >/dev/null
test -d "$remote_dir"
test ! -L "$remote_dir"
test -f "$env_file"
test ! -L "$env_file"
test -r "$env_file"
test -f "$config_file"
test ! -L "$config_file"
test -r "$config_file"
docker info >/dev/null

current_uid="$(id -u)"
remote_dir_mode="$(stat -c '%a' "$remote_dir")"
remote_dir_owner="$(stat -c '%u' "$remote_dir")"
remote_dir_group="$(stat -c '%g' "$remote_dir")"
current_gid="$(id -g)"
remote_dir_mode_decimal=$((8#$remote_dir_mode))
if [[ "$remote_dir_owner" != "$current_uid" && "$remote_dir_owner" != 0 ]] \
    || (( (remote_dir_mode_decimal & 8#002) != 0 )) \
    || { (( (remote_dir_mode_decimal & 8#020) != 0 )) \
        && [[ "$remote_dir_owner" != "$current_uid" \
            || "$remote_dir_group" != "$current_gid" ]]; }; then
    printf 'refusing to use %s: mode=%s owner_uid=%s group_gid=%s; directory is not deployment-user controlled\n' \
        "$remote_dir" "$remote_dir_mode" "$remote_dir_owner" "$remote_dir_group" >&2
    exit 1
fi
if [[ -e "$lock_file" || -L "$lock_file" ]]; then
    if [[ ! -f "$lock_file" || -L "$lock_file" ]]; then
        printf 'refusing to use non-regular or symlink lock file: %s\n' "$lock_file" >&2
        exit 1
    fi
fi

env_mode="$(stat -c '%a' "$env_file")"
env_owner="$(stat -c '%u' "$env_file")"
if [[ "$env_mode" != 600 || "$env_owner" != "$current_uid" ]]; then
    printf 'refusing to use %s: mode=%s owner_uid=%s; expected mode=600 owner_uid=%s\n' \
        "$env_file" "$env_mode" "$env_owner" "$current_uid" >&2
    exit 1
fi
config_mode="$(stat -c '%a' "$config_file")"
config_owner="$(stat -c '%u' "$config_file")"
if [[ "$config_mode" != 644 \
    || ( "$config_owner" != "$current_uid" && "$config_owner" != 0 ) ]]; then
    printf 'refusing to use %s: mode=%s owner_uid=%s; expected mode=644 and trusted owner\n' \
        "$config_file" "$config_mode" "$config_owner" >&2
    exit 1
fi

if docker container inspect "$previous_name" >/dev/null 2>&1; then
    printf 'refusing to continue: rollback container %s already exists\n' \
        "$previous_name" >&2
    exit 1
fi

container_exists=false
if docker container inspect "$container_name" >/dev/null 2>&1; then
    container_exists=true
fi

case "$deploy_mode" in
    fresh)
        [[ "$container_exists" == false ]] \
            || { printf 'fresh mode requires no existing %s container\n' "$container_name" >&2; exit 1; }
        ;;
    upgrade)
        [[ "$container_exists" == true ]] \
            || { printf 'upgrade mode requires an existing %s container\n' "$container_name" >&2; exit 1; }
        ;;
esac

docker_arch="$(docker info --format '{{.Architecture}}')"
case "$docker_arch" in
    arm64|aarch64) ;;
    *)
        printf 'refusing to deploy: Docker host architecture is %s, expected arm64\n' \
            "$docker_arch" >&2
        exit 1
        ;;
esac

config_status=unchecked
if [[ -n "$local_config_hash" ]]; then
    remote_config_hash="$(sha256sum "$config_file" | awk '{print $1}')"
    if [[ "$remote_config_hash" == "$local_config_hash" ]]; then
        config_status=in-sync
    else
        config_status=drifted
    fi
fi

printf 'remote=%s env=%s config=%s config_status=%s arch=%s container_exists=%s\n' \
    "$(hostname)" "$env_file" "$config_file" "$config_status" "$docker_arch" "$container_exists"
REMOTE_PREFLIGHT

if [[ "$dry_run" == true ]]; then
    printf 'mode=%s\nimage=%s\nport=%s\npublic_url=%s\n' \
        "$deploy_mode" "$image_ref" "$port_bind" "${public_url:-<skipped>}"
    printf 'Dry run completed: no login, pull, or container change was performed.\n'
    exit 0
fi

run_remote_deploy() {
remote_bash \
    "$image_ref" "$container_name" "$env_file" "$config_file" \
    "$container_user" "$port_bind" "$health_url" "$deploy_mode" \
    "$readiness_timeout" "$remote_dir" "$force" <<'REMOTE_DEPLOY'
set -Eeuo pipefail
umask 077

image_ref="$1"
container_name="$2"
env_file="$3"
config_file="$4"
container_user="$5"
port_bind="$6"
health_url="${7%/}"
deploy_mode="$8"
readiness_timeout="$9"
remote_dir="${10}"
force="${11}"
previous_name="${container_name}-previous"
lock_file="${remote_dir}/.deploy.lock"
previous_saved=false
old_container_was_running=false
had_existing_container=false
rollout_started=false
health_body=''
ready_body=''

probe_readiness() {
    local deadline="$1"
    local remaining request_timeout
    local health_response ready_response health_status ready_status

    remaining=$((deadline - SECONDS))
    (( remaining > 0 )) || return 1
    request_timeout="$remaining"
    (( request_timeout > 2 )) && request_timeout=2
    health_response="$(
        curl --silent --show-error --max-time "$request_timeout" --write-out $'\n%{http_code}' \
            "$health_url/healthz" 2>/dev/null
    )" || return 1
    health_status="${health_response##*$'\n'}"
    health_body="${health_response%$'\n'*}"
    [[ "$health_status" == 200 ]] || return 1

    remaining=$((deadline - SECONDS))
    (( remaining > 0 )) || return 1
    request_timeout="$remaining"
    (( request_timeout > 2 )) && request_timeout=2
    ready_response="$(
        curl --silent --show-error --max-time "$request_timeout" --write-out $'\n%{http_code}' \
            "$health_url/readyz" 2>/dev/null
    )" || return 1
    ready_status="${ready_response##*$'\n'}"
    ready_body="${ready_response%$'\n'*}"
    [[ "$ready_status" == 200 ]]
}

wait_for_readiness() {
    local deadline=$((SECONDS + readiness_timeout))
    while (( SECONDS < deadline )); do
        if probe_readiness "$deadline"; then
            return 0
        fi
        if (( SECONDS < deadline )); then
            sleep 1
        fi
    done
    return 1
}

rollback() {
    local exit_code=$?
    if (( exit_code == 0 )); then
        return
    fi

    trap - EXIT
    trap '' HUP INT TERM
    set +e

    if [[ "$rollout_started" != true ]]; then
        exit "$exit_code"
    fi
    printf 'deployment failed; attempting rollback\n' >&2

    if docker container inspect "$previous_name" >/dev/null 2>&1; then
        if docker container inspect "$container_name" >/dev/null 2>&1; then
            if ! docker rm -f "$container_name" >/dev/null 2>&1; then
                printf 'rollback=failed-removing-new-container; manual recovery required\n' >&2
                exit "$exit_code"
            fi
        fi
        if ! docker rename "$previous_name" "$container_name" >/dev/null 2>&1; then
            printf 'rollback=failed-restoring-container-name; manual recovery required\n' >&2
            exit "$exit_code"
        fi
        if [[ "$old_container_was_running" == true ]]; then
            if ! docker start "$container_name" >/dev/null 2>&1; then
                printf 'rollback=failed-starting-previous-container; manual recovery required\n' >&2
                exit "$exit_code"
            fi
            if wait_for_readiness; then
                printf 'rollback=ready\n' >&2
            else
                printf 'rollback=failed-readiness-check; manual recovery required\n' >&2
            fi
        else
            printf 'rollback=restored-previous-stopped-container\n' >&2
        fi
    elif [[ "$had_existing_container" == true ]] \
        && docker container inspect "$container_name" >/dev/null 2>&1; then
        if [[ "$old_container_was_running" == true ]]; then
            if docker start "$container_name" >/dev/null 2>&1 && wait_for_readiness; then
                printf 'rollback=ready-after-rename-failure\n' >&2
            else
                printf 'rollback=failed-after-rename-failure; manual recovery required\n' >&2
            fi
        else
            printf 'rollback=kept-previous-stopped-container\n' >&2
        fi
    elif [[ "$had_existing_container" == false ]] \
        && docker container inspect "$container_name" >/dev/null 2>&1; then
        # A fresh deployment has no old container to restore. Remove a
        # partially created replacement so the next fresh run is clean.
        docker rm -f "$container_name" >/dev/null 2>&1 || true
    fi

    exit "$exit_code"
}
trap rollback EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

exec 9>"$lock_file"
if ! flock -n 9; then
    printf 'another deployment holds %s\n' "$lock_file" >&2
    exit 1
fi

if docker container inspect "$previous_name" >/dev/null 2>&1; then
    printf 'refusing to continue: rollback container %s already exists\n' \
        "$previous_name" >&2
    exit 1
fi

container_exists=false
if docker container inspect "$container_name" >/dev/null 2>&1; then
    container_exists=true
fi

case "$deploy_mode" in
    fresh)
        [[ "$container_exists" == false ]] \
            || { printf 'container already exists\n' >&2; exit 1; }
        ;;
    upgrade)
        [[ "$container_exists" == true ]] \
            || { printf 'container does not exist\n' >&2; exit 1; }
        ;;
esac

printf 'Pulling image under deployment lock: %s\n' "$image_ref"
pull_output=''
if ! pull_output="$(docker pull "$image_ref" 2>&1)"; then
    printf '%s\n' "$pull_output" >&2
    if grep -Eiq \
        'unauthorized|authentication required|denied: requested access|pull access denied|insufficient_scope|authorization failed|ghcr\.io.*: denied' \
        <<<"$pull_output"; then
        # The local wrapper treats this status as the sole authorization to
        # transmit an explicitly provided PAT, then reruns this whole locked
        # transaction so a mutable tag cannot change between pull and start.
        exit 42
    fi
    exit 1
fi
printf '%s\n' "$pull_output"

image_id="$(docker image inspect --format '{{.Id}}' "$image_ref")"
[[ "$image_id" =~ ^sha256:[a-fA-F0-9]{64}$ ]] || {
    printf 'remote pull returned an invalid image ID: %s\n' "$image_id" >&2
    exit 1
}
image_os="$(docker image inspect --format '{{.Os}}' "$image_id")"
image_arch="$(docker image inspect --format '{{.Architecture}}' "$image_id")"
if [[ "$image_os" != linux || "$image_arch" != arm64 ]]; then
    printf 'unsupported image platform: %s/%s; expected linux/arm64\n' \
        "$image_os" "$image_arch" >&2
    exit 1
fi
printf 'Resolved immutable image ID: %s\n' "$image_id"

if [[ "$container_exists" == true ]]; then
    current_image_id="$(docker inspect --format '{{.Image}}' "$container_name")"
    current_running="$(docker inspect --format '{{.State.Running}}' "$container_name")"
    current_user="$(docker inspect --format '{{.Config.User}}' "$container_name")"
    current_restart="$(docker inspect --format '{{.HostConfig.RestartPolicy.Name}}' "$container_name")"
    current_port="$(docker inspect --format '{{with (index .HostConfig.PortBindings "9091/tcp")}}{{(index . 0).HostIp}}:{{(index . 0).HostPort}}:9091{{end}}' "$container_name")"
    current_config_mount="$(docker inspect --format '{{range .Mounts}}{{if eq .Destination "/etc/nagisalake/hub.toml"}}{{.Source}}:{{.RW}}{{end}}{{end}}' "$container_name")"
    runtime_contract_matches=false
    if [[ "$current_user" == "$container_user" \
        && "$current_restart" == unless-stopped \
        && "$current_port" == "$port_bind" \
        && "$current_config_mount" == "$config_file:false" ]]; then
        runtime_contract_matches=true
    fi
    if [[ "$force" != true && "$current_running" == true \
        && "$current_image_id" == "$image_id" \
        && "$runtime_contract_matches" == true ]]; then
        if ! wait_for_readiness; then
            printf 'the requested image is already running but is not ready\n' >&2
            exit 1
        fi
        trap - EXIT HUP INT TERM
        printf 'deployment=no-op reason=requested-image-already-running\n'
        printf 'healthz=%s\nreadyz=%s\n' "$health_body" "$ready_body"
        docker inspect "$container_name" \
            --format 'container={{.Name}} status={{.State.Status}} image_id={{.Image}} restart={{.HostConfig.RestartPolicy.Name}}' \
            || printf 'warning: unable to print final container details\n' >&2
        docker image inspect "$image_id" --format 'repo_digests={{json .RepoDigests}}' \
            || printf 'warning: unable to print image digest details\n' >&2
        exit 0
    fi
    had_existing_container=true
    old_container_was_running="$current_running"
fi

rollout_started=true
if [[ "$container_exists" == true ]]; then
    if [[ "$old_container_was_running" == true ]]; then
        docker stop --timeout 30 "$container_name" >/dev/null
    fi
    docker rename "$container_name" "$previous_name"
    previous_saved=true
fi

if docker run -d \
    --pull never \
    --name "$container_name" \
    --restart unless-stopped \
    --user "$container_user" \
    --env-file "$env_file" \
    --publish "$port_bind" \
    --volume "$config_file:/etc/nagisalake/hub.toml:ro" \
    "$image_id" \
    --config /etc/nagisalake/hub.toml >/dev/null; then
    :
else
    false
fi

if ! wait_for_readiness; then
    printf 'Hub did not become ready within %s seconds\n' "$readiness_timeout" >&2
    docker inspect "$container_name" \
        --format 'status={{.State.Status}} image={{.Image}}' >&2 || true
    printf 'Inspect logs privately: docker logs --tail 100 %s\n' "$container_name" >&2
    exit 1
fi

container_image_id="$(docker inspect --format '{{.Image}}' "$container_name")"
if [[ "$container_image_id" != "$image_id" ]]; then
    printf 'container image mismatch: expected %s, got %s\n' \
        "$image_id" "$container_image_id" >&2
    exit 1
fi

# Readiness succeeded. A cleanup failure must not replace the healthy new Hub
# with the old one, so disable rollback before removing the stopped container.
trap - EXIT HUP INT TERM
if [[ "$previous_saved" == true ]]; then
    if ! docker rm "$previous_name" >/dev/null; then
        printf 'warning: healthy rollout completed, but failed to remove %s\n' \
            "$previous_name" >&2
    fi
fi

printf 'healthz=%s\nreadyz=%s\n' "$health_body" "$ready_body"
docker inspect "$container_name" \
    --format 'container={{.Name}} status={{.State.Status}} image_id={{.Image}} restart={{.HostConfig.RestartPolicy.Name}}' \
    || printf 'warning: unable to print final container details\n' >&2
docker image inspect "$image_id" --format 'repo_digests={{json .RepoDigests}}' \
    || printf 'warning: unable to print image digest details\n' >&2
REMOTE_DEPLOY
}

printf 'Deploying image: %s\n' "$image_ref"
set +e
run_remote_deploy
deployment_status=$?
set -e

if (( deployment_status == 42 )); then
    if [[ -z "${GITHUB_PAT:-}" ]]; then
        die "GHCR rejected the image pull; authenticate Docker on the host or export GITHUB_PAT"
    fi

    printf 'GHCR requested authentication; logging in through stdin and retrying.\n'
    set +x
    if ! printf '%s' "$GITHUB_PAT" | ssh "${ssh_options[@]}" "$target_host" \
        "env -u BASH_ENV -u ENV bash --noprofile --norc -c 'set -Eeuo pipefail; set +x; docker login ghcr.io --username tokimorphling --password-stdin >/dev/null'"; then
        die "GHCR authentication failed"
    fi

    set +e
    run_remote_deploy
    deployment_status=$?
    set -e
    if (( deployment_status == 42 )); then
        die "GHCR still rejected the image pull after authentication"
    fi
fi

if (( deployment_status != 0 )); then
    exit "$deployment_status"
fi

if [[ -n "$public_url" ]]; then
    public_url="${public_url%/}"
    printf 'Public health check: %s\n' "$public_url"
    if ! public_health="$(curl --proto '=https' --silent --show-error --fail \
        --retry 2 --retry-all-errors --retry-delay 1 --connect-timeout 5 --max-time 15 \
        --write-out $'\n%{http_code}' "$public_url/healthz")"; then
        die "local rollout is healthy, but the public /healthz request failed"
    fi
    public_health_status="${public_health##*$'\n'}"
    public_health="${public_health%$'\n'*}"
    [[ "$public_health_status" == 200 ]] \
        || die "local rollout is healthy, but public /healthz returned HTTP $public_health_status"
    if ! public_ready="$(curl --proto '=https' --silent --show-error --fail \
        --retry 2 --retry-all-errors --retry-delay 1 --connect-timeout 5 --max-time 15 \
        --write-out $'\n%{http_code}' "$public_url/readyz")"; then
        die "local rollout is healthy, but the public /readyz request failed"
    fi
    public_ready_status="${public_ready##*$'\n'}"
    public_ready="${public_ready%$'\n'*}"
    [[ "$public_ready_status" == 200 ]] \
        || die "local rollout is healthy, but public /readyz returned HTTP $public_ready_status"
    printf 'public_healthz=%s\npublic_readyz=%s\n' "$public_health" "$public_ready"
fi

printf 'Deployment completed successfully.\n'
