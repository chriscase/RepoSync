#!/usr/bin/env bash
# One product-image UI/API qualification. Synthetic config only.
# Does not install Docker, push an image, or use a live SVN or Git host.
# A 200 from the SPA fallback is not acceptance. The asset named by the
# built index must be fetched, and it must be the production bundle.
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd -- "$ROOT" && pwd)"
# shellcheck source=docker_ui_qualify_decisions.sh
source "${ROOT}/scripts/docker_ui_qualify_decisions.sh"
SOURCE="$(git -C "$ROOT" rev-parse HEAD)"
TREE="$(git -C "$ROOT" rev-parse 'HEAD^{tree}')"
SLOT="rs118ui-$$"
NET="${SLOT}-net"
IMAGE="reposync-ui-qualify:${SOURCE}"
WORKDIR="$(mktemp -d /tmp/${SLOT}.XXXXXX)"
RECEIPT="${WORKDIR}/receipt"
CONTAINERS=()
NETWORK_CREATED=0
BUILD_TIMEOUT="${BUILD_TIMEOUT:-1800}"
START_TIMEOUT="${START_TIMEOUT:-60}"
FAIL_TIMEOUT="${FAIL_TIMEOUT:-20}"
CLEANUP_STOP_TIMEOUT="${CLEANUP_STOP_TIMEOUT:-20}"
CLEANUP_WAIT_TIMEOUT="${CLEANUP_WAIT_TIMEOUT:-15}"
CLEANUP_RM_TIMEOUT="${CLEANUP_RM_TIMEOUT:-15}"
CLEANUP_INSPECT_TIMEOUT="${CLEANUP_INSPECT_TIMEOUT:-15}"

receipt() { printf '%s\n' "$*" | tee -a "$RECEIPT"; }

# run_bounded SECONDS OUT_VAR ERR_VAR -- CMD...
# Every cleanup and config observation goes through this bound.
run_bounded() {
    local seconds="$1"
    local __out="$2"
    local __err="$3"
    shift 3
    local out_file="" err_file="" rc=0 out_data="" err_data=""
    out_file="$(mktemp "${WORKDIR}/obs.XXXXXX")" || return 125
    err_file="$(mktemp "${WORKDIR}/obs.XXXXXX")" || {
        rm -f "$out_file"
        return 125
    }
    timeout "$seconds" "$@" >"$out_file" 2>"$err_file" || rc=$?
    out_data="$(cat "$out_file" 2>/dev/null || true)"
    err_data="$(cat "$err_file" 2>/dev/null || true)"
    rm -f "$out_file" "$err_file"
    printf -v "$__out" '%s' "$out_data"
    printf -v "$__err" '%s' "$err_data"
    return "$rc"
}

cleanup() {
    local prior=$?
    trap - EXIT
    set +e
    local name cout="" cerr="" crc=0 class="" status_json="" state=""
    local stop_rc=0 wait_rc=0 rm_out="" rm_err="" rm_rc=0
    local post_out="" post_err="" post_rc=0 post_class="" removal=""
    local container_fold="none" network_fold="not_created" saw_container=0
    local net_out="" net_err="" net_rc=0 result="" final="" token=""
    for name in "${CONTAINERS[@]}"; do
        saw_container=1
        cout=""
        cerr=""
        run_bounded "$CLEANUP_INSPECT_TIMEOUT" cout cerr \
            docker inspect --format '{{.Id}} {{json .State.Status}}' "$name"
        crc=$?
        class="$(classify_inspect_presence "$crc" "$cout" "$cerr" || true)"
        class="${class%$'\n'}"
        receipt "cleanup_inspect ${name} rc=${crc} class=${class}"
        if [[ -n "$cerr" ]]; then
            receipt "cleanup_inspect_err ${name} ${cerr}"
        fi
        if [[ "$class" == "absent" ]]; then
            receipt "cleanup_container_absent ${name}"
            continue
        fi
        state=""
        if [[ "$class" == "present" ]]; then
            status_json="${cout#* }"
            state="$(python3 -c 'import json,sys; sys.stdout.write(json.loads(sys.argv[1]))' "$status_json" || true)"
            receipt "cleanup_container_before ${name} ${state:-unknown}"
        else
            receipt "cleanup_container_unknown ${name} ${class}"
        fi
        stop_rc=0
        if [[ "$state" == "running" || "$state" == "paused" || "$state" == "restarting" || "$class" != "present" ]]; then
            run_bounded "$CLEANUP_STOP_TIMEOUT" cout cerr docker stop -t 10 "$name"
            stop_rc=$?
            if [[ "$stop_rc" != "0" ]]; then
                receipt "cleanup_stop_failed ${name} rc=${stop_rc}"
            else
                receipt "cleanup_stop_ok ${name}"
            fi
        else
            receipt "cleanup_stop_not_needed ${name} state=${state}"
        fi
        if cleanup_may_wait_after_stop "$stop_rc"; then
            run_bounded "$CLEANUP_WAIT_TIMEOUT" cout cerr docker wait "$name"
            wait_rc=$?
            if [[ "$wait_rc" != "0" ]]; then
                receipt "cleanup_wait_failed ${name} rc=${wait_rc}"
            else
                receipt "cleanup_wait_ok ${name}"
            fi
        else
            receipt "cleanup_wait_skipped ${name} stop_rc=${stop_rc}"
        fi
        run_bounded "$CLEANUP_RM_TIMEOUT" rm_out rm_err docker rm -f "$name"
        rm_rc=$?
        if [[ "$rm_rc" != "0" ]]; then
            receipt "cleanup_rm_failed ${name} rc=${rm_rc}"
            if [[ -n "$rm_err" ]]; then
                receipt "cleanup_rm_err ${name} ${rm_err}"
            fi
        fi
        run_bounded "$CLEANUP_INSPECT_TIMEOUT" post_out post_err \
            docker inspect --format '{{.Id}}' "$name"
        post_rc=$?
        post_class="$(classify_inspect_presence "$post_rc" "$post_out" "$post_err" || true)"
        post_class="${post_class%$'\n'}"
        removal="$(cleanup_removal_result "$rm_rc" "$post_class" || true)"
        removal="${removal%$'\n'}"
        receipt "cleanup_after ${name} inspect_rc=${post_rc} class=${post_class} removal=${removal}"
        if [[ -n "$post_err" ]]; then
            receipt "cleanup_after_err ${name} ${post_err}"
        fi
        if [[ "$removal" == "removed" ]]; then
            receipt "cleanup_container_removed ${name}"
        else
            receipt "cleanup_container_unresolved ${name} ${removal}"
            if [[ "$post_class" == "present" ]]; then
                container_fold="present"
            elif [[ "$container_fold" != "present" ]]; then
                container_fold="unknown"
            fi
        fi
    done
    if [[ "$saw_container" == "0" ]]; then
        container_fold="none"
    elif [[ "$container_fold" == "none" ]]; then
        container_fold="absent"
    fi
    if [[ "$NETWORK_CREATED" == "1" ]]; then
        run_bounded "$CLEANUP_RM_TIMEOUT" net_out net_err docker network rm "$NET"
        net_rc=$?
        receipt "cleanup_network_rm ${NET} rc=${net_rc}"
        if [[ -n "$net_err" ]]; then
            receipt "cleanup_network_rm_err ${NET} ${net_err}"
        fi
        run_bounded "$CLEANUP_INSPECT_TIMEOUT" net_out net_err docker network inspect "$NET"
        net_rc=$?
        network_fold="$(classify_inspect_presence "$net_rc" "$net_out" "$net_err" || true)"
        network_fold="${network_fold%$'\n'}"
        receipt "cleanup_network_inspect ${NET} rc=${net_rc} class=${network_fold}"
        if [[ -n "$net_err" ]]; then
            receipt "cleanup_network_inspect_err ${NET} ${net_err}"
        fi
    else
        network_fold="not_created"
        receipt "cleanup_network_not_created ${NET}"
    fi
    result="$(finalize_cleanup_result "$prior" "$container_fold" "$network_fold" || true)"
    result="${result%$'\n'}"
    final="${result%% *}"
    token="${result#* }"
    receipt "cleanup_classes container=${container_fold} network=${network_fold}"
    receipt "${token}"
    if [[ "$final" == "0" && "$token" == "cleanup_resolved" ]]; then
        receipt "RESULT pass image=${IMAGE_ID:-unset}"
    else
        receipt "RESULT fail prior=${prior} exit=${final} token=${token}"
    fi
    if [[ -f "$RECEIPT" ]]; then
        cat "$RECEIPT"
    fi
    if [[ "$token" == "cleanup_unresolved" ]]; then
        printf 'cleanup_evidence_retained %s\n' "$WORKDIR" | tee -a "$RECEIPT"
    else
        rm -rf "$WORKDIR"
    fi
    exit "$final"
}
trap cleanup EXIT

: >"$RECEIPT"
receipt "source ${SOURCE}"
receipt "tree ${TREE}"
receipt "slot ${SLOT}"
receipt "host_arch $(uname -m)"
receipt "limits cpus=1 memory=512m memory-swap=512m pids=128 data-tmpfs=64m build-timeout=${BUILD_TIMEOUT}s start-timeout=${START_TIMEOUT}s fail-timeout=${FAIL_TIMEOUT}s cleanup-stop=${CLEANUP_STOP_TIMEOUT}s cleanup-wait=${CLEANUP_WAIT_TIMEOUT}s cleanup-rm=${CLEANUP_RM_TIMEOUT}s cleanup-inspect=${CLEANUP_INSPECT_TIMEOUT}s"
receipt "platform_note aarch64 and Darwin are not accepted from this run"

bash "${ROOT}/scripts/qualify_docker_ui_authored_checks.sh" >>"$RECEIPT" 2>&1
receipt "authored_checks_ok"

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    receipt "BLOCKER no-admitted-docker-daemon"
    exit 2
fi
receipt "docker_server_arch $(docker version --format '{{.Server.Arch}}')"

cp "${ROOT}/scripts/fixtures/docker-ui-qualify-synthetic.toml" "${WORKDIR}/synthetic.toml"
cp "${ROOT}/scripts/fixtures/docker-ui-qualify-invalid.toml" "${WORKDIR}/invalid.toml"

docker network create --driver bridge "$NET" >>"$RECEIPT"
NETWORK_CREATED=1
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
    local kind="$2"
    shift 2
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
    local cid=""
    if ! cid="$(docker run -d \
        --name "$name" \
        --network "$NET" \
        --cpus=1 \
        --memory=256m \
        --memory-swap=256m \
        --pids-limit=64 \
        "${docker_opts[@]}" \
        "$IMAGE_ID" \
        "${daemon_args[@]}")"; then
        receipt "FAIL ${name} docker_run_failed"
        return 1
    fi
    receipt "fail_case ${name} container ${cid}"

    local wait_rc=0
    local status=""
    set +e
    status="$(timeout "$FAIL_TIMEOUT" docker wait "$cid")"
    wait_rc=$?
    set -e
    status="${status//$'\n'/}"

    local inspect_rc=0 logs_rc=0
    local inspect_body="" inspect_err="" logs_body="" logs_err=""
    set +e
    run_bounded "$FAIL_TIMEOUT" inspect_body inspect_err \
        docker inspect --format '{{json .State.Status}} {{json .State.ExitCode}} {{json .State.OOMKilled}} {{json .State.Error}}' "$name"
    inspect_rc=$?
    run_bounded "$FAIL_TIMEOUT" logs_body logs_err docker logs "$name"
    logs_rc=$?
    set -e
    printf '%s\n' "$logs_body" >>"$RECEIPT"
    if [[ -n "$logs_err" ]]; then
        printf '%s\n' "$logs_err" >>"$RECEIPT"
    fi
    if [[ -n "$inspect_err" ]]; then
        printf '%s\n' "$inspect_err" >>"$RECEIPT"
    fi

    local decision
    decision="$(accept_config_observations "$kind" "$wait_rc" "$inspect_rc" "$inspect_body" "$logs_rc" "$logs_body" "$inspect_err" || true)"
    decision="${decision%$'\n'}"
    receipt "fail_case ${name} kind=${kind} wait_rc=${wait_rc} wait_status=${status:-empty} inspect_rc=${inspect_rc} logs_rc=${logs_rc} decision=${decision}"
    if [[ "$decision" != "qualify" ]]; then
        receipt "FAIL ${name} ${decision}"
        return 1
    fi
    receipt "fail_case ${name} qualified ${kind}"
}

# Missing file: load_from_file returns FileNotFound before listen.
expect_config_failure "${SLOT}-missing" missing_file -- --config /etc/reposync/missing.toml
# poll_interval_secs = 0 fails validate before listen.
expect_config_failure "${SLOT}-invalid" zero_poll \
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

index_body="${WORKDIR}/missing-static-index"
health_body="${WORKDIR}/missing-static-health"
index_rc=0
index_code=""
set +e
index_code="$(curl -sS -o "$index_body" -w '%{http_code}' --max-time 5 "http://127.0.0.1:${NO_PORT}/")"
index_rc=$?
set -e
index_transport=0
if [[ "$index_rc" == "0" && "$index_code" =~ ^[0-9]{3}$ && "$index_code" != "000" ]]; then
    index_transport=1
fi

health_rc=0
health_code=""
set +e
health_code="$(curl -sS -o "$health_body" -w '%{http_code}' --max-time 5 "http://127.0.0.1:${NO_PORT}/api/status/health")"
health_rc=$?
set -e
health_transport=0
if [[ "$health_rc" == "0" && "$health_code" =~ ^[0-9]{3}$ && "$health_code" != "000" ]]; then
    health_transport=1
fi
health_ok="false"
if [[ "$health_transport" == "1" && "$health_code" == "200" ]]; then
    health_ok="$(python3 - "$health_body" <<'PY'
import json, sys
try:
    body = json.load(open(sys.argv[1]))
except Exception:
    print("false")
    raise SystemExit(0)
ok = (
    body.get("ok") is True
    and "version" in body
    and "git_commit" in body
    and "status" not in body
)
print("true" if ok else "false")
PY
)"
fi
static_decision="$(missing_static_qualifies "$index_transport" "$index_code" "$health_transport" "$health_code" "$health_ok" || true)"
static_decision="${static_decision%$'\n'}"
receipt "missing_static_index transport=${index_transport} rc=${index_rc} status=${index_code:-empty}"
receipt "missing_static_health transport=${health_transport} rc=${health_rc} status=${health_code:-empty} ok=${health_ok}"
receipt "missing_static_decision ${static_decision}"
if [[ "$static_decision" != "qualify" ]]; then
    receipt "FAIL missing static ${static_decision}"
    exit 1
fi
receipt "missing_static_rejected"

exit 0
