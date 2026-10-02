#!/usr/bin/env bash
# ============================================================================
# RepoSync GHE Live Validation Script
# ============================================================================
# Real end-to-end bidirectional validation against a live GitHub Enterprise
# instance and a real SVN repository.  This is NOT a local simulation.
#
# --dry-run is local tool preflight only. It never calls GHE or SVN.
# Dry-run and scenario results are NOT ENTERPRISE QUALIFICATION.
# local/offline PASS is not enterprise/live PASS.
# A scenario pass is not Chris's active-environment GO.
# Live mode requires Chris/admin approval, an allowlisted disposable target,
# and dedicated credentials. This script does not self-authorize that.
#
# Environment variables (required for live run; not needed for --dry-run):
#   GHE_API_URL     — GitHub Enterprise API base URL
#   GHE_TOKEN       — GitHub PAT (repo scope)
#   GHE_OWNER       — Repository owner/org
#   GHE_REPO        — Repository name (will be created if missing)
#   SVN_URL         — SVN repository URL (must be writable)
#   SVN_USERNAME    — SVN username
#   SVN_PASSWORD    — SVN password (or set SVN_PASSWORD_ENV to the env var name)
#
# Optional:
#   GHE_WEB_URL     — Web base URL (defaults derived from GHE_API_URL)
#   REPOSYNC_CONFIG — Path to reposync personal config (auto-generated if absent)
#
# Usage:
#   scripts/ghe-live-validation.sh --dry-run                  # preflight only
#   scripts/ghe-live-validation.sh --cycles 3 --interval 5    # 3 live cycles
#   scripts/ghe-live-validation.sh --strict --cycles 5        # fail-fast on any scenario
#   scripts/ghe-live-validation.sh --config /path/to/personal.toml --cycles 1
#   scripts/ghe-live-validation.sh --help
#
# Output: artifacts/ghe-live-validation/<UTC_TIMESTAMP>/
# Exit code: 0 when executed scenarios meet the script gate, or when --dry-run
# finishes local preflight. Exit code 0 is not production acceptance.
# ============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

TIMESTAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ID="ghe-live-$TIMESTAMP"

# Defaults
CYCLES=1
INTERVAL_SEC=5
DRY_RUN=false
STRICT=false
CUSTOM_CONFIG=""
CUSTOM_ARTIFACTS=""

# Counters
SCENARIO_PASS=0
SCENARIO_FAIL=0
SCENARIO_SKIP=0
CYCLE_PASS=0
CYCLE_FAIL=0

# ============================================================================
# Argument parsing
# ============================================================================
while [[ $# -gt 0 ]]; do
    case "$1" in
        --cycles)        CYCLES="$2"; shift 2 ;;
        --interval)      INTERVAL_SEC="$2"; shift 2 ;;
        --dry-run)       DRY_RUN=true; shift ;;
        --strict)        STRICT=true; shift ;;
        --config)        CUSTOM_CONFIG="$2"; shift 2 ;;
        --artifacts-dir) CUSTOM_ARTIFACTS="$2"; shift 2 ;;
        --help|-h)
            cat <<'USAGE'
Usage: scripts/ghe-live-validation.sh [OPTIONS]

Options:
  --dry-run          Local tool preflight only. Never calls GHE or SVN.
                     Not enterprise qualification, even if credentials are set.
  --cycles N         Number of validation cycles (default: 1)
  --interval N       Seconds between cycles (default: 5)
  --strict           Fail immediately on any scenario failure
  --config PATH      Path to reposync personal config
  --artifacts-dir D  Override artifact output directory
  --help             Show this help

Required environment variables (for live run):
  GHE_API_URL, GHE_TOKEN, GHE_OWNER, GHE_REPO
  SVN_URL, SVN_USERNAME, SVN_PASSWORD
USAGE
            exit 0
            ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

# Resolve artifact directory.
if [[ -n "$CUSTOM_ARTIFACTS" ]]; then
    ARTIFACT_DIR="$CUSTOM_ARTIFACTS"
else
    ARTIFACT_DIR="$REPO_ROOT/artifacts/ghe-live-validation/$TIMESTAMP"
fi
EVENTS_FILE="$ARTIFACT_DIR/events.ndjson"
TIMELINE_LOG="$ARTIFACT_DIR/timeline.log"
SUMMARY_FILE="$ARTIFACT_DIR/summary.md"
MANIFEST_FILE="$ARTIFACT_DIR/manifest.json"
VERIFY_DIR="$ARTIFACT_DIR/verification"

mkdir -p "$ARTIFACT_DIR" "$VERIFY_DIR"

# ============================================================================
# Helpers
# ============================================================================

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

record_scenario() {
    local name="$1" result="$2" detail="${3:-}"
    emit_event "scenario" "$name" "$result" 0
    if [[ "$result" == "pass" ]]; then
        SCENARIO_PASS=$((SCENARIO_PASS + 1))
        log "  ✅ $name: PASS${detail:+ ($detail)}"
    elif [[ "$result" == "skip" ]]; then
        SCENARIO_SKIP=$((SCENARIO_SKIP + 1))
        log "  ⏭  $name: SKIP${detail:+ ($detail)}"
    else
        SCENARIO_FAIL=$((SCENARIO_FAIL + 1))
        log "  ❌ $name: FAIL${detail:+ ($detail)}"
        if $STRICT; then
            log "STRICT MODE: aborting on first failure"
            write_summary
            exit 1
        fi
    fi
}

# Redact a string for safe display (replace middle with ***).
redact() {
    local val="$1"
    if [[ ${#val} -le 8 ]]; then
        echo "***"
    else
        echo "${val:0:4}***${val: -4}"
    fi
}

# GitHub API helper — includes auth, returns HTTP status.
gh_api() {
    local method="$1" endpoint="$2" body="${3:-}"
    local url="${GHE_API_URL}${endpoint}"
    local args=(-s -w "\n%{http_code}" -H "Authorization: token ${GHE_TOKEN}" -H "Accept: application/vnd.github+json")
    if [[ -n "$body" ]]; then
        args+=(-X "$method" -H "Content-Type: application/json" -d "$body")
    else
        args+=(-X "$method")
    fi
    curl "${args[@]}" "$url" 2>/dev/null
}

# Extract HTTP status from gh_api output (last line).
gh_status() {
    tail -1 <<< "$1"
}

# Extract body from gh_api output (everything except last line).
gh_body() {
    sed '$d' <<< "$1"
}

cleanup() {
    log "Cleaning up temp directories..."
    if [[ -n "${WORK_DIR:-}" && -d "$WORK_DIR" ]]; then
        rm -rf "$WORK_DIR"
    fi
}
trap cleanup EXIT

write_summary() {
    local total=$((SCENARIO_PASS + SCENARIO_FAIL + SCENARIO_SKIP))
    local overall="PASS"
    local qualification_note
    if $DRY_RUN; then
        overall="DRY_RUN"
        qualification_note="**NOT ENTERPRISE QUALIFICATION** — dry-run/preflight only.

No live soak cycles ran. Dry-run does not call GitHub Enterprise or SVN.
local/offline PASS is not enterprise/live PASS.
Do not record this artifact as a GO for a candidate SHA. Chris decides live acceptance."
    elif [[ $SCENARIO_FAIL -gt 0 || $CYCLE_FAIL -gt 0 ]]; then
        overall="FAIL"
        qualification_note="**NO-GO** — failures detected; review timeline.log and verification/ artifacts.

Data-integrity failures are NO-GO even if other cycles passed."
    else
        qualification_note="**SCENARIO PASS — not production acceptance.**

Recorded scenarios passed on this host. This is not Chris's active-environment GO.
**NOT ENTERPRISE QUALIFICATION** of a candidate SHA until the candidate report records this exact head, artifact digest, and tier.
Data-integrity failures elsewhere remain NO-GO even if this run's error count is zero."
    fi

    cat > "$SUMMARY_FILE" <<SUMMARY
# GHE Live Validation Summary

**Run ID:** $RUN_ID
**Timestamp:** $TIMESTAMP
**Overall:** $overall
**Cycles:** $CYCLES (pass: $CYCLE_PASS, fail: $CYCLE_FAIL)
**Scenarios:** $total (pass: $SCENARIO_PASS, fail: $SCENARIO_FAIL, skip: $SCENARIO_SKIP)
**Strict mode:** $STRICT
**Dry-run:** $DRY_RUN

## Environment

| Setting | Value |
|---------|-------|
| GHE API | ${GHE_API_URL:-not set} |
| GHE Repo | ${GHE_OWNER:-?}/${GHE_REPO:-?} |
| SVN URL | ${SVN_URL:-not set} |
| SVN User | ${SVN_USERNAME:-not set} |

## Scenario Results

$(grep '"phase":"scenario"' "$EVENTS_FILE" 2>/dev/null | while IFS= read -r line; do
    action=$(echo "$line" | sed 's/.*"action":"\([^"]*\)".*/\1/')
    status=$(echo "$line" | sed 's/.*"status":"\([^"]*\)".*/\1/')
    printf "| %s | %s |\n" "$action" "$status"
done)

## Artifact Directory

\`$ARTIFACT_DIR\`

## Go/No-Go

$qualification_note
SUMMARY

    # Manifest.
    local entries=""
    while IFS= read -r -d '' file; do
        local rel="${file#"$ARTIFACT_DIR"/}"
        local size
        size=$(wc -c < "$file" 2>/dev/null | tr -d ' ')
        entries="${entries}{\"path\":\"$rel\",\"size\":$size},"
    done < <(find "$ARTIFACT_DIR" -type f -print0 2>/dev/null | sort -z)
    entries="${entries%,}"

    cat > "$MANIFEST_FILE" <<MANIFEST
{
  "run_id": "$RUN_ID",
  "timestamp": "$TIMESTAMP",
  "overall": "$overall",
  "enterprise_qualification": "NOT RUN",
  "production_acceptance": "NOT DECIDED",
  "cycles": $CYCLES,
  "cycle_pass": $CYCLE_PASS,
  "cycle_fail": $CYCLE_FAIL,
  "scenario_pass": $SCENARIO_PASS,
  "scenario_fail": $SCENARIO_FAIL,
  "scenario_skip": $SCENARIO_SKIP,
  "artifacts": [$entries]
}
MANIFEST
}

# ============================================================================
# Banner
# ============================================================================
log "═══════════════════════════════════════════════════════════════"
log "RepoSync GHE Live Validation"
log "Run ID:    $RUN_ID"
log "Timestamp: $TIMESTAMP"
log "Cycles:    $CYCLES | Interval: ${INTERVAL_SEC}s | Strict: $STRICT"
log "Artifacts: $ARTIFACT_DIR"
log "═══════════════════════════════════════════════════════════════"

# ============================================================================
# Preflight: tools
# ============================================================================
log ""
log "──────────────────────────────────────────────────────────────"
log "Preflight: required tools"
log "──────────────────────────────────────────────────────────────"

emit_event "preflight" "tools" "running" 0
MISSING=""
for tool in svn git curl cargo jq; do
    if command -v "$tool" &>/dev/null; then
        log "  ✓ $tool"
    else
        MISSING="$MISSING $tool"
        log "  ✗ $tool — MISSING"
    fi
done
if [[ -n "$MISSING" ]]; then
    log "FATAL: Missing tools:$MISSING"
    emit_event "preflight" "tools" "fail" 0
    exit 1
fi
emit_event "preflight" "tools" "pass" 0

# Save sanitized environment.
env | sort | grep -v -iE '(token|password|secret|key|credential|auth)' \
    > "$ARTIFACT_DIR/env-snapshot.txt" 2>/dev/null || true
{
    echo "svn: $(svn --version --quiet 2>/dev/null || echo unknown)"
    echo "git: $(git --version 2>/dev/null || echo unknown)"
    echo "cargo: $(cargo --version 2>/dev/null || echo unknown)"
    echo "rustc: $(rustc --version 2>/dev/null || echo unknown)"
    echo "curl: $(curl --version 2>/dev/null | head -1 || echo unknown)"
    echo "jq: $(jq --version 2>/dev/null || echo unknown)"
    echo "os: $(uname -srm 2>/dev/null || echo unknown)"
} > "$ARTIFACT_DIR/tool-versions.txt"

# ============================================================================
# Preflight: environment variables
# ============================================================================
log ""
log "──────────────────────────────────────────────────────────────"
log "Preflight: environment variables"
log "──────────────────────────────────────────────────────────────"

emit_event "preflight" "env-vars" "running" 0
ENV_OK=true
MISSING_VARS=""

check_var() {
    local name="$1"
    local val="${!name:-}"
    if [[ -n "$val" ]]; then
        log "  ✓ $name = $(redact "$val")"
    else
        log "  ✗ $name — NOT SET"
        MISSING_VARS="${MISSING_VARS}  $name\n"
        ENV_OK=false
    fi
}

check_var GHE_API_URL
check_var GHE_TOKEN
check_var GHE_OWNER
check_var GHE_REPO
check_var SVN_URL
check_var SVN_USERNAME
check_var SVN_PASSWORD

if $DRY_RUN; then
    log ""
    if $ENV_OK; then
        emit_event "preflight" "env-vars" "pass" 0
        log "Credential variables are present and were not used."
    else
        emit_event "preflight" "env-vars" "not-run" 0
        log "Credential variables are unset (expected for local preflight):"
        printf "  %b" "$MISSING_VARS" | tee -a "$TIMELINE_LOG"
    fi
    log ""
    log "DRY RUN COMPLETE — local preflight only."
    log "NOT ENTERPRISE QUALIFICATION"
    log "local/offline PASS is not enterprise/live PASS"
    log "Dry-run does not call GitHub Enterprise or SVN."
    emit_event "dry-run" "complete" "pass" 0 "enterprise_qualification=NOT RUN"
    write_summary
    log "Artifacts: $ARTIFACT_DIR"
    exit 0
fi

if ! $ENV_OK; then
    emit_event "preflight" "env-vars" "fail" 0
    log ""
    log "FATAL: Missing required environment variables:"
    printf "  %b" "$MISSING_VARS" | tee -a "$TIMELINE_LOG"
    log ""
    log "Set these variables and re-run, or use --dry-run for local preflight only."
    log "A live run needs Chris/admin approval and an allowlisted disposable target."
    log "This script does not self-authorize that run."
    exit 1
fi
emit_event "preflight" "env-vars" "pass" 0

# ============================================================================
# Live validation — set up working directories
# ============================================================================
log ""
log "──────────────────────────────────────────────────────────────"
log "Provisioning live validation environment"
log "──────────────────────────────────────────────────────────────"

WORK_DIR=$(mktemp -d)
SVN_WC="$WORK_DIR/svn_wc"
GIT_CLONE="$WORK_DIR/git_clone"
DATA_DIR="$WORK_DIR/data"
mkdir -p "$DATA_DIR"

# Ensure binary is built.
PERSONAL_BIN="$REPO_ROOT/target/debug/reposync-personal"
if [[ ! -f "$PERSONAL_BIN" ]]; then
    log "Building workspace..."
    if ! cargo build --workspace > "$ARTIFACT_DIR/build-stdout.log" 2> "$ARTIFACT_DIR/build-stderr.log"; then
        log "FATAL: Build failed — see $ARTIFACT_DIR/build-stderr.log"
        exit 1
    fi
fi

# SVN checkout.
log "▶ SVN checkout: $SVN_URL"
if svn checkout "$SVN_URL" "$SVN_WC" \
    --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
    --non-interactive --no-auth-cache \
    > "$VERIFY_DIR/svn-checkout.log" 2>&1; then
    log "  ✓ SVN checkout complete"
    emit_event "provision" "svn-checkout" "pass" 0
else
    log "  ✗ SVN checkout failed — see $VERIFY_DIR/svn-checkout.log"
    emit_event "provision" "svn-checkout" "fail" 0
    exit 1
fi

# GHE: ensure repo exists.
log "▶ GHE: verifying repo ${GHE_OWNER}/${GHE_REPO}"
REPO_RESP=$(gh_api GET "/repos/${GHE_OWNER}/${GHE_REPO}" "")
REPO_CODE=$(gh_status "$REPO_RESP")
if [[ "$REPO_CODE" == "200" ]]; then
    log "  ✓ Repo exists"
    emit_event "provision" "ghe-repo-check" "pass" 0
elif [[ "$REPO_CODE" == "404" ]]; then
    log "  Repo not found — creating ${GHE_OWNER}/${GHE_REPO}..."
    CREATE_RESP=$(gh_api POST "/user/repos" "{\"name\":\"${GHE_REPO}\",\"private\":true,\"auto_init\":true,\"description\":\"RepoSync live validation canary\"}")
    CREATE_CODE=$(gh_status "$CREATE_RESP")
    if [[ "$CREATE_CODE" == "201" ]]; then
        log "  ✓ Repo created"
        emit_event "provision" "ghe-repo-create" "pass" 0
        sleep 2  # Give GitHub a moment.
    else
        log "  ✗ Repo creation failed: HTTP $CREATE_CODE"
        gh_body "$CREATE_RESP" > "$VERIFY_DIR/repo-create-error.json"
        emit_event "provision" "ghe-repo-create" "fail" 0
        exit 1
    fi
else
    log "  ✗ Repo check failed: HTTP $REPO_CODE"
    emit_event "provision" "ghe-repo-check" "fail" 0
    exit 1
fi

# Git clone.
GHE_WEB="${GHE_WEB_URL:-$(echo "$GHE_API_URL" | sed 's|/api/v3||; s|/api||')}"
CLONE_URL="${GHE_WEB}/${GHE_OWNER}/${GHE_REPO}.git"
log "▶ Git clone: $CLONE_URL"
if GIT_ASKPASS=true git clone \
    -c "http.extraHeader=Authorization: token ${GHE_TOKEN}" \
    "$CLONE_URL" "$GIT_CLONE" > "$VERIFY_DIR/git-clone.log" 2>&1; then
    log "  ✓ Git clone complete"
    emit_event "provision" "git-clone" "pass" 0
else
    log "  ✗ Git clone failed — see $VERIFY_DIR/git-clone.log"
    emit_event "provision" "git-clone" "fail" 0
    exit 1
fi

# Configure git identity and persist auth for subsequent pulls.
git -C "$GIT_CLONE" config user.name "reposync-validator"
git -C "$GIT_CLONE" config user.email "validator@reposync.local"
git -C "$GIT_CLONE" config "http.extraHeader" "Authorization: token ${GHE_TOKEN}"

log "✅ Provisioning complete"
emit_event "provision" "complete" "pass" 0

# ============================================================================
# Cycle loop
# ============================================================================
for cycle in $(seq 1 "$CYCLES"); do
    log ""
    log "══════════════════════════════════════════════════════════════"
    log "Cycle $cycle/$CYCLES"
    log "══════════════════════════════════════════════════════════════"

    CYCLE_START=$SECONDS
    CYCLE_OK=true
    CANARY="canary_c${cycle}_$(date +%s)"
    CYCLE_DIR="$ARTIFACT_DIR/cycle-$(printf '%03d' "$cycle")"
    mkdir -p "$CYCLE_DIR"
    emit_event "cycle-$cycle" "start" "running" 0

    # ---- Scenario 1: SVN→ add file ----
    log "▶ S1: SVN add file"
    SVN_FILE="$CANARY.txt"
    SVN_CONTENT="svn-add-${CANARY}"
    echo "$SVN_CONTENT" > "$SVN_WC/$SVN_FILE"
    svn add "$SVN_WC/$SVN_FILE" -q 2>/dev/null || true
    if svn commit "$SVN_WC" -m "Validation: add $SVN_FILE" \
        --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
        --non-interactive --no-auth-cache > "$CYCLE_DIR/s1-commit.log" 2>&1; then
        SVN_REV=$(svn info "$SVN_URL" --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
            --non-interactive --no-auth-cache --show-item revision --no-newline 2>/dev/null || echo "?")
        VERIFY=$(svn cat "$SVN_URL/$SVN_FILE" --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
            --non-interactive --no-auth-cache 2>/dev/null || echo "")
        if [[ "$VERIFY" == "$SVN_CONTENT" ]]; then
            record_scenario "s1-svn-add" "pass" "r${SVN_REV}"
        else
            record_scenario "s1-svn-add" "fail" "content mismatch after commit"
            CYCLE_OK=false
        fi
    else
        record_scenario "s1-svn-add" "fail" "svn commit failed"
        CYCLE_OK=false
    fi

    # ---- Scenario 2: SVN→ modify file ----
    log "▶ S2: SVN modify file"
    SVN_MOD_CONTENT="svn-modified-${CANARY}"
    echo "$SVN_MOD_CONTENT" > "$SVN_WC/$SVN_FILE"
    if svn commit "$SVN_WC" -m "Validation: modify $SVN_FILE" \
        --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
        --non-interactive --no-auth-cache > "$CYCLE_DIR/s2-commit.log" 2>&1; then
        VERIFY=$(svn cat "$SVN_URL/$SVN_FILE" --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
            --non-interactive --no-auth-cache 2>/dev/null || echo "")
        if [[ "$VERIFY" == "$SVN_MOD_CONTENT" ]]; then
            record_scenario "s2-svn-modify" "pass"
        else
            record_scenario "s2-svn-modify" "fail" "content mismatch"
            CYCLE_OK=false
        fi
    else
        record_scenario "s2-svn-modify" "fail" "svn commit failed"
        CYCLE_OK=false
    fi

    # ---- Scenario 3: SVN→ delete file ----
    log "▶ S3: SVN delete file"
    svn rm "$SVN_WC/$SVN_FILE" -q 2>/dev/null || true
    if svn commit "$SVN_WC" -m "Validation: delete $SVN_FILE" \
        --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
        --non-interactive --no-auth-cache > "$CYCLE_DIR/s3-commit.log" 2>&1; then
        if ! svn cat "$SVN_URL/$SVN_FILE" --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
            --non-interactive --no-auth-cache >/dev/null 2>&1; then
            record_scenario "s3-svn-delete" "pass"
        else
            record_scenario "s3-svn-delete" "fail" "file still exists after delete"
            CYCLE_OK=false
        fi
    else
        record_scenario "s3-svn-delete" "fail" "svn commit failed"
        CYCLE_OK=false
    fi

    # ---- Scenario 4: SVN→ nested directory ----
    log "▶ S4: SVN nested directory"
    NEST_DIR="$SVN_WC/nested_${CANARY}"
    mkdir -p "$NEST_DIR/sub/deep"
    echo "nested-${CANARY}" > "$NEST_DIR/sub/deep/file.txt"
    svn add "$NEST_DIR" -q 2>/dev/null || true
    if svn commit "$SVN_WC" -m "Validation: nested dirs $CANARY" \
        --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
        --non-interactive --no-auth-cache > "$CYCLE_DIR/s4-commit.log" 2>&1; then
        VERIFY=$(svn cat "$SVN_URL/nested_${CANARY}/sub/deep/file.txt" \
            --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
            --non-interactive --no-auth-cache 2>/dev/null || echo "")
        if [[ "$VERIFY" == "nested-${CANARY}" ]]; then
            record_scenario "s4-svn-nested" "pass"
        else
            record_scenario "s4-svn-nested" "fail" "nested file content mismatch"
            CYCLE_OK=false
        fi
    else
        record_scenario "s4-svn-nested" "fail" "svn commit failed"
        CYCLE_OK=false
    fi

    # ---- Scenario 4b: SVN→Git sync verification ----
    # After SVN mutations, invoke reposync to sync SVN→Git and verify files
    # appear in the Git clone.  This is the *real* sync test — without it, we
    # are only validating that SVN CLI operations work, not that RepoSync
    # actually syncs them.
    log "▶ S4b: SVN→Git sync (invoke reposync-personal sync)"

    SYNC_CONFIG="$WORK_DIR/sync_config_c${cycle}.toml"
    SYNC_DATA="$WORK_DIR/sync_data_c${cycle}"
    mkdir -p "$SYNC_DATA"

    cat > "$SYNC_CONFIG" <<TOML
[personal]
poll_interval_secs = 30
data_dir = "$SYNC_DATA"

[svn]
url = "${SVN_URL}"
username = "${SVN_USERNAME}"
password_env = "SVN_PASSWORD"

[github]
api_url = "${GHE_API_URL}"
repo = "${GHE_OWNER}/${GHE_REPO}"
token_env = "GHE_TOKEN"
default_branch = "main"

[developer]
name = "reposync-validator"
email = "validator@reposync.local"
svn_username = "${SVN_USERNAME}"
TOML

    if "$PERSONAL_BIN" --config "$SYNC_CONFIG" sync \
        > "$CYCLE_DIR/s4b-sync-stdout.log" 2> "$CYCLE_DIR/s4b-sync-stderr.log"; then
        # Pull latest Git state — fail scenario if auth/network prevents pull.
        if ! GIT_ASKPASS=true git -C "$GIT_CLONE" pull --ff-only \
            > "$CYCLE_DIR/s4b-git-pull.log" 2>&1; then
            # Redact any tokens that might appear in the error output.
            sed -i.bak "s|${GHE_TOKEN}|[REDACTED]|g" "$CYCLE_DIR/s4b-git-pull.log" 2>/dev/null || true
            rm -f "$CYCLE_DIR/s4b-git-pull.log.bak"
            record_scenario "s4b-svn-to-git-sync" "fail" "git pull failed — see s4b-git-pull.log"
            CYCLE_OK=false
        else
            # The nested directory from S4 should now exist in Git.
            if [[ -d "$GIT_CLONE/nested_${CANARY}" ]] && \
               [[ -f "$GIT_CLONE/nested_${CANARY}/sub/deep/file.txt" ]]; then
                NESTED_CONTENT=$(cat "$GIT_CLONE/nested_${CANARY}/sub/deep/file.txt" 2>/dev/null || echo "")
                if [[ "$NESTED_CONTENT" == "nested-${CANARY}" ]]; then
                    record_scenario "s4b-svn-to-git-sync" "pass" "nested files verified in Git"
                else
                    record_scenario "s4b-svn-to-git-sync" "fail" "nested file content mismatch in Git"
                    CYCLE_OK=false
                fi
            else
                # Files might not appear if sync logic filters them — check logs.
                SVN_TO_GIT_COUNT=$(grep -oE 'SVN→Git: [0-9]+ commits' "$CYCLE_DIR/s4b-sync-stdout.log" 2>/dev/null || echo "")
                record_scenario "s4b-svn-to-git-sync" "fail" "nested dir not found in Git (sync output: ${SVN_TO_GIT_COUNT:-none})"
                CYCLE_OK=false
            fi
        fi
    else
        record_scenario "s4b-svn-to-git-sync" "fail" "reposync sync exited non-zero"
        CYCLE_OK=false
    fi

    # ---- Scenario 5: Git→ create branch + commit via GHE API ----
    # RepoSync only syncs *merged PR* commits from Git→SVN. Direct pushes to
    # main are NOT replayed. So we must: create branch → commit → open PR →
    # merge PR → sync → verify in SVN.
    log "▶ S5: Git create branch + commit file via PR"
    PR_BRANCH="validation/${CANARY}"
    GIT_FILE="git_${CANARY}.txt"
    GIT_CONTENT="git-pr-add-${CANARY}"
    GIT_CONTENT_B64=$(echo -n "$GIT_CONTENT" | base64)

    # Get main branch SHA for creating the new branch.
    MAIN_REF_RESP=$(gh_api GET "/repos/${GHE_OWNER}/${GHE_REPO}/git/ref/heads/main" "")
    MAIN_REF_CODE=$(gh_status "$MAIN_REF_RESP")
    MAIN_SHA=$(gh_body "$MAIN_REF_RESP" | jq -r '.object.sha // ""' 2>/dev/null || echo "")

    if [[ "$MAIN_REF_CODE" == "200" && -n "$MAIN_SHA" ]]; then
        # Create feature branch.
        CREATE_REF_RESP=$(gh_api POST "/repos/${GHE_OWNER}/${GHE_REPO}/git/refs" \
            "{\"ref\":\"refs/heads/${PR_BRANCH}\",\"sha\":\"${MAIN_SHA}\"}")
        CREATE_REF_CODE=$(gh_status "$CREATE_REF_RESP")
        if [[ "$CREATE_REF_CODE" == "201" ]]; then
            # Commit file on the feature branch.
            COMMIT_RESP=$(gh_api PUT "/repos/${GHE_OWNER}/${GHE_REPO}/contents/${GIT_FILE}" \
                "{\"message\":\"Validation: add $GIT_FILE via PR\",\"content\":\"${GIT_CONTENT_B64}\",\"branch\":\"${PR_BRANCH}\"}")
            COMMIT_CODE=$(gh_status "$COMMIT_RESP")
            if [[ "$COMMIT_CODE" == "201" ]]; then
                GIT_SHA=$(gh_body "$COMMIT_RESP" | jq -r '.commit.sha // "unknown"' 2>/dev/null || echo "unknown")
                echo "$GIT_SHA" > "$CYCLE_DIR/s5-git-sha.txt"
                record_scenario "s5-git-branch-commit" "pass" "sha=${GIT_SHA:0:8} on ${PR_BRANCH}"
            else
                record_scenario "s5-git-branch-commit" "fail" "commit HTTP $COMMIT_CODE"
                gh_body "$COMMIT_RESP" > "$CYCLE_DIR/s5-error.json" 2>/dev/null || true
                CYCLE_OK=false
            fi
        else
            record_scenario "s5-git-branch-commit" "fail" "create-ref HTTP $CREATE_REF_CODE"
            CYCLE_OK=false
        fi
    else
        record_scenario "s5-git-branch-commit" "fail" "could not get main SHA (HTTP $MAIN_REF_CODE)"
        CYCLE_OK=false
    fi

    # ---- Scenario 6: Open + merge PR via GHE API ----
    log "▶ S6: Open and merge PR"
    PR_MERGED=false
    PR_MERGE_SHA=""
    if [[ -f "$CYCLE_DIR/s5-git-sha.txt" ]]; then
        # Open PR.
        PR_CREATE_RESP=$(gh_api POST "/repos/${GHE_OWNER}/${GHE_REPO}/pulls" \
            "{\"title\":\"Validation: ${CANARY}\",\"head\":\"${PR_BRANCH}\",\"base\":\"main\",\"body\":\"Automated RepoSync validation PR\"}")
        PR_CREATE_CODE=$(gh_status "$PR_CREATE_RESP")
        PR_NUMBER=$(gh_body "$PR_CREATE_RESP" | jq -r '.number // ""' 2>/dev/null || echo "")
        if [[ "$PR_CREATE_CODE" == "201" && -n "$PR_NUMBER" ]]; then
            # Merge the PR (squash merge to keep it clean).
            sleep 1  # Give GitHub a moment for merge-ability check.
            MERGE_RESP=$(gh_api PUT "/repos/${GHE_OWNER}/${GHE_REPO}/pulls/${PR_NUMBER}/merge" \
                "{\"merge_method\":\"squash\",\"commit_title\":\"Validation: merge ${CANARY}\"}")
            MERGE_CODE=$(gh_status "$MERGE_RESP")
            if [[ "$MERGE_CODE" == "200" ]]; then
                PR_MERGE_SHA=$(gh_body "$MERGE_RESP" | jq -r '.sha // ""' 2>/dev/null || echo "")
                PR_MERGED=true
                echo "$PR_NUMBER" > "$CYCLE_DIR/s6-pr-number.txt"
                echo "$PR_MERGE_SHA" > "$CYCLE_DIR/s6-merge-sha.txt"
                record_scenario "s6-git-pr-merge" "pass" "PR #${PR_NUMBER} merged (sha=${PR_MERGE_SHA:0:8})"
            else
                record_scenario "s6-git-pr-merge" "fail" "merge HTTP $MERGE_CODE"
                gh_body "$MERGE_RESP" > "$CYCLE_DIR/s6-merge-error.json" 2>/dev/null || true
                CYCLE_OK=false
            fi
        else
            record_scenario "s6-git-pr-merge" "fail" "create-PR HTTP $PR_CREATE_CODE"
            gh_body "$PR_CREATE_RESP" > "$CYCLE_DIR/s6-pr-error.json" 2>/dev/null || true
            CYCLE_OK=false
        fi
    else
        record_scenario "s6-git-pr-merge" "skip" "S5 did not produce a commit"
    fi

    # Clean up the feature branch (best-effort).
    gh_api DELETE "/repos/${GHE_OWNER}/${GHE_REPO}/git/refs/heads/${PR_BRANCH}" "" > /dev/null 2>&1 || true

    # ---- Scenario 7: Git→SVN sync via merged PR ----
    # This is the critical Git→SVN proof.  We invoke reposync-personal sync
    # after the PR merge, then verify:
    #   (a) the PR's file content appears in SVN
    #   (b) the SVN log for the replayed commit contains Git-SHA / PR metadata
    log "▶ S7: Git→SVN sync (merged-PR replay)"

    if $PR_MERGED; then
        if "$PERSONAL_BIN" --config "$SYNC_CONFIG" sync \
            > "$CYCLE_DIR/s7-sync-stdout.log" 2> "$CYCLE_DIR/s7-sync-stderr.log"; then
            # Update SVN working copy to pick up any replayed commits.
            svn update "$SVN_WC" --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
                --non-interactive --no-auth-cache > "$CYCLE_DIR/s7-svn-update.log" 2>&1 || true

            # (a) Verify the PR-committed file now exists in SVN with correct content.
            VERIFY_SVN=$(svn cat "$SVN_URL/$GIT_FILE" --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
                --non-interactive --no-auth-cache 2>/dev/null || echo "")
            if [[ "$VERIFY_SVN" != "$GIT_CONTENT" ]]; then
                SYNC_OUTPUT=$(cat "$CYCLE_DIR/s7-sync-stdout.log" 2>/dev/null || echo "")
                if echo "$SYNC_OUTPUT" | grep -qE 'Git→SVN: [1-9][0-9]* commits|prs_synced.*[1-9]'; then
                    record_scenario "s7-git-to-svn-sync" "fail" "sync reported commits but file not in SVN"
                else
                    record_scenario "s7-git-to-svn-sync" "fail" "no PR replay occurred — file not in SVN (content: '${VERIFY_SVN:0:40}')"
                fi
                CYCLE_OK=false
            else
                # (b) Verify the replayed SVN commit carries EXACT Git/PR metadata.
                # RepoSync formats commit messages with "Git-Commit: <sha>" and
                # "PR: #N (<branch>)" trailers. We require an exact match to the
                # values recorded in S6 — a generic "some trailer exists" is not
                # sufficient to prove this cycle's PR was actually replayed.
                SVN_LOG_XML=$(svn log "$SVN_URL/$GIT_FILE" --xml -l 3 \
                    --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
                    --non-interactive --no-auth-cache 2>/dev/null || echo "")
                echo "$SVN_LOG_XML" > "$CYCLE_DIR/s7-svn-log.xml"

                S7_PROOF_DETAILS="content=OK"
                S7_PASS=true

                # Load expected values from S6 artifacts.
                EXPECTED_PR=$(cat "$CYCLE_DIR/s6-pr-number.txt" 2>/dev/null || echo "")
                EXPECTED_SHA=$(cat "$CYCLE_DIR/s6-merge-sha.txt" 2>/dev/null || echo "")

                # --- Git-Commit trailer: require exact SHA match ---
                if [[ -z "$EXPECTED_SHA" ]]; then
                    S7_PROOF_DETAILS="${S7_PROOF_DETAILS}, Git-Commit=FAIL(no expected SHA from S6)"
                    S7_PASS=false
                elif echo "$SVN_LOG_XML" | grep -q "Git-Commit: ${EXPECTED_SHA}"; then
                    S7_PROOF_DETAILS="${S7_PROOF_DETAILS}, Git-Commit=${EXPECTED_SHA:0:8}=exact"
                else
                    OBSERVED_SHA=$(echo "$SVN_LOG_XML" | sed -n 's/.*Git-Commit: \([0-9a-f]*\).*/\1/p' | head -1)
                    S7_PROOF_DETAILS="${S7_PROOF_DETAILS}, Git-Commit=MISMATCH(expected=${EXPECTED_SHA:0:8},observed=${OBSERVED_SHA:-MISSING})"
                    S7_PASS=false
                fi

                # --- PR trailer: require exact "#N" match ---
                if [[ -z "$EXPECTED_PR" ]]; then
                    S7_PROOF_DETAILS="${S7_PROOF_DETAILS}, PR=FAIL(no expected PR# from S6)"
                    S7_PASS=false
                elif echo "$SVN_LOG_XML" | grep -q "#${EXPECTED_PR}"; then
                    S7_PROOF_DETAILS="${S7_PROOF_DETAILS}, PR=#${EXPECTED_PR}=exact"
                else
                    OBSERVED_PR=$(echo "$SVN_LOG_XML" | sed -n 's/.*PR: #\([0-9]*\).*/\1/p' | head -1)
                    S7_PROOF_DETAILS="${S7_PROOF_DETAILS}, PR=MISMATCH(expected=#${EXPECTED_PR},observed=#${OBSERVED_PR:-MISSING})"
                    S7_PASS=false
                fi

                if $S7_PASS; then
                    record_scenario "s7-git-to-svn-sync" "pass" "$S7_PROOF_DETAILS"
                else
                    record_scenario "s7-git-to-svn-sync" "fail" "content OK but provenance mismatch ($S7_PROOF_DETAILS)"
                    CYCLE_OK=false
                fi
            fi
        else
            SYNC_EXIT=$?
            record_scenario "s7-git-to-svn-sync" "fail" "reposync sync exited with code $SYNC_EXIT"
            CYCLE_OK=false
        fi
    else
        record_scenario "s7-git-to-svn-sync" "skip" "PR was not merged (S6 failed/skipped)"
    fi

    # Capture sync engine logs as artifacts.
    if [[ -d "$SYNC_DATA" ]]; then
        cp -r "$SYNC_DATA" "$CYCLE_DIR/sync-engine-data" 2>/dev/null || true
    fi

    # ---- Scenario 8: Echo/duplicate suppression marker ----
    log "▶ S8: Echo suppression marker in SVN"
    ECHO_FILE="echo_${CANARY}.txt"
    echo "echo-content" > "$SVN_WC/$ECHO_FILE"
    svn add "$SVN_WC/$ECHO_FILE" -q 2>/dev/null || true
    if svn commit "$SVN_WC" -m "Synced from Git [reposync] validation echo" \
        --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
        --non-interactive --no-auth-cache > "$CYCLE_DIR/s8-commit.log" 2>&1; then
        ECHO_REV=$(svn info "$SVN_URL" --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
            --non-interactive --no-auth-cache --show-item revision --no-newline 2>/dev/null || echo "?")
        ECHO_LOG=$(svn log "$SVN_URL" -r "$ECHO_REV" --xml \
            --username "$SVN_USERNAME" --password "$SVN_PASSWORD" \
            --non-interactive --no-auth-cache 2>/dev/null || echo "")
        if echo "$ECHO_LOG" | grep -q "\[reposync\]"; then
            record_scenario "s8-echo-marker" "pass" "marker at r$ECHO_REV"
        else
            record_scenario "s8-echo-marker" "fail" "no marker in r$ECHO_REV"
            CYCLE_OK=false
        fi
    else
        record_scenario "s8-echo-marker" "fail" "svn commit failed"
        CYCLE_OK=false
    fi

    # ---- Scenario 9: GHE API health — rate limit check ----
    log "▶ S9: GHE API rate limit health"
    RATE_RESP=$(gh_api GET "/rate_limit" "")
    RATE_CODE=$(gh_status "$RATE_RESP")
    if [[ "$RATE_CODE" == "200" ]]; then
        REMAINING=$(gh_body "$RATE_RESP" | jq -r '.rate.remaining // "?"' 2>/dev/null || echo "?")
        LIMIT=$(gh_body "$RATE_RESP" | jq -r '.rate.limit // "?"' 2>/dev/null || echo "?")
        gh_body "$RATE_RESP" > "$CYCLE_DIR/s9-rate-limit.json"
        if [[ "$REMAINING" != "?" && "$REMAINING" -gt 100 ]]; then
            record_scenario "s9-rate-limit" "pass" "${REMAINING}/${LIMIT} remaining"
        else
            record_scenario "s9-rate-limit" "fail" "only ${REMAINING}/${LIMIT} remaining"
            CYCLE_OK=false
        fi
    else
        record_scenario "s9-rate-limit" "fail" "HTTP $RATE_CODE"
        CYCLE_OK=false
    fi

    # ---- Scenario 10: Log-probe (daemon logging health) ----
    log "▶ S10: Log-probe"
    PROBE_DATA="$WORK_DIR/probe_c${cycle}"
    mkdir -p "$PROBE_DATA"
    cat > "$WORK_DIR/probe_c${cycle}.toml" <<TOML
[personal]
log_level = "info"
data_dir = "$PROBE_DATA"

[svn]
url = "${SVN_URL}"
username = "${SVN_USERNAME}"
password_env = "SVN_PASSWORD"

[github]
api_url = "${GHE_API_URL}"
repo = "${GHE_OWNER}/${GHE_REPO}"
token_env = "GHE_TOKEN"

[developer]
name = "reposync-validator"
email = "validator@reposync.local"
svn_username = "${SVN_USERNAME}"
TOML

    if "$PERSONAL_BIN" --config "$WORK_DIR/probe_c${cycle}.toml" log-probe \
        > "$CYCLE_DIR/s10-probe-stdout.log" 2> "$CYCLE_DIR/s10-probe-stderr.log"; then
        if [[ -f "$PROBE_DATA/personal.log" ]] && grep -q "LOG_PROBE" "$PROBE_DATA/personal.log"; then
            cp "$PROBE_DATA/personal.log" "$CYCLE_DIR/daemon.log"
            record_scenario "s10-log-probe" "pass"
        else
            record_scenario "s10-log-probe" "fail" "no LOG_PROBE marker in personal.log"
            CYCLE_OK=false
        fi
    else
        record_scenario "s10-log-probe" "fail" "process exited non-zero"
        CYCLE_OK=false
    fi

    # ---- Cycle verdict ----
    CYCLE_DUR=$(( (SECONDS - CYCLE_START) * 1000 ))
    if $CYCLE_OK; then
        CYCLE_PASS=$((CYCLE_PASS + 1))
        emit_event "cycle-$cycle" "complete" "pass" "$CYCLE_DUR"
        log "✅ Cycle $cycle PASS (${CYCLE_DUR}ms)"
    else
        CYCLE_FAIL=$((CYCLE_FAIL + 1))
        emit_event "cycle-$cycle" "complete" "fail" "$CYCLE_DUR"
        log "❌ Cycle $cycle FAIL (${CYCLE_DUR}ms)"
    fi

    # Inter-cycle pause.
    if [[ $cycle -lt $CYCLES ]]; then
        sleep "$INTERVAL_SEC"
    fi
done

# ============================================================================
# Secret scan
# ============================================================================
log ""
log "──────────────────────────────────────────────────────────────"
log "Post-run: secret leak scan"
log "──────────────────────────────────────────────────────────────"

LEAKED=false
if grep -rlE '(ghp_|gho_|ghs_|ghu_|github_pat_)[A-Za-z0-9_]{10,}' \
    "$ARTIFACT_DIR" --include='*.log' --include='*.txt' --include='*.json' \
    > "$VERIFY_DIR/leak-scan.log" 2>&1; then
    LEAKED=true
fi
# Also scan for raw tokens (first 8 chars of GHE_TOKEN).
TOKEN_PREFIX="${GHE_TOKEN:0:8}"
if grep -rl "$TOKEN_PREFIX" "$ARTIFACT_DIR" --include='*.log' --include='*.txt' \
    >> "$VERIFY_DIR/leak-scan.log" 2>&1; then
    LEAKED=true
fi

if $LEAKED; then
    log "  ⚠ Token patterns found — see $VERIFY_DIR/leak-scan.log"
    emit_event "secret-scan" "complete" "fail" 0
else
    log "  ✅ No token patterns found in artifacts"
    emit_event "secret-scan" "complete" "pass" 0
fi

# ============================================================================
# Final summary
# ============================================================================
TOTAL=$((SCENARIO_PASS + SCENARIO_FAIL + SCENARIO_SKIP))
EXIT_CODE=0
if [[ $SCENARIO_FAIL -gt 0 || $CYCLE_FAIL -gt 0 ]]; then
    EXIT_CODE=1
fi

log ""
log "═══════════════════════════════════════════════════════════════"
if [[ $EXIT_CODE -eq 0 ]]; then
    log "VALIDATION COMPLETE — SCENARIO PASS (not a production GO)"
    log "NOT ENTERPRISE QUALIFICATION until a candidate report records this SHA."
else
    log "VALIDATION COMPLETE — NO-GO"
fi
log "  Cycles: $CYCLE_PASS/$CYCLES passed"
log "  Scenarios: $SCENARIO_PASS/$TOTAL passed ($SCENARIO_FAIL failed, $SCENARIO_SKIP skipped)"
if $LEAKED; then log "  ⚠ Secret leakage detected in artifacts"; fi
log "═══════════════════════════════════════════════════════════════"

write_summary
log "Artifacts: $ARTIFACT_DIR"
exit $EXIT_CODE
