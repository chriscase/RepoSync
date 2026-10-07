#!/usr/bin/env bash
# ============================================================================
# RepoSync Controlled-Environment Validation Script
# ============================================================================
# Non-interactive, CI-safe, one-command validation of the full sync pipeline
# using local-only resources (file:// SVN repos, local Git repos).
# Produces a timestamped forensic artifact bundle suitable for human review
# and AI-assisted analysis.
#
# NOTE: This validates compilation, tests, linting, local SVN operations,
# and the logging subsystem.  It does NOT validate live GitHub API or
# remote SVN connectivity.  For real GHE+SVN validation, use:
#   scripts/ghe-live-validation.sh
#
# Usage:
#   scripts/controlled-validation.sh              # default run
#   scripts/controlled-validation.sh --quick      # quick smoke test (fewer revisions)
#   scripts/controlled-validation.sh --help       # show usage
#
# Output: artifacts/controlled-validation/<UTC_TIMESTAMP>/
# Exit code: 0 on all PASS, non-zero on any FAIL (partial artifacts preserved)
# ============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

TIMESTAMP="$(date -u +%Y%m%dT%H%M%SZ)"
ARTIFACT_DIR="$REPO_ROOT/artifacts/controlled-validation/$TIMESTAMP"
EVENTS_FILE="$ARTIFACT_DIR/events.ndjson"
TIMELINE_LOG="$ARTIFACT_DIR/timeline.log"
SUMMARY_FILE="$ARTIFACT_DIR/summary.md"
MANIFEST_FILE="$ARTIFACT_DIR/manifest.json"

# Defaults
QUICK_MODE=false

# Phase-level counters (build, cargo-test, clippy, log-probe, redaction)
PHASE_PASS=0
PHASE_FAIL=0

# Scenario-level counters (SVN live scenarios within phase 4)
SCENARIO_PASS=0
SCENARIO_FAIL=0
SCENARIO_SKIP=0

# ============================================================================
# Argument parsing
# ============================================================================
while [[ $# -gt 0 ]]; do
    case "$1" in
        --quick) QUICK_MODE=true; shift ;;
        --help|-h)
            echo "Usage: $0 [--quick] [--help]"
            echo "  --quick    Run a reduced scenario matrix (fewer SVN revisions)"
            echo "  --help     Show this help"
            exit 0
            ;;
        *) echo "Unknown argument: $1"; exit 1 ;;
    esac
done

# ============================================================================
# Helpers
# ============================================================================

mkdir -p "$ARTIFACT_DIR"

log() {
    local msg="[$(date -u +%H:%M:%S)] $*"
    echo "$msg" | tee -a "$TIMELINE_LOG"
}

emit_event() {
    local phase="$1" action="$2" status="$3" duration_ms="${4:-0}"
    shift 4 || true
    local extra=""
    for kv in "$@"; do
        local key="${kv%%=*}"
        local val="${kv#*=}"
        extra="${extra},\"${key}\":\"${val}\""
    done
    printf '{"timestamp":"%s","phase":"%s","action":"%s","status":"%s","duration_ms":%d%s}\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "$phase" "$action" "$status" "$duration_ms" "$extra" >> "$EVENTS_FILE"
}

# Record pass/fail for a scenario within a phase (e.g. SVN live scenarios).
record_scenario() {
    local name="$1" result="$2" detail="${3:-}"
    if [[ "$result" == "pass" ]]; then
        SCENARIO_PASS=$((SCENARIO_PASS + 1))
        log "  ✅ $name: PASS${detail:+ ($detail)}"
    elif [[ "$result" == "skip" ]]; then
        SCENARIO_SKIP=$((SCENARIO_SKIP + 1))
        log "  ⏭  $name: SKIP${detail:+ ($detail)}"
    else
        SCENARIO_FAIL=$((SCENARIO_FAIL + 1))
        log "  ❌ $name: FAIL${detail:+ ($detail)}"
    fi
}

# Accumulate per-phase results.  These track the 6 top-level phases.
declare -a PHASE_NAMES=()
declare -a PHASE_RESULTS=()

record_phase() {
    local name="$1" result="$2"
    PHASE_NAMES+=("$name")
    PHASE_RESULTS+=("$result")
    if [[ "$result" == "PASS" ]]; then
        PHASE_PASS=$((PHASE_PASS + 1))
    else
        PHASE_FAIL=$((PHASE_FAIL + 1))
    fi
}

cleanup() {
    log "Cleaning up temp directories..."
    if [[ -n "${WORK_DIR:-}" && -d "$WORK_DIR" ]]; then
        rm -rf "$WORK_DIR"
    fi
}
trap cleanup EXIT

# ============================================================================
# Preflight checks
# ============================================================================
log "═══════════════════════════════════════════════════════════════"
log "RepoSync Controlled-Environment Validation (local)"
log "Timestamp: $TIMESTAMP"
log "Artifact dir: $ARTIFACT_DIR"
log "Quick mode: $QUICK_MODE"
log "═══════════════════════════════════════════════════════════════"

emit_event "preflight" "start" "running" 0

MISSING_TOOLS=""
for tool in svn svnadmin git cargo; do
    if ! command -v "$tool" &>/dev/null; then
        MISSING_TOOLS="$MISSING_TOOLS $tool"
    fi
done

if [[ -n "$MISSING_TOOLS" ]]; then
    log "FATAL: Missing required tools:$MISSING_TOOLS"
    emit_event "preflight" "complete" "fail" 0
    exit 1
fi

# Save sanitized environment snapshot (strip secrets).
env | sort | grep -v -iE '(token|password|secret|key|credential|auth)' \
    > "$ARTIFACT_DIR/env-snapshot.txt" 2>/dev/null || true

{
    echo "svn: $(svn --version --quiet 2>/dev/null || echo unknown)"
    echo "svnadmin: $(svnadmin --version --quiet 2>/dev/null || echo unknown)"
    echo "git: $(git --version 2>/dev/null || echo unknown)"
    echo "cargo: $(cargo --version 2>/dev/null || echo unknown)"
    echo "rustc: $(rustc --version 2>/dev/null || echo unknown)"
    echo "os: $(uname -srm 2>/dev/null || echo unknown)"
} > "$ARTIFACT_DIR/tool-versions.txt"

emit_event "preflight" "complete" "pass" 0
log "Preflight: all tools available"

# ============================================================================
# Phase 1: Build workspace
# ============================================================================
log ""
log "──────────────────────────────────────────────────────────────"
log "Phase 1/6: Build workspace"
log "──────────────────────────────────────────────────────────────"

BUILD_DIR="$ARTIFACT_DIR/build"
mkdir -p "$BUILD_DIR"
PHASE_START=$SECONDS
emit_event "build" "start" "running" 0
log "▶ cargo build --workspace..."

if cargo build --workspace > "$BUILD_DIR/stdout.log" 2> "$BUILD_DIR/stderr.log"; then
    DUR=$(( (SECONDS - PHASE_START) * 1000 ))
    emit_event "build" "complete" "pass" "$DUR"
    log "  ✅ Build PASS (${DUR}ms)"
    record_phase "Build" "PASS"
else
    DUR=$(( (SECONDS - PHASE_START) * 1000 ))
    emit_event "build" "complete" "fail" "$DUR"
    log "  ❌ Build FAIL — see $BUILD_DIR/stderr.log"
    record_phase "Build" "FAIL"
    log "FATAL: Build failed. Aborting."
    exit 1
fi

# ============================================================================
# Phase 2: Cargo tests
# ============================================================================
log ""
log "──────────────────────────────────────────────────────────────"
log "Phase 2/6: Cargo test --workspace"
log "──────────────────────────────────────────────────────────────"

TEST_DIR="$ARTIFACT_DIR/cargo-test"
mkdir -p "$TEST_DIR"
PHASE_START=$SECONDS
emit_event "cargo-test" "start" "running" 0
log "▶ cargo test --workspace..."

if cargo test --workspace > "$TEST_DIR/stdout.log" 2> "$TEST_DIR/stderr.log"; then
    DUR=$(( (SECONDS - PHASE_START) * 1000 ))
    emit_event "cargo-test" "complete" "pass" "$DUR"
    log "  ✅ Tests PASS (${DUR}ms)"
    record_phase "Cargo tests" "PASS"
else
    DUR=$(( (SECONDS - PHASE_START) * 1000 ))
    emit_event "cargo-test" "complete" "fail" "$DUR"
    log "  ❌ Tests FAIL — see $TEST_DIR/stderr.log"
    record_phase "Cargo tests" "FAIL"
fi

# ============================================================================
# Phase 3: Clippy
# ============================================================================
log ""
log "──────────────────────────────────────────────────────────────"
log "Phase 3/6: Clippy (zero warnings)"
log "──────────────────────────────────────────────────────────────"

CLIPPY_DIR="$ARTIFACT_DIR/clippy"
mkdir -p "$CLIPPY_DIR"
PHASE_START=$SECONDS
emit_event "clippy" "start" "running" 0
log "▶ cargo clippy --workspace --all-targets -- -D warnings..."

if cargo clippy --workspace --all-targets -- -D warnings \
    > "$CLIPPY_DIR/stdout.log" 2> "$CLIPPY_DIR/stderr.log"; then
    DUR=$(( (SECONDS - PHASE_START) * 1000 ))
    emit_event "clippy" "complete" "pass" "$DUR"
    log "  ✅ Clippy PASS (${DUR}ms)"
    record_phase "Clippy" "PASS"
else
    DUR=$(( (SECONDS - PHASE_START) * 1000 ))
    emit_event "clippy" "complete" "fail" "$DUR"
    log "  ❌ Clippy FAIL — see $CLIPPY_DIR/stderr.log"
    record_phase "Clippy" "FAIL"
fi

# ============================================================================
# Phase 4: Local SVN scenario matrix (file:// repos, no network)
# ============================================================================
log ""
log "──────────────────────────────────────────────────────────────"
log "Phase 4/6: Local SVN scenario matrix"
log "──────────────────────────────────────────────────────────────"

WORK_DIR=$(mktemp -d)
SVN_REPO_DIR="$WORK_DIR/svn_repo"
SVN_WC="$WORK_DIR/svn_wc"
LIVE_DIR="$ARTIFACT_DIR/local-svn-scenarios"
mkdir -p "$LIVE_DIR"

svnadmin create "$SVN_REPO_DIR"
SVN_URL="file://$SVN_REPO_DIR"

# Enable revprop changes.
printf '#!/bin/sh\nexit 0\n' > "$SVN_REPO_DIR/hooks/pre-revprop-change"
chmod +x "$SVN_REPO_DIR/hooks/pre-revprop-change"

svn checkout "$SVN_URL" "$SVN_WC" --non-interactive -q

emit_event "local-svn" "start" "running" 0

# --- 4a: Basic SVN commits ---
log "▶ 4a: Basic SVN commits"
NUM_REVS=3; $QUICK_MODE && NUM_REVS=2
for i in $(seq 1 "$NUM_REVS"); do
    echo "content_$i" > "$SVN_WC/file_$i.txt"
    svn add "$SVN_WC/file_$i.txt" -q 2>/dev/null || true
    svn commit "$SVN_WC" -m "Add file_$i" --non-interactive -q
done
SVN_HEAD=$(svn info "$SVN_URL" --show-item revision --no-newline 2>/dev/null || echo "0")
if [[ "$SVN_HEAD" == "$NUM_REVS" ]]; then
    record_scenario "4a-svn-commits" "pass" "$NUM_REVS revisions committed"
    emit_event "local-svn" "4a-commits" "pass" 0 "svn_rev=$SVN_HEAD"
else
    record_scenario "4a-svn-commits" "fail" "expected $NUM_REVS, got $SVN_HEAD"
    emit_event "local-svn" "4a-commits" "fail" 0
fi

# --- 4b: Echo suppression marker ---
log "▶ 4b: Echo suppression marker"
echo "echo_content" > "$SVN_WC/echo_test.txt"
svn add "$SVN_WC/echo_test.txt" -q 2>/dev/null || true
svn commit "$SVN_WC" -m "Synced from Git [reposync] echo marker" --non-interactive -q
ECHO_REV=$(svn info "$SVN_URL" --show-item revision --no-newline 2>/dev/null || echo "0")
ECHO_LOG=$(svn log "$SVN_URL" -r "$ECHO_REV" --xml 2>/dev/null || echo "")
if echo "$ECHO_LOG" | grep -q "\[reposync\]"; then
    record_scenario "4b-echo-marker" "pass" "echo marker found in r$ECHO_REV"
    emit_event "local-svn" "4b-echo" "pass" 0 "svn_rev=$ECHO_REV"
else
    record_scenario "4b-echo-marker" "fail" "no marker in r$ECHO_REV"
    emit_event "local-svn" "4b-echo" "fail" 0
fi

# --- 4c: File modification tracking ---
log "▶ 4c: File modification tracking"
echo "base content" > "$SVN_WC/conflict_file.txt"
svn add "$SVN_WC/conflict_file.txt" -q 2>/dev/null || true
svn commit "$SVN_WC" -m "Add conflict_file" --non-interactive -q
echo "svn modified content" > "$SVN_WC/conflict_file.txt"
svn commit "$SVN_WC" -m "Modify conflict_file" --non-interactive -q
CONTENT=$(svn cat "$SVN_URL/conflict_file.txt" 2>/dev/null || echo "")
if [[ "$CONTENT" == "svn modified content" ]]; then
    record_scenario "4c-file-modify" "pass" "modification verified"
    emit_event "local-svn" "4c-modify" "pass" 0
else
    record_scenario "4c-file-modify" "fail" "unexpected content"
    emit_event "local-svn" "4c-modify" "fail" 0
fi

# --- 4d: File deletion ---
log "▶ 4d: File deletion propagation"
echo "to_be_deleted" > "$SVN_WC/deleteme.txt"
svn add "$SVN_WC/deleteme.txt" -q 2>/dev/null || true
svn commit "$SVN_WC" -m "Add deleteme.txt" --non-interactive -q
svn rm "$SVN_WC/deleteme.txt" -q
svn commit "$SVN_WC" -m "Remove deleteme.txt" --non-interactive -q
if ! svn cat "$SVN_URL/deleteme.txt" >/dev/null 2>&1; then
    record_scenario "4d-file-deletion" "pass" "file removed from HEAD"
    emit_event "local-svn" "4d-deletion" "pass" 0
else
    record_scenario "4d-file-deletion" "fail" "file still exists"
    emit_event "local-svn" "4d-deletion" "fail" 0
fi

# --- 4e: Nested directory structure ---
log "▶ 4e: Nested directory structure"
mkdir -p "$SVN_WC/src/main/java"
echo "public class App {}" > "$SVN_WC/src/main/java/App.java"
svn add "$SVN_WC/src" -q 2>/dev/null || true
svn commit "$SVN_WC" -m "Add nested directory structure" --non-interactive -q
NESTED=$(svn cat "$SVN_URL/src/main/java/App.java" 2>/dev/null || echo "")
if [[ "$NESTED" == "public class App {}" ]]; then
    record_scenario "4e-nested-dirs" "pass" "nested file verified"
    emit_event "local-svn" "4e-nested" "pass" 0
else
    record_scenario "4e-nested-dirs" "fail" "unexpected content"
    emit_event "local-svn" "4e-nested" "fail" 0
fi

# Phase 4 rollup.
if [[ $SCENARIO_FAIL -eq 0 ]]; then
    emit_event "local-svn" "complete" "pass" 0
    record_phase "Local SVN scenarios (${SCENARIO_PASS}/${SCENARIO_PASS})" "PASS"
else
    emit_event "local-svn" "complete" "fail" 0
    record_phase "Local SVN scenarios (${SCENARIO_PASS}/$((SCENARIO_PASS + SCENARIO_FAIL)))" "FAIL"
fi

# ============================================================================
# Phase 5: Personal binary log-probe (real process spawn)
# ============================================================================
log ""
log "──────────────────────────────────────────────────────────────"
log "Phase 5/6: Log-probe (spawn-based black-box)"
log "──────────────────────────────────────────────────────────────"

PROBE_DIR="$ARTIFACT_DIR/log-probe"
PROBE_DATA="$WORK_DIR/probe_data"
mkdir -p "$PROBE_DIR" "$PROBE_DATA"

PERSONAL_BIN="$REPO_ROOT/target/debug/reposync-personal"

if [[ -f "$PERSONAL_BIN" ]]; then
    cat > "$WORK_DIR/probe_config.toml" <<TOML
[personal]
log_level = "debug"
data_dir = "$PROBE_DATA"

[svn]
url = "file:///tmp/nonexistent"
username = "test"
password_env = "REPOSYNC_TEST_SVN_PW"

[github]
repo = "test/test"
token_env = "REPOSYNC_TEST_GH_TOKEN"

[developer]
name = "Test User"
email = "test@example.com"
svn_username = "test"
TOML

    emit_event "log-probe" "start" "running" 0

    if "$PERSONAL_BIN" --config "$WORK_DIR/probe_config.toml" log-probe \
        > "$PROBE_DIR/stdout.log" 2> "$PROBE_DIR/stderr.log"; then
        PROBE_LOG="$PROBE_DATA/personal.log"
        if [[ -f "$PROBE_LOG" ]] && grep -q "LOG_PROBE" "$PROBE_LOG"; then
            cp "$PROBE_LOG" "$PROBE_DIR/personal.log"
            emit_event "log-probe" "complete" "pass" 0
            log "  ✅ Log-probe: personal.log written with markers"
            record_phase "Log-probe" "PASS"
        else
            emit_event "log-probe" "complete" "fail" 0
            log "  ❌ Log-probe: personal.log missing or no markers"
            record_phase "Log-probe" "FAIL"
        fi
    else
        emit_event "log-probe" "complete" "fail" 0
        log "  ❌ Log-probe: process exited non-zero"
        record_phase "Log-probe" "FAIL"
    fi
else
    emit_event "log-probe" "complete" "skip" 0
    log "  ⏭  Log-probe: binary not found (build may have failed)"
    record_phase "Log-probe" "FAIL"
fi

# ============================================================================
# Phase 6: Secret redaction scan
# ============================================================================
log ""
log "──────────────────────────────────────────────────────────────"
log "Phase 6/7: Secret redaction scan"
log "──────────────────────────────────────────────────────────────"

REDACT_DIR="$ARTIFACT_DIR/redaction"
mkdir -p "$REDACT_DIR"
emit_event "redaction" "start" "running" 0

LEAKED=false
if grep -rlE '(ghp_|gho_|ghs_|ghu_|github_pat_)[A-Za-z0-9_]{10,}' \
    "$ARTIFACT_DIR" --include='*.log' --include='*.txt' > "$REDACT_DIR/leak-scan.log" 2>&1; then
    LEAKED=true
fi

if $LEAKED; then
    emit_event "redaction" "complete" "fail" 0
    log "  ❌ Secret scan: token patterns found — see $REDACT_DIR/leak-scan.log"
    record_phase "Secret scan" "FAIL"
else
    emit_event "redaction" "complete" "pass" 0
    log "  ✅ Secret scan: no token patterns found"
    record_phase "Secret scan" "PASS"
fi

# ============================================================================
# Phase 7: Real-engine svnserve scenario suite (#62)
# ============================================================================
log ""
log "──────────────────────────────────────────────────────────────"
log "Phase 7/7: Real-engine svnserve scenario suite"
log "──────────────────────────────────────────────────────────────"

REAL_ENGINE_DIR="$ARTIFACT_DIR/real-engine-scenarios"
mkdir -p "$REAL_ENGINE_DIR"
emit_event "real-engine-scenarios" "start" "running" 0
if scripts/real-engine-scenario-suite.sh > "$REAL_ENGINE_DIR/suite.log" 2>&1; then
    emit_event "real-engine-scenarios" "complete" "pass" 0
    log "  ✅ Real-engine scenario suite PASS"
    record_phase "Real-engine scenarios" "PASS"
else
    suite_exit=$?
    if grep -q '"overall": "NOT RUN"' "$REAL_ENGINE_DIR"/*/summary.json 2>/dev/null || grep -q '"overall": "NOT RUN"' artifacts/real-engine-scenarios/*/summary.json 2>/dev/null; then
        emit_event "real-engine-scenarios" "complete" "not_run" 0
        log "  ⏭  Real-engine scenario suite NOT RUN (missing tools)"
        record_phase "Real-engine scenarios" "NOT RUN"
    else
        emit_event "real-engine-scenarios" "complete" "fail" 0
        log "  ❌ Real-engine scenario suite FAIL — see $REAL_ENGINE_DIR/suite.log"
        record_phase "Real-engine scenarios" "FAIL"
    fi
    if [[ $suite_exit -ne 2 ]]; then
        : # keep overall FAIL when scenarios failed
    fi
fi

# ============================================================================
# Summary
# ============================================================================
TOTAL_PHASES=${#PHASE_NAMES[@]}
OVERALL="PASS"
EXIT_CODE=0
if [[ $PHASE_FAIL -gt 0 || $SCENARIO_FAIL -gt 0 ]]; then
    OVERALL="FAIL"
    EXIT_CODE=1
fi

log ""
log "═══════════════════════════════════════════════════════════════"
log "VALIDATION COMPLETE — $OVERALL"
log "  Phases: $PHASE_PASS/$TOTAL_PHASES passed"
log "  SVN scenarios: $SCENARIO_PASS/$((SCENARIO_PASS + SCENARIO_FAIL + SCENARIO_SKIP)) passed"
log "═══════════════════════════════════════════════════════════════"

# Build summary table rows.
SUMMARY_ROWS=""
for i in "${!PHASE_NAMES[@]}"; do
    SUMMARY_ROWS="${SUMMARY_ROWS}| ${PHASE_NAMES[$i]} | ${PHASE_RESULTS[$i]} |
"
done

cat > "$SUMMARY_FILE" <<SUMMARY
# Controlled-Environment Validation Summary

**Timestamp:** $TIMESTAMP
**Overall:** $OVERALL
**Phases passed:** $PHASE_PASS / $TOTAL_PHASES
**SVN scenarios passed:** $SCENARIO_PASS / $((SCENARIO_PASS + SCENARIO_FAIL + SCENARIO_SKIP))
**Quick mode:** $QUICK_MODE

## Phase Results

| Phase | Result |
|-------|--------|
${SUMMARY_ROWS}
## Notes

This validation uses **local-only** resources (file:// SVN, local Git, no network).
For real GitHub Enterprise + SVN validation, run:
\`\`\`bash
scripts/ghe-live-validation.sh --dry-run    # preflight
scripts/ghe-live-validation.sh --cycles 3   # live run
\`\`\`

## Artifact Directory

\`$ARTIFACT_DIR\`
SUMMARY

# Write manifest.json.
cd "$ARTIFACT_DIR"
MANIFEST_ENTRIES=""
while IFS= read -r -d '' file; do
    REL="${file#"$ARTIFACT_DIR"/}"
    SIZE=$(wc -c < "$file" 2>/dev/null | tr -d ' ')
    MANIFEST_ENTRIES="${MANIFEST_ENTRIES}{\"path\":\"$REL\",\"size\":$SIZE},"
done < <(find "$ARTIFACT_DIR" -type f -print0 | sort -z)
MANIFEST_ENTRIES="${MANIFEST_ENTRIES%,}"

cat > "$MANIFEST_FILE" <<MANIFEST
{
  "timestamp": "$TIMESTAMP",
  "overall": "$OVERALL",
  "phases_pass": $PHASE_PASS,
  "phases_total": $TOTAL_PHASES,
  "scenarios_pass": $SCENARIO_PASS,
  "scenarios_fail": $SCENARIO_FAIL,
  "artifacts": [$MANIFEST_ENTRIES]
}
MANIFEST

log "Artifacts: $ARTIFACT_DIR"
exit $EXIT_CODE
