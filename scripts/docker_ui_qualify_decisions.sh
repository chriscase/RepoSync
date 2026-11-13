#!/usr/bin/env bash
# Pure pass/fail decisions for product-image qualification.
# Sourcing this file does not build, start, or clean a container.

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    printf 'source this file; it does not qualify an image\n' >&2
    exit 2
fi

# config_failure_qualifies KIND WAIT_RC EXIT_CODE STATE OOM DOCKER_ERROR LOG
# Prints "qualify" or "refuse <reason>". Returns 0 only for qualify.
# WAIT_RC is the docker-wait command status: 0 observed an exit, 124 timed out.
# Only exit 1 plus the named diagnostic qualifies. OOM, panic, Docker errors,
# timeouts, and any other exit do not.
config_failure_qualifies() {
    local kind="$1"
    local wait_rc="$2"
    local exit_code="$3"
    local state="$4"
    local oom="$5"
    local docker_error="$6"
    local log="$7"

    if [[ "$wait_rc" == "124" ]]; then
        printf 'refuse wait_timeout\n'
        return 1
    fi
    if [[ "$wait_rc" != "0" ]]; then
        printf 'refuse docker_wait_failed\n'
        return 1
    fi
    if [[ "$state" != "exited" ]]; then
        printf 'refuse container_state_%s\n' "$state"
        return 1
    fi
    if [[ "$oom" != "false" ]]; then
        printf 'refuse oom\n'
        return 1
    fi
    if [[ -n "$docker_error" ]]; then
        printf 'refuse docker_error\n'
        return 1
    fi
    if [[ "$log" == *"panicked at"* || "$log" == *"stack backtrace"* || "$log" == *"fatal runtime error"* ]]; then
        printf 'refuse panic\n'
        return 1
    fi
    if [[ "$exit_code" != "1" ]]; then
        printf 'refuse exit_%s\n' "$exit_code"
        return 1
    fi

    case "$kind" in
        missing_file)
            if [[ "$log" != *"configuration file not found:"* ]]; then
                printf 'refuse missing_file_diagnostic\n'
                return 1
            fi
            if [[ "$log" == *"poll interval must be > 0"* ]]; then
                printf 'refuse unexpected_poll_diagnostic\n'
                return 1
            fi
            ;;
        zero_poll)
            if [[ "$log" != *"daemon.poll_interval_secs"* || "$log" != *"poll interval must be > 0"* ]]; then
                printf 'refuse zero_poll_diagnostic\n'
                return 1
            fi
            if [[ "$log" == *"configuration file not found:"* ]]; then
                printf 'refuse unexpected_missing_file_diagnostic\n'
                return 1
            fi
            ;;
        *)
            printf 'refuse unknown_kind\n'
            return 1
            ;;
    esac

    printf 'qualify\n'
    return 0
}

# missing_static_qualifies INDEX_TRANSPORT INDEX_CODE HEALTH_TRANSPORT HEALTH_CODE HEALTH_OK
# INDEX_TRANSPORT/HEALTH_TRANSPORT are 1 only after curl completes.
# HTTP 000, non-500 index statuses, and an unhealthy API do not qualify.
missing_static_qualifies() {
    local index_transport="$1"
    local index_code="$2"
    local health_transport="$3"
    local health_code="$4"
    local health_ok="$5"

    if [[ "$index_transport" != "1" || "$index_code" == "000" || -z "$index_code" ]]; then
        printf 'refuse index_transport\n'
        return 1
    fi
    if [[ "$index_code" != "500" ]]; then
        printf 'refuse index_status_%s\n' "$index_code"
        return 1
    fi
    if [[ "$health_transport" != "1" || "$health_code" == "000" || -z "$health_code" ]]; then
        printf 'refuse health_transport\n'
        return 1
    fi
    if [[ "$health_code" != "200" || "$health_ok" != "true" ]]; then
        printf 'refuse health_status\n'
        return 1
    fi
    printf 'qualify\n'
    return 0
}

# cleanup_final_status PRIOR_EXIT UNRESOLVED
# Unresolved removal cannot remain success. An earlier failure is kept.
cleanup_final_status() {
    local prior="$1"
    local unresolved="$2"
    if [[ "$prior" != "0" ]]; then
        printf '%s\n' "$prior"
        return 0
    fi
    if [[ "$unresolved" != "0" ]]; then
        printf '1\n'
        return 0
    fi
    printf '0\n'
    return 0
}

# A failed stop must not be followed by docker wait. Wait would block while
# the container is still running.
cleanup_may_wait_after_stop() {
    local stop_rc="$1"
    [[ "$stop_rc" == "0" ]]
}

# observation_failure_class RC STDOUT STDERR
# Names a failed Docker observation. This does not confirm absence.
observation_failure_class() {
    local rc="$1"
    local out="$2"
    local err="$3"
    local blob="${out}"$'\n'"${err}"
    if [[ "$rc" == "124" ]]; then
        printf 'timeout\n'
        return 0
    fi
    if [[ "$blob" == *"permission denied"* ]]; then
        printf 'permission\n'
        return 0
    fi
    if [[ "$blob" == *"Cannot connect to the Docker daemon"* || "$blob" == *"Is the docker daemon running"* ]]; then
        printf 'daemon\n'
        return 0
    fi
    if [[ "$blob" == *"error during connect"* || "$blob" == *"dial unix"* || "$blob" == *"connection refused"* ]]; then
        printf 'transport\n'
        return 0
    fi
    printf 'failed\n'
}

# parse_complete_state BODY
# BODY is one docker inspect format:
#   {{json .State.Status}} {{json .State.ExitCode}} {{json .State.OOMKilled}} {{json .State.Error}}
# Prints status, exit, oom, and a JSON error string separated by tabs.
parse_complete_state() {
    local body="$1"
    local parsed=""
    local rc=0
    parsed="$(python3 -c '
import json, sys
dec = json.JSONDecoder()
raw = sys.argv[1].strip()
vals = []
while raw:
    raw = raw.lstrip()
    if not raw:
        break
    try:
        val, end = dec.raw_decode(raw)
    except json.JSONDecodeError:
        raise SystemExit(1)
    vals.append(val)
    raw = raw[end:]
if len(vals) != 4:
    raise SystemExit(1)
status, code, oom, err = vals
if (
    not isinstance(status, str)
    or isinstance(code, bool)
    or not isinstance(code, int)
    or not isinstance(oom, bool)
    or not isinstance(err, str)
):
    raise SystemExit(1)
sys.stdout.write("%s\t%s\t%s\t%s" % (status, code, "true" if oom else "false", json.dumps(err)))
' "$body")" || rc=$?
    if [[ "$rc" != "0" || -z "$parsed" ]]; then
        return 1
    fi
    printf '%s' "$parsed"
}

# stream_panics TEXT
stream_panics() {
    local text="$1"
    [[ "$text" == *"panicked at"* || "$text" == *"stack backtrace"* || "$text" == *"fatal runtime error"* ]]
}

# config_streams_qualify KIND WAIT_RC EXIT_CODE STATE OOM DOCKER_ERROR STDOUT STDERR
# STDOUT and STDERR stay separate. A phrase split across them does not match.
# Panic on either stream refuses. The expected diagnostic may be on either one.
config_streams_qualify() {
    local kind="$1"
    local wait_rc="$2"
    local exit_code="$3"
    local state="$4"
    local oom="$5"
    local docker_error="$6"
    local logs_stdout="$7"
    local logs_stderr="$8"
    local structural=""
    structural="$(config_failure_qualifies "$kind" "$wait_rc" "$exit_code" "$state" "$oom" "$docker_error" ' ' || true)"
    structural="${structural%$'\n'}"
    if [[ "$structural" != "refuse missing_file_diagnostic" && "$structural" != "refuse zero_poll_diagnostic" ]]; then
        printf '%s\n' "$structural"
        [[ "$structural" == "qualify" ]] && return 0
        return 1
    fi
    if stream_panics "$logs_stdout" || stream_panics "$logs_stderr"; then
        printf 'refuse panic\n'
        return 1
    fi
    case "$kind" in
        missing_file)
            if [[ "$logs_stdout" != *"configuration file not found:"* && "$logs_stderr" != *"configuration file not found:"* ]]; then
                printf 'refuse missing_file_diagnostic\n'
                return 1
            fi
            if [[ "$logs_stdout" == *"poll interval must be > 0"* || "$logs_stderr" == *"poll interval must be > 0"* ]]; then
                printf 'refuse unexpected_poll_diagnostic\n'
                return 1
            fi
            printf 'qualify\n'
            return 0
            ;;
        zero_poll)
            if [[ ! ( "$logs_stdout" == *"daemon.poll_interval_secs"* && "$logs_stdout" == *"poll interval must be > 0"* ) && ! ( "$logs_stderr" == *"daemon.poll_interval_secs"* && "$logs_stderr" == *"poll interval must be > 0"* ) ]]; then
                printf 'refuse zero_poll_diagnostic\n'
                return 1
            fi
            if [[ "$logs_stdout" == *"configuration file not found:"* || "$logs_stderr" == *"configuration file not found:"* ]]; then
                printf 'refuse unexpected_missing_file_diagnostic\n'
                return 1
            fi
            printf 'qualify\n'
            return 0
            ;;
        *)
            printf 'refuse unknown_kind\n'
            return 1
            ;;
    esac
}

# accept_config_observations KIND WAIT_RC INSPECT_RC INSPECT_BODY LOGS_RC LOGS_STDOUT LOGS_STDERR INSPECT_ERR
# Caller gate for one complete state response and one log retrieval.
# Both captured streams are scored only after that retrieval succeeds.
# A failed, timed-out, permission, daemon, transport, or malformed observation
# refuses even when a stream already contains a valid diagnostic.
accept_config_observations() {
    local kind="$1"
    local wait_rc="$2"
    local inspect_rc="$3"
    local inspect_body="$4"
    local logs_rc="$5"
    local logs_stdout="$6"
    local logs_stderr="${7:-}"
    local inspect_err="${8:-}"
    local -a problems=()
    local state="" exit_code="" oom="" docker_error="" parsed="" error_json="" rest="" why=""

    if [[ "$inspect_rc" != "0" ]]; then
        why="$(observation_failure_class "$inspect_rc" "$inspect_body" "$inspect_err")"
        why="${why%$'\n'}"
        problems+=("observation_${why}")
    elif ! parsed="$(parse_complete_state "$inspect_body")"; then
        problems+=("malformed_state")
    else
        state="${parsed%%$'\t'*}"
        rest="${parsed#*$'\t'}"
        exit_code="${rest%%$'\t'*}"
        rest="${rest#*$'\t'}"
        oom="${rest%%$'\t'*}"
        error_json="${rest#*$'\t'}"
        if ! docker_error="$(python3 -c 'import json,sys; sys.stdout.write(json.loads(sys.argv[1]))' "$error_json")"; then
            problems+=("malformed_state")
            docker_error=""
        fi
    fi

    if [[ "$logs_rc" == "124" ]]; then
        problems+=("logs_timeout")
    elif [[ "$logs_rc" != "0" ]]; then
        problems+=("logs_failed")
    fi

    if ((${#problems[@]} > 0)); then
        printf 'refuse %s\n' "${problems[*]}"
        return 1
    fi
    config_streams_qualify "$kind" "$wait_rc" "$exit_code" "$state" "$oom" "$docker_error" "$logs_stdout" "$logs_stderr"
}

# qualify_captured_config_logs KIND WAIT_RC INSPECT_RC INSPECT_BODY LOGS_RC LOGS_STDOUT LOGS_STDERR INSPECT_ERR
# Entry used by the qualification script after it has captured the two streams.
qualify_captured_config_logs() {
    accept_config_observations "$@"
}

# qualify_replayed_split_capture KIND WAIT_RC INSPECT_RC INSPECT_BODY LOGS_RC STDOUT_TEXT STDERR_TEXT INSPECT_ERR
# Writes each stream to its own file and reads those files back without joining them.
# This is the capture path: empty stdout stays empty, and stderr is a separate stream.
qualify_replayed_split_capture() {
    local kind="$1"
    local wait_rc="$2"
    local inspect_rc="$3"
    local inspect_body="$4"
    local logs_rc="$5"
    local stdout_text="$6"
    local stderr_text="$7"
    local inspect_err="${8:-}"
    local dir="" out_file="" err_file="" logs_stdout="" logs_stderr="" decision=""
    dir="$(mktemp -d)"
    out_file="${dir}/stdout"
    err_file="${dir}/stderr"
    printf '%s' "$stdout_text" >"$out_file"
    printf '%s' "$stderr_text" >"$err_file"
    logs_stdout="$(cat "$out_file")"
    logs_stderr="$(cat "$err_file")"
    decision="$(qualify_captured_config_logs "$kind" "$wait_rc" "$inspect_rc" "$inspect_body" "$logs_rc" "$logs_stdout" "$logs_stderr" "$inspect_err" || true)"
    rm -rf "$dir"
    printf '%s\n' "${decision%$'\n'}"
    [[ "${decision%$'\n'}" == "qualify" ]]
}

# confirmed_absence KIND NAME TEXT
# KIND is container or network. TEXT must be exactly one completed Docker
# not-found line for that object. A different object, the other kind, extra
# text, or a generic daemon error is not absence.
confirmed_absence() {
    local kind="$1"
    local name="$2"
    local text="$3"
    local line=""
    if [[ -z "$name" ]]; then
        return 1
    fi
    line="$text"
    while [[ "$line" == *$'\n' || "$line" == *$'\r' ]]; do
        line="${line%$'\n'}"
        line="${line%$'\r'}"
    done
    if [[ -z "$line" || "$line" == *$'\n'* ]]; then
        return 1
    fi
    case "$kind" in
        container)
            [[ "$line" == "error: no such object: ${name}" || "$line" == "Error: No such object: ${name}" ]]
            ;;
        network)
            [[ "$line" == "Error response from daemon: network ${name} not found" || "$line" == "Error: No such network: ${name}" ]]
            ;;
        *)
            return 1
            ;;
    esac
}

# classify_inspect_presence RC STDOUT STDERR KIND NAME
# "absent" is only a completed not-found response.
# Empty failed output, timeout, permission, daemon, and transport are unknown.
classify_inspect_presence() {
    local rc="$1"
    local out="$2"
    local err="$3"
    local kind="${4:-}"
    local name="${5:-}"
    local trimmed="$out"
    local class=""
    if [[ "$rc" == "0" ]]; then
        trimmed="${trimmed#"${trimmed%%[![:space:]]*}"}"
        trimmed="${trimmed%"${trimmed##*[![:space:]]}"}"
        if [[ -z "$trimmed" ]]; then
            printf 'unknown malformed\n'
            return 1
        fi
        printf 'present\n'
        return 0
    fi
    class="$(observation_failure_class "$rc" "$out" "$err")"
    class="${class%$'\n'}"
    if [[ "$class" != "failed" ]]; then
        printf 'unknown %s\n' "$class"
        return 1
    fi
    if [[ -z "$trimmed" ]] && confirmed_absence "$kind" "$name" "$err"; then
        printf 'absent\n'
        return 0
    fi
    if [[ -z "$out" && -z "$err" ]]; then
        printf 'unknown empty_failure\n'
        return 1
    fi
    printf 'unknown failed\n'
    return 1
}

# cleanup_removal_result REMOVE_RC PRESENCE_CLASS
# Confirmed absence is removal. A failed remove followed by a failed or
# unknown inspection stays unresolved; empty failed output is not absence.
cleanup_removal_result() {
    local rm_rc="$1"
    local presence="$2"
    if [[ "$presence" == "absent" ]]; then
        # Confirmed not-found stands on its own. rm_rc is retained so a
        # failed remove cannot be paired with a failed inspection above.
        printf 'removed\n'
        return 0
    fi
    if [[ -z "$rm_rc" ]]; then
        printf 'unresolved unknown failed\n'
        return 1
    fi
    if [[ "$presence" == "present" ]]; then
        printf 'unresolved remains\n'
        return 1
    fi
    if [[ "$presence" == unknown* ]]; then
        printf 'unresolved %s\n' "$presence"
        return 1
    fi
    printf 'unresolved unknown failed\n'
    return 1
}

# finalize_cleanup_result PRIOR CONTAINER_CLASS NETWORK_CLASS
# CONTAINER_CLASS: none, absent, present, or unknown...
# NETWORK_CLASS: not_created, absent, present, or unknown...
# Unknown is unresolved. A prior nonzero exit is preserved.
finalize_cleanup_result() {
    local prior="$1"
    local container_class="$2"
    local network_class="$3"
    local unresolved=0
    local final=""
    if [[ "$container_class" != "absent" && "$container_class" != "none" ]]; then
        unresolved=1
    fi
    if [[ "$network_class" != "absent" && "$network_class" != "not_created" ]]; then
        unresolved=1
    fi
    final="$(cleanup_final_status "$prior" "$unresolved")"
    final="${final%$'\n'}"
    if [[ "$unresolved" == "0" ]]; then
        printf '%s cleanup_resolved\n' "$final"
    else
        printf '%s cleanup_unresolved\n' "$final"
    fi
}
