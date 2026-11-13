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

# Caller-level observation controls. These exercise accept_config_observations,
# not only the semantic helper. A valid diagnostic cannot outvote a failed read.
VALID_STATE='"exited" 1 false ""'
expect_text qualify accept_config_observations missing_file 0 0 "$VALID_STATE" 0 "$MISSING_LOG" ''
expect_text qualify accept_config_observations zero_poll 0 0 "$VALID_STATE" 0 "$ZERO_POLL_LOG" ''
expect_text 'refuse logs_failed' accept_config_observations missing_file 0 0 "$VALID_STATE" 1 "$MISSING_LOG" ''
expect_text 'refuse logs_timeout' accept_config_observations zero_poll 0 0 "$VALID_STATE" 124 "$ZERO_POLL_LOG" ''
expect_text 'refuse observation_timeout' accept_config_observations missing_file 0 124 "$VALID_STATE" 0 "$MISSING_LOG" ''
expect_text 'refuse observation_permission' accept_config_observations missing_file 0 1 "$VALID_STATE" 0 "$MISSING_LOG" '' 'permission denied'
expect_text 'refuse observation_daemon' accept_config_observations zero_poll 0 1 "$VALID_STATE" 0 "$ZERO_POLL_LOG" '' 'Cannot connect to the Docker daemon'
expect_text 'refuse observation_transport' accept_config_observations missing_file 0 1 "$VALID_STATE" 0 "$MISSING_LOG" '' 'error during connect: dial unix /var/run/docker.sock: connection refused'
expect_text 'refuse observation_failed' accept_config_observations missing_file 0 1 "$VALID_STATE" 0 "$MISSING_LOG" '' 'inspect api failed'
expect_text 'refuse observation_timeout logs_failed' accept_config_observations missing_file 0 124 "$VALID_STATE" 1 "$MISSING_LOG" ''
expect_text 'refuse malformed_state' accept_config_observations missing_file 0 0 'not-json' 0 "$MISSING_LOG" ''
expect_text 'refuse malformed_state' accept_config_observations zero_poll 0 0 '"exited"' 0 "$ZERO_POLL_LOG" ''
expect_text 'refuse oom' accept_config_observations missing_file 0 0 '"exited" 137 true ""' 0 "$MISSING_LOG" ''
expect_text 'refuse exit_7' accept_config_observations zero_poll 0 0 '"exited" 7 false ""' 0 "$ZERO_POLL_LOG" ''
expect_text 'refuse panic' accept_config_observations zero_poll 0 0 "$VALID_STATE" 0 $'panicked at crates/daemon/src/main.rs:1:1\n'"$ZERO_POLL_LOG" ''
expect_text 'refuse docker_error' accept_config_observations missing_file 0 0 '"exited" 1 false "OCI runtime create failed"' 0 "$MISSING_LOG" ''
expect_text 'refuse missing_file_diagnostic' accept_config_observations missing_file 0 0 "$VALID_STATE" 0 "$ZERO_POLL_LOG" ''
expect_text 'refuse zero_poll_diagnostic' accept_config_observations zero_poll 0 0 "$VALID_STATE" 0 "$MISSING_LOG" ''

# Capture path. Stdout and stderr are separate files, then the same caller
# entry the script uses. Empty stdout with the daemon diagnostic on stderr qualifies.
# A nonzero retrieval still refuses when that diagnostic is present.
expect_text qualify qualify_replayed_split_capture missing_file 0 0 "$VALID_STATE" 0 '' "$MISSING_LOG" ''
expect_text qualify qualify_replayed_split_capture zero_poll 0 0 "$VALID_STATE" 0 '' "$ZERO_POLL_LOG" ''
expect_text 'refuse logs_failed' qualify_replayed_split_capture missing_file 0 0 "$VALID_STATE" 1 '' "$MISSING_LOG" ''
expect_text 'refuse logs_failed' qualify_replayed_split_capture zero_poll 0 0 "$VALID_STATE" 1 'configuration file' "$ZERO_POLL_LOG" ''
expect_text 'refuse logs_timeout' qualify_replayed_split_capture missing_file 0 0 "$VALID_STATE" 124 '' "$MISSING_LOG" ''
expect_text 'refuse panic' qualify_replayed_split_capture missing_file 0 0 "$VALID_STATE" 0 '' $'panicked at crates/daemon/src/main.rs:1:1\n'"$MISSING_LOG" ''
expect_text 'refuse panic' qualify_replayed_split_capture zero_poll 0 0 "$VALID_STATE" 0 'fatal runtime error: aborted' "$ZERO_POLL_LOG" ''
expect_text 'refuse missing_file_diagnostic' qualify_replayed_split_capture missing_file 0 0 "$VALID_STATE" 0 '' 'database is locked' ''
expect_text 'refuse zero_poll_diagnostic' qualify_replayed_split_capture zero_poll 0 0 "$VALID_STATE" 0 '' 'unrelated startup failure' ''
expect_text 'refuse missing_file_diagnostic' qualify_replayed_split_capture missing_file 0 0 "$VALID_STATE" 0 '' "$ZERO_POLL_LOG" ''
expect_text 'refuse zero_poll_diagnostic' qualify_replayed_split_capture zero_poll 0 0 "$VALID_STATE" 0 '' "$MISSING_LOG" ''

# Caller-level cleanup controls. Failed or empty inspect output is not absence.
expect_text 'unknown daemon' classify_inspect_presence 1 '' 'Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?'
expect_text 'unknown timeout' classify_inspect_presence 124 '' ''
expect_text 'unknown empty_failure' classify_inspect_presence 1 '' ''
expect_text 'unknown permission' classify_inspect_presence 1 '' 'permission denied while trying to connect to the Docker daemon socket'
expect_text 'unknown permission' classify_inspect_presence 1 '' $'permission denied\nError: No such object: rs118ui-1'
expect_text 'unknown transport' classify_inspect_presence 1 '' 'error during connect: dial unix /var/run/docker.sock: connect: connection refused'
expect_text 'unknown malformed' classify_inspect_presence 0 '' ''
expect_text 'unknown malformed' classify_inspect_presence 0 '   ' ''
expect_text 'absent' classify_inspect_presence 1 '' 'Error: No such object: rs118ui-1'
expect_text 'absent' classify_inspect_presence 1 '' 'Error: No such network: rs118ui-1-net'
expect_text 'absent' classify_inspect_presence 1 '' 'error: no such object: rs118ui-1'
expect_text 'absent' classify_inspect_presence 1 '' 'Error response from daemon: network rs118ui-1-net not found'
expect_text 'unknown failed' classify_inspect_presence 1 '' 'not found'
expect_text 'unknown failed' classify_inspect_presence 1 '' 'Error response from daemon: something else failed'
expect_text 'present' classify_inspect_presence 0 'abc123 "exited"' ''
expect_text 'unresolved unknown daemon' cleanup_removal_result 1 'unknown daemon'
expect_text 'unresolved unknown timeout' cleanup_removal_result 1 'unknown timeout'
expect_text 'unresolved unknown empty_failure' cleanup_removal_result 1 'unknown empty_failure'
expect_text 'unresolved unknown daemon' cleanup_removal_result 0 'unknown daemon'
expect_text 'unresolved remains' cleanup_removal_result 1 'present'
expect_text 'removed' cleanup_removal_result 0 'absent'
expect_text 'removed' cleanup_removal_result 1 'absent'
expect_text '0 cleanup_resolved' finalize_cleanup_result 0 absent absent
expect_text '0 cleanup_resolved' finalize_cleanup_result 0 none not_created
expect_text '1 cleanup_unresolved' finalize_cleanup_result 0 'unknown daemon' absent
expect_text '1 cleanup_unresolved' finalize_cleanup_result 0 absent 'unknown timeout'
expect_text '1 cleanup_unresolved' finalize_cleanup_result 0 absent 'unknown empty_failure'
expect_text '1 cleanup_unresolved' finalize_cleanup_result 0 present absent
expect_text '2 cleanup_unresolved' finalize_cleanup_result 2 'unknown daemon' 'unknown timeout'
expect_text '2 cleanup_resolved' finalize_cleanup_result 2 none not_created

if [[ "$failures" != "0" ]]; then
    printf 'authored_checks_failed count=%s\n' "$failures" >&2
    exit 1
fi
printf 'authored_checks_ok\n'
