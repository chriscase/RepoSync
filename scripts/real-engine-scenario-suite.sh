#!/usr/bin/env bash
# ============================================================================
# RepoSync real-engine scenario suite (issue #62)
# ============================================================================
# CI-runnable suite that drives real svnserve remotes and local bare Git repos
# through the production team SyncEngine. Reuses the docker-compose SVN service
# when REPOSYNC_REAL_ENGINE_USE_COMPOSE=1 and the stack is already healthy.
#
# Each scenario reports PASS, FAIL, PARTIAL, NOT RUN, or SKIP in machine-readable
# JSON. Missing required tools always yield NOT RUN — never PASS.
#
# Usage:
#   scripts/real-engine-scenario-suite.sh
#   REPOSYNC_REAL_ENGINE_USE_COMPOSE=1 scripts/real-engine-scenario-suite.sh
#
# Output: artifacts/real-engine-scenarios/<UTC_TIMESTAMP>/summary.json
# Exit code: 0 only when suite overall is PASS (every scenario PASS or NOT RUN with
# no PARTIAL rows). Exit 1 on FAIL, 2 when every scenario is NOT RUN, 3 when any
# scenario is PARTIAL (including mixed PASS + PARTIAL).
#
# CI does not treat exit 3 as a red Build & Test by itself: after the suite runs,
# `.github/workflows/ci.yml` calls `real-engine-scenario-report.py ci-gate` on the
# emitted summary.json. Only PARTIAL rows listed in
# docs/reliability/real-engine-ci-partial-allowlist.json are accepted; any FAIL,
# NOT RUN, or other PARTIAL still fails the job. Self-test:
#   python3 scripts/real-engine-scenario-report.py self-test
# ============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

TIMESTAMP="$(date -u +%Y%m%dT%H%M%SZ)"
ARTIFACT_DIR="$REPO_ROOT/artifacts/real-engine-scenarios/$TIMESTAMP"
SUMMARY_FILE="$ARTIFACT_DIR/summary.json"
RESULTS_FILE="$ARTIFACT_DIR/results.ndjson"

SCENARIOS=(
  "R17_SVNSERVE_CONCURRENT_OVERLAP:scenario_r17_svnserve_concurrent_overlap"
  "R17_SVNSERVE_CHECKPOINT_ISOLATION:scenario_r17_svnserve_checkpoint_isolation"
  "R17_SVNSERVE_CREDENTIAL_ISOLATION:scenario_r17_svnserve_credential_isolation"
  "R17_SVNSERVE_JOB_ISOLATION:scenario_r17_svnserve_job_isolation"
  "R01_SVNSERVE_MULTI_COMMIT_SVN_TO_GIT:scenario_r01_svnserve_multi_commit_svn_to_git"
  "R01_SVNSERVE_MULTI_COMMIT_GIT_TO_SVN:scenario_r01_svnserve_multi_commit_git_to_svn"
  "R16_SVNSERVE_GIT_REMOTE_UNREACHABLE:scenario_r16_svnserve_git_remote_unreachable"
  "R16_SVNSERVE_SVN_REMOTE_UNREACHABLE:scenario_r16_svnserve_svn_remote_unreachable"
  "R16_SVNSERVE_SVN_AUTH_DENIED:scenario_r16_svnserve_svn_auth_denied"
  "R16_SVNSERVE_MISSING_GIT_BRANCH:scenario_r16_svnserve_missing_git_branch"
  "R01_SVNSERVE_BIDIRECTIONAL_ROUNDTRIP:scenario_r01_svnserve_bidirectional_roundtrip"
  "R16_SVNSERVE_CREDENTIAL_ROTATION:scenario_r16_svnserve_credential_rotation"
  "R16_SVNSERVE_PARENT_CHILD_CREDENTIAL_CHAIN_ROTATION:scenario_r16_svnserve_parent_child_credential_chain_rotation"
  "R17_SVNSERVE_CONCURRENT_CREDENTIAL_RELOAD:scenario_r17_svnserve_concurrent_credential_reload"
  "R17_SVNSERVE_PARENT_CHILD_CONCURRENT_CREDENTIAL_RELOAD:scenario_r17_svnserve_parent_child_concurrent_credential_reload"
)

mkdir -p "$ARTIFACT_DIR"
: > "$RESULTS_FILE"

missing_tool=""
for tool in svn svnadmin svnserve git cargo; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    missing_tool="$tool"
    break
  fi
done

if [[ -n "$missing_tool" ]]; then
  for entry in "${SCENARIOS[@]}"; do
    id="${entry%%:*}"
    test_name="${entry##*:}"
    printf '{"id":"%s","status":"NOT RUN","test":"%s","detail":{"reason":"missing_tool","tool":"%s"}}\n' \
      "$id" "$test_name" "$missing_tool" >> "$RESULTS_FILE"
  done
  python3 scripts/real-engine-scenario-report.py summary "$RESULTS_FILE" "$SUMMARY_FILE" "NOT RUN" "$missing_tool"
  exit 2
fi

if [[ "${REPOSYNC_REAL_ENGINE_USE_COMPOSE:-}" == "1" ]]; then
  if command -v docker >/dev/null 2>&1 && docker compose -f tests/docker-compose.yml ps --status running --services 2>/dev/null | grep -qx svn-server; then
    export REPOSYNC_TEST_SVN_URL="${REPOSYNC_TEST_SVN_URL:-http://localhost:8081/svn/testrepo}"
    echo "Using docker-compose SVN service at $REPOSYNC_TEST_SVN_URL" > "$ARTIFACT_DIR/compose-note.txt"
  else
    echo "REPOSYNC_REAL_ENGINE_USE_COMPOSE=1 but svn-server is not healthy; using local svnserve" > "$ARTIFACT_DIR/compose-note.txt"
  fi
fi

export REPOSYNC_TEST_SVN_PW="${REPOSYNC_TEST_SVN_PW:-fixture-only-svn-secret}"
export REPOSYNC_TEST_GH_TOKEN="${REPOSYNC_TEST_GH_TOKEN:-fixture-only-git-secret}"

if command -v rustup >/dev/null 2>&1 && rustup toolchain list | grep -q '^stable'; then
  CARGO=(rustup run stable cargo)
else
  CARGO=(cargo)
fi

"${CARGO[@]}" build --tests -p reposync-core --test real_engine_scenarios --locked >/dev/null

suite_fail=0
suite_partial=0
suite_not_run=0
for entry in "${SCENARIOS[@]}"; do
  id="${entry%%:*}"
  test_name="${entry##*:}"
  log_file="$ARTIFACT_DIR/${id}.log"
  set +e
  "${CARGO[@]}" test -p reposync-core --test real_engine_scenarios "$test_name" -- --exact --nocapture >"$log_file" 2>&1
  test_exit=$?
  set -e

  append_exit=0
  python3 scripts/real-engine-scenario-report.py append "$RESULTS_FILE" "$id" "$test_name" "$test_exit" "$log_file" || append_exit=$?
  case "$append_exit" in
    1) suite_fail=1 ;;
    3) suite_partial=1 ;;
    2) suite_not_run=1 ;;
  esac
done

summary_exit=0
python3 scripts/real-engine-scenario-report.py summary "$RESULTS_FILE" "$SUMMARY_FILE" PASS || summary_exit=$?
case "$summary_exit" in
  1) suite_fail=1 ;;
  3) suite_partial=1 ;;
  2) suite_not_run=1 ;;
esac
if [[ "$suite_fail" -eq 1 ]]; then
  exit 1
fi
if [[ "$suite_partial" -eq 1 ]]; then
  exit 3
fi
if [[ "$suite_not_run" -eq 1 ]]; then
  exit 2
fi
exit 0
