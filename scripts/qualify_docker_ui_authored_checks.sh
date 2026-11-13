#!/usr/bin/env bash
# Authored decision controls. No Docker, image build, or network.
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=docker_ui_qualify_decisions.sh
source "${ROOT}/docker_ui_qualify_decisions.sh"

failures=0

expect_text() {
    local want="$1"
    shift
    local got
    got="$("$@" || true)"
    got="${got%$'\n'}"
    if [[ "$got" != "$want" ]]; then
        printf 'authored_check_failed want=%s got=%s\n' "$want" "$got" >&2
        failures=$((failures + 1))
    fi
}

MISSING_LOG=$'Error: failed to load configuration file\nCaused by:\n    configuration file not found: /etc/reposync/missing.toml'
ZERO_POLL_LOG=$'Error: configuration validation failed\nCaused by:\n    invalid configuration value for '\''daemon.poll_interval_secs'\'': poll interval must be > 0'

expect_text qualify config_failure_qualifies missing_file 0 1 exited false '' "$MISSING_LOG"
expect_text qualify config_failure_qualifies zero_poll 0 1 exited false '' "$ZERO_POLL_LOG"

# Any other nonzero exit, including one that happens to print a diagnostic, is not the case.
expect_text 'refuse exit_7' config_failure_qualifies missing_file 0 7 exited false '' "$MISSING_LOG"
expect_text 'refuse exit_2' config_failure_qualifies zero_poll 0 2 exited false '' "$ZERO_POLL_LOG"
expect_text 'refuse exit_0' config_failure_qualifies missing_file 0 0 exited false '' "$MISSING_LOG"
expect_text 'refuse exit_137' config_failure_qualifies zero_poll 0 137 exited false '' "$ZERO_POLL_LOG"
expect_text 'refuse oom' config_failure_qualifies missing_file 0 137 exited true '' "$MISSING_LOG"
expect_text 'refuse panic' config_failure_qualifies zero_poll 0 101 exited false '' $'panicked at crates/daemon/src/main.rs:1:1\n'"$ZERO_POLL_LOG"
expect_text 'refuse panic' config_failure_qualifies missing_file 0 1 exited false '' $'fatal runtime error: aborted\n'"$MISSING_LOG"
expect_text 'refuse wait_timeout' config_failure_qualifies missing_file 124 '' running false '' ''
expect_text 'refuse docker_wait_failed' config_failure_qualifies zero_poll 1 '' missing false '' ''
expect_text 'refuse docker_error' config_failure_qualifies missing_file 0 1 exited false 'OCI runtime create failed' "$MISSING_LOG"
expect_text 'refuse container_state_running' config_failure_qualifies missing_file 0 1 running false '' "$MISSING_LOG"
expect_text 'refuse missing_file_diagnostic' config_failure_qualifies missing_file 0 1 exited false '' "$ZERO_POLL_LOG"
expect_text 'refuse zero_poll_diagnostic' config_failure_qualifies zero_poll 0 1 exited false '' "$MISSING_LOG"
expect_text 'refuse unexpected_poll_diagnostic' config_failure_qualifies missing_file 0 1 exited false '' $'configuration file not found: /etc/reposync/missing.toml\npoll interval must be > 0'
expect_text 'refuse unexpected_missing_file_diagnostic' config_failure_qualifies zero_poll 0 1 exited false '' $'daemon.poll_interval_secs poll interval must be > 0\nconfiguration file not found: /etc/reposync/missing.toml'
expect_text 'refuse unknown_kind' config_failure_qualifies unrelated 0 1 exited false '' "$MISSING_LOG"

expect_text qualify missing_static_qualifies 1 500 1 200 true
expect_text 'refuse index_transport' missing_static_qualifies 0 000 1 200 true
expect_text 'refuse index_transport' missing_static_qualifies 0 '' 1 200 true
expect_text 'refuse index_status_200' missing_static_qualifies 1 200 1 200 true
expect_text 'refuse index_status_404' missing_static_qualifies 1 404 1 200 true
expect_text 'refuse index_status_502' missing_static_qualifies 1 502 1 200 true
expect_text 'refuse health_transport' missing_static_qualifies 1 500 0 000 true
expect_text 'refuse health_status' missing_static_qualifies 1 500 1 500 true
expect_text 'refuse health_status' missing_static_qualifies 1 500 1 200 false

expect_text 0 cleanup_final_status 0 0
expect_text 1 cleanup_final_status 0 1
expect_text 1 cleanup_final_status 1 0
expect_text 1 cleanup_final_status 1 1
expect_text 2 cleanup_final_status 2 1
expect_text 2 cleanup_final_status 2 0

if cleanup_may_wait_after_stop 0; then
    :
else
    printf 'authored_check_failed stop_success_should_allow_bounded_wait\n' >&2
    failures=$((failures + 1))
fi
if cleanup_may_wait_after_stop 1; then
    printf 'authored_check_failed failed_stop_must_not_wait\n' >&2
    failures=$((failures + 1))
fi

if [[ "$failures" != "0" ]]; then
    printf 'authored_checks_failed count=%s\n' "$failures" >&2
    exit 1
fi
printf 'authored_checks_ok\n'
