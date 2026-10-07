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
# Exit code: 0 when every scenario is PASS or NOT RUN; non-zero on any FAIL.
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
export GIT_AUTHOR_NAME="${GIT_AUTHOR_NAME:-Test User}"
export GIT_AUTHOR_EMAIL="${GIT_AUTHOR_EMAIL:-test@example.com}"
export GIT_COMMITTER_NAME="${GIT_COMMITTER_NAME:-Test User}"
export GIT_COMMITTER_EMAIL="${GIT_COMMITTER_EMAIL:-test@example.com}"

if command -v rustup >/dev/null 2>&1 && rustup toolchain list | grep -q '^stable'; then
  CARGO=(rustup run stable cargo)
else
  CARGO=(cargo)
fi

"${CARGO[@]}" build --tests -p reposync-core --test real_engine_scenarios --locked >/dev/null

overall_fail=0
for entry in "${SCENARIOS[@]}"; do
  id="${entry%%:*}"
  test_name="${entry##*:}"
  log_file="$ARTIFACT_DIR/${id}.log"
  set +e
  "${CARGO[@]}" test -p reposync-core --test real_engine_scenarios "$test_name" -- --exact --nocapture >"$log_file" 2>&1
  test_exit=$?
  set -e

  if ! python3 scripts/real-engine-scenario-report.py append "$RESULTS_FILE" "$id" "$test_name" "$test_exit" "$log_file"; then
    overall_fail=1
  fi
done

python3 scripts/real-engine-scenario-report.py summary "$RESULTS_FILE" "$SUMMARY_FILE" PASS
exit "$overall_fail"
