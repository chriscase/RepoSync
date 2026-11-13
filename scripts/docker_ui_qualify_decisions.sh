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
