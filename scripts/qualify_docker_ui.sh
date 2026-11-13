#!/usr/bin/env bash
# One product-image UI/API qualification. Synthetic config only.
# Does not install Docker, push an image, or use a live SVN or Git host.
# A 200 from the SPA fallback is not acceptance. The asset named by the
# built index must be fetched, and it must be the production bundle.
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd -- "$ROOT" && pwd)"
SOURCE="$(git -C "$ROOT" rev-parse HEAD)"
TREE="$(git -C "$ROOT" rev-parse 'HEAD^{tree}')"
SLOT="rs118ui-$$"
NET="${SLOT}-net"
IMAGE="reposync-ui-qualify:${SOURCE}"
WORKDIR="$(mktemp -d /tmp/${SLOT}.XXXXXX)"
RECEIPT="${WORKDIR}/receipt"
CONTAINERS=()
BUILD_TIMEOUT="${BUILD_TIMEOUT:-1800}"
START_TIMEOUT="${START_TIMEOUT:-60}"
FAIL_TIMEOUT="${FAIL_TIMEOUT:-20}"

receipt() { printf '%s\n' "$*" | tee -a "$RECEIPT"; }

cleanup() {
    local ec=$?
    local name
    for name in "${CONTAINERS[@]}"; do
        docker stop -t 10 "$name" >>"$RECEIPT" 2>&1 || true
        docker wait "$name" >>"$RECEIPT" 2>&1 || true
        docker rm -f "$name" >>"$RECEIPT" 2>&1 || true
    done
    docker network rm "$NET" >>"$RECEIPT" 2>&1 || true
    if [[ -f "$RECEIPT" ]]; then
        cat "$RECEIPT"
    fi
    rm -rf "$WORKDIR"
    exit "$ec"
}
trap cleanup EXIT

: >"$RECEIPT"
receipt "source ${SOURCE}"
receipt "tree ${TREE}"
receipt "slot ${SLOT}"
receipt "host_arch $(uname -m)"
receipt "limits cpus=1 memory=512m memory-swap=512m pids=128 data-tmpfs=64m build-timeout=${BUILD_TIMEOUT}s start-timeout=${START_TIMEOUT}s fail-timeout=${FAIL_TIMEOUT}s"
receipt "platform_note aarch64 and Darwin are not accepted from this run"

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    receipt "BLOCKER no-admitted-docker-daemon"
    exit 2
fi
receipt "docker_server_arch $(docker version --format '{{.Server.Arch}}')"

cat >"${WORKDIR}/synthetic.toml" <<'EOF'
[daemon]
poll_interval_secs = 3600
log_level = "error"
data_dir = "/var/lib/reposync"

[svn]
url = "file:///var/lib/reposync/no-such-repo"
username = "synthetic"

[github]
api_url = "http://127.0.0.1:9"
repo = "local/synthetic"

[web]
listen = "0.0.0.0:8080"
auth_mode = "simple"

[identity]
email_domain = "example.invalid"

[sync]
mode = "direct"
auto_merge = false
sync_branches = false
sync_tags = false
EOF

cat >"${WORKDIR}/invalid.toml" <<'EOF'
[daemon]
poll_interval_secs = 0
log_level = "info"
data_dir = "/var/lib/reposync"

[svn]
url = "file:///var/lib/reposync/no-such-repo"
username = "synthetic"

[github]
repo = "local/synthetic"
EOF

docker network create --driver bridge "$NET" >>"$RECEIPT"
timeout "$BUILD_TIMEOUT" docker build --pull=false -t "$IMAGE" "$ROOT" >>"$RECEIPT"
IMAGE_ID="$(docker image inspect --format '{{.Id}}' "$IMAGE")"
receipt "image_id ${IMAGE_ID}"

run_container() {
    local name="$1"
    shift
    local -a docker_opts=()
    local -a daemon_args=()
    local passed_separator=0
    local arg
    for arg in "$@"; do
        if [[ "$arg" == "--" ]]; then
            passed_separator=1
            continue
        fi
        if [[ "$passed_separator" -eq 0 ]]; then
            docker_opts+=("$arg")
        else
            daemon_args+=("$arg")
        fi
    done
    CONTAINERS+=("$name")
    docker run -d \
        --name "$name" \
        --network "$NET" \
        --cpus=1 \
        --memory=512m \
        --memory-swap=512m \
        --pids-limit=128 \
        --tmpfs /var/lib/reposync:rw,size=64m,mode=1777 \
        -p 127.0.0.1::8080 \
        -v "${WORKDIR}/synthetic.toml:/etc/reposync/synthetic.toml:ro" \
        -v "${WORKDIR}/invalid.toml:/etc/reposync/invalid.toml:ro" \
        "${docker_opts[@]}" \
        "$IMAGE_ID" \
        "${daemon_args[@]}" >>"$RECEIPT"
}

host_port() {
    docker port "$1" 8080/tcp | awk -F: 'NR==1 { print $NF }'
}

wait_listen() {
    local port="$1"
    local deadline=$((SECONDS + START_TIMEOUT))
    while (( SECONDS < deadline )); do
        if curl -fsS --max-time 2 "http://127.0.0.1:${port}/api/status/health" >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    return 1
}

expect_config_failure() {
    local name="$1"
    shift
    local -a docker_opts=()
    local -a daemon_args=()
    local passed_separator=0
    local arg
    for arg in "$@"; do
        if [[ "$arg" == "--" ]]; then
            passed_separator=1
            continue
        fi
        if [[ "$passed_separator" -eq 0 ]]; then
            docker_opts+=("$arg")
        else
            daemon_args+=("$arg")
        fi
    done
    CONTAINERS+=("$name")
    local cid
    cid="$(docker run -d \
        --name "$name" \
        --network "$NET" \
        --cpus=1 \
        --memory=256m \
        --memory-swap=256m \
        --pids-limit=64 \
        "${docker_opts[@]}" \
        "$IMAGE_ID" \
        "${daemon_args[@]}")"
    local status
    status="$(timeout "$FAIL_TIMEOUT" docker wait "$cid" || true)"
    receipt "fail_case ${name} exit ${status:-timeout}"
    docker logs --tail 80 "$name" >>"$RECEIPT" 2>&1 || true
    if [[ -z "$status" || "$status" == "0" ]]; then
        receipt "FAIL ${name} expected a non-zero config exit"
        return 1
    fi
}

# Missing file: load_from_file returns FileNotFound before listen.
expect_config_failure "${SLOT}-missing" -- --config /etc/reposync/missing.toml
# poll_interval_secs = 0 fails validate before listen.
expect_config_failure "${SLOT}-invalid" \
    -v "${WORKDIR}/invalid.toml:/etc/reposync/invalid.toml:ro" \
    -- --config /etc/reposync/invalid.toml

run_container "${SLOT}-ok" -- --config /etc/reposync/synthetic.toml
PORT="$(host_port "${SLOT}-ok")"
receipt "loopback_port ${PORT}"
[[ "$PORT" =~ ^[0-9]+$ ]] || { receipt "FAIL host port was not loopback-ephemeral"; exit 1; }
wait_listen "$PORT" || { receipt "FAIL health did not listen"; docker logs --tail 80 "${SLOT}-ok" >>"$RECEIPT"; exit 1; }

python3 - "$PORT" <<'PY'
import json, sys, urllib.request
port = sys.argv[1]
url = f"http://127.0.0.1:{port}/api/status/health"
with urllib.request.urlopen(url, timeout=5) as response:
    body = json.load(response)
    status = response.status
if status != 200 or body.get("ok") is not True or "version" not in body or "git_commit" not in body:
    raise SystemExit(f"health JSON is not a running API response: {status} {body}")
if "status" in body:
    raise SystemExit(f"health is not the running scheduler body: {body}")
print(f"health_ok version={body['version']}")
PY

python3 - "$PORT" <<'PY'
import re, sys, urllib.request
port = sys.argv[1]
index_url = f"http://127.0.0.1:{port}/"
with urllib.request.urlopen(index_url, timeout=5) as response:
    index_status = response.status
    content_type = response.headers.get("content-type", "")
    index = response.read().decode("utf-8", "replace")
if index_status != 200 or "text/html" not in content_type:
    raise SystemExit(f"index was not production HTML: {index_status} {content_type}")
# An unknown route also falls back to index.html with 200. That is not the asset.
with urllib.request.urlopen(f"http://127.0.0.1:{port}/not-a-built-asset", timeout=5) as response:
    fallback = response.read()
if fallback.decode("utf-8", "replace") == index:
    print("spa_fallback_observed_not_accepted")
match = re.search(r'src="(/assets/[^"]+)"', index)
if not match:
    raise SystemExit("production index does not name a built asset")
asset_url = f"http://127.0.0.1:{port}{match.group(1)}"
with urllib.request.urlopen(asset_url, timeout=5) as response:
    asset_status = response.status
    asset = response.read().decode("utf-8", "replace")
sentinel = 'window.reposyncStatic = "synthetic"'
if asset_status != 200 or "Welcome to RepoSync" not in asset or sentinel in asset:
    raise SystemExit(f"asset is not the production bundle: {asset_status}")
print(f"asset_ok {match.group(1)}")
PY
receipt "positive_ui_api_ok"

# Hide the image static directory. Health may still return 200; that is not a UI pass.
# ServeDir then falls through, and the missing index.html handler returns 500.
run_container "${SLOT}-nostatic" \
    --tmpfs /usr/local/bin/static:rw,size=1m,mode=555 \
    -- --config /etc/reposync/synthetic.toml
NO_PORT="$(host_port "${SLOT}-nostatic")"
wait_listen "$NO_PORT" || { receipt "FAIL no-static health did not listen"; exit 1; }
NO_STATUS="$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "http://127.0.0.1:${NO_PORT}/" || true)"
receipt "missing_static_index_status ${NO_STATUS}"
if [[ "$NO_STATUS" == "200" ]]; then
    receipt "FAIL missing static index was accepted as a page"
    exit 1
fi
receipt "missing_static_rejected"

receipt "RESULT pass image=${IMAGE_ID}"
