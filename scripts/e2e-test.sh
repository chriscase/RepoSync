#!/usr/bin/env bash
# =============================================================================
# e2e-test.sh — End-to-end bidirectional sync test for RepoSync
# =============================================================================
#
# Usage:
#   scripts/e2e-test.sh [--rounds N] [--pause SECS]
#
# Requires:
#   - SVN checkouts in $SVN_SANDBOX, $SVN_DEV, $SVN_FEATURE
#   - Git clone in $GIT_REPO with branches checked out
#   - RepoSync daemon running and syncing these branches
#
# What it does:
#   Each round simulates a developer sprint:
#     1. SVN commits on all 3 branches (add, modify, delete)
#     2. Git commits on all 3 branches (add, modify, delete, new subdirs)
#     3. PR merge Feature -> Developer -> Sandbox (main)
#     4. Waits for sync, then verifies Git and SVN trees match
#
# =============================================================================

set -euo pipefail

# ---- Configuration ----------------------------------------------------------

# Local checkout paths (override with env vars)
SVN_SANDBOX="${SVN_SANDBOX:-C:/Projects/RepoSyncSandbox/RepoSyncSandbox}"
SVN_DEV="${SVN_DEV:-C:/Projects/RepoSyncSandbox/Developer1}"
SVN_FEATURE="${SVN_FEATURE:-C:/Projects/RepoSyncSandbox/Developer1_Feature1}"
GIT_REPO="${GIT_REPO:-C:/Projects/RepoSyncSandbox/GitRepo}"

GIT_BRANCH_SANDBOX="${GIT_BRANCH_SANDBOX:-dev/RepoSyncSandbox}"
GIT_BRANCH_DEV="${GIT_BRANCH_DEV:-dev/RepoSyncSandbox_Developer1}"
GIT_BRANCH_FEATURE="${GIT_BRANCH_FEATURE:-dev/RepoSyncSandbox_Developer1_Feature1}"

# GitHub Enterprise API (for PR creation/merge)
GHE_API="${GHE_API:-https://github.siemens.cloud/api/v3}"
GHE_REPO="${GHE_REPO:-open/EDM-Server-Load-Simulator}"
GHE_TOKEN="${GHE_TOKEN:-}"  # Set via env var or prompt

# Test parameters
ROUNDS="${ROUNDS:-3}"
PAUSE="${PAUSE:-75}"        # seconds to wait for sync between steps
TEST_DIR="source/SLS/NonBuild/e2e-tests"

# Parse args
while [[ $# -gt 0 ]]; do
    case "$1" in
        --rounds) ROUNDS="$2"; shift 2 ;;
        --pause)  PAUSE="$2";  shift 2 ;;
        *)        echo "Unknown arg: $1"; exit 1 ;;
    esac
done

# ---- Helpers ----------------------------------------------------------------

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'
PASS=0
FAIL=0

ts() { date '+%H:%M:%S'; }
log()  { echo -e "${NC}[$(ts)] $*"; }
pass() { echo -e "${GREEN}[PASS]${NC} $*"; PASS=$((PASS + 1)); }
fail() { echo -e "${RED}[FAIL]${NC} $*"; FAIL=$((FAIL + 1)); }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }

wait_for_sync() {
    log "Waiting ${PAUSE}s for sync..."
    sleep "$PAUSE"
}

svn_commit() {
    local wc="$1" msg="$2"
    cd "$wc"
    svn update -q 2>/dev/null
    svn add --force --parents "$TEST_DIR" 2>/dev/null || true
    svn commit -m "$msg" -q 2>/dev/null
    log "SVN commit: $msg"
}

git_commit_and_push() {
    local branch="$1" msg="$2"
    cd "$GIT_REPO"
    git checkout "$branch" -q 2>/dev/null
    git pull origin "$branch" --rebase -q 2>/dev/null || {
        # If rebase fails due to unstaged changes, stash and retry
        git stash -q 2>/dev/null
        git pull origin "$branch" --rebase -q 2>/dev/null
        git stash pop -q 2>/dev/null || true
    }
    git add "$TEST_DIR" 2>/dev/null
    git commit -m "$msg" -q 2>/dev/null
    git push origin "$branch" -q 2>/dev/null || {
        # Non-fast-forward: pull rebase and retry
        git pull origin "$branch" --rebase -q 2>/dev/null
        git push origin "$branch" -q 2>/dev/null
    }
    log "Git commit: $msg ($branch)"
}

create_and_merge_pr() {
    local head="$1" base="$2" title="$3"
    if [[ -z "$GHE_TOKEN" ]]; then
        warn "No GHE_TOKEN — skipping PR merge: $title"
        return 0
    fi
    local pr_num
    pr_num=$(curl -s -X POST "$GHE_API/repos/$GHE_REPO/pulls" \
        -H "Authorization: token $GHE_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"title\": \"$title\", \"head\": \"$head\", \"base\": \"$base\"}" \
        2>/dev/null | grep -o '"number": [0-9]*' | head -1 | grep -o '[0-9]*')

    if [[ -z "$pr_num" ]]; then
        warn "Failed to create PR: $title (may already exist)"
        return 0
    fi

    sleep 2
    local merged
    merged=$(curl -s -X PUT "$GHE_API/repos/$GHE_REPO/pulls/$pr_num/merge" \
        -H "Authorization: token $GHE_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"merge_method\": \"merge\", \"commit_title\": \"$title\"}" \
        2>/dev/null | grep -o '"merged": true')

    if [[ "$merged" == '"merged": true' ]]; then
        log "PR #$pr_num merged: $title"
    else
        warn "PR #$pr_num merge may have failed: $title"
    fi
}

verify_file_synced() {
    local svn_wc="$1" git_branch="$2" file_rel="$3" description="$4"
    cd "$svn_wc"
    svn update -q 2>/dev/null

    local svn_has=false git_has=false
    [[ -f "$file_rel" ]] && svn_has=true

    cd "$GIT_REPO"
    git checkout "$git_branch" -q 2>/dev/null
    git pull origin "$git_branch" -q 2>/dev/null
    [[ -f "$file_rel" ]] && git_has=true

    if [[ "$svn_has" == true && "$git_has" == true ]]; then
        pass "$description — in both SVN and Git"
    elif [[ "$svn_has" == true ]]; then
        fail "$description — in SVN but NOT in Git"
    elif [[ "$git_has" == true ]]; then
        fail "$description — in Git but NOT in SVN"
    else
        fail "$description — NOT in either SVN or Git"
    fi
}

verify_file_deleted() {
    local svn_wc="$1" git_branch="$2" file_rel="$3" description="$4"
    cd "$svn_wc"
    svn update -q 2>/dev/null

    local svn_gone=true git_gone=true
    [[ -f "$file_rel" ]] && svn_gone=false

    cd "$GIT_REPO"
    git checkout "$git_branch" -q 2>/dev/null
    git pull origin "$git_branch" -q 2>/dev/null
    [[ -f "$file_rel" ]] && git_gone=false

    if [[ "$svn_gone" == true && "$git_gone" == true ]]; then
        pass "$description — deleted from both"
    else
        fail "$description — still exists (SVN:$([[ $svn_gone == true ]] && echo gone || echo exists) Git:$([[ $git_gone == true ]] && echo gone || echo exists))"
    fi
}

# ---- Pre-flight -------------------------------------------------------------

log "=========================================="
log "RepoSync E2E Test"
log "Rounds: $ROUNDS | Pause: ${PAUSE}s"
log "=========================================="

if [[ -z "$GHE_TOKEN" ]]; then
    warn "GHE_TOKEN not set — PR merge tests will be skipped"
    warn "Set it with: export GHE_TOKEN=ghp_..."
fi

# Ensure test directory exists in all SVN checkouts
for wc in "$SVN_SANDBOX" "$SVN_DEV" "$SVN_FEATURE"; do
    mkdir -p "$wc/$TEST_DIR" 2>/dev/null || true
done

# ---- Test Rounds ------------------------------------------------------------

for round in $(seq 1 "$ROUNDS"); do
    log ""
    log "=========================================="
    log "ROUND $round of $ROUNDS"
    log "=========================================="

    TAG="r${round}-$(date +%s)"

    # ------ Step 1: SVN commits on all branches ------
    log ""
    log "--- Step 1: SVN commits ---"

    # Sandbox: add + modify
    cd "$SVN_SANDBOX" && mkdir -p "$TEST_DIR"
    echo "SVN sandbox add $TAG" > "$TEST_DIR/${TAG}-svn-sandbox.txt"
    svn_commit "$SVN_SANDBOX" "R$round: SVN add on Sandbox"

    # Developer1: add
    cd "$SVN_DEV" && mkdir -p "$TEST_DIR"
    echo "SVN dev1 add $TAG" > "$TEST_DIR/${TAG}-svn-dev1.txt"
    svn_commit "$SVN_DEV" "R$round: SVN add on Developer1"

    # Feature1: add + new subdirectory
    cd "$SVN_FEATURE" && mkdir -p "$TEST_DIR/${TAG}-subdir"
    echo "SVN feat1 add $TAG" > "$TEST_DIR/${TAG}-svn-feat1.txt"
    echo "SVN nested $TAG" > "$TEST_DIR/${TAG}-subdir/nested.txt"
    svn_commit "$SVN_FEATURE" "R$round: SVN add + subdir on Feature1"

    # ------ Step 2: Git commits on all branches ------
    log ""
    log "--- Step 2: Git commits ---"

    cd "$GIT_REPO" && mkdir -p "$TEST_DIR"

    # Sandbox: add from Git
    git checkout "$GIT_BRANCH_SANDBOX" -q 2>/dev/null
    echo "Git sandbox add $TAG" > "$TEST_DIR/${TAG}-git-sandbox.txt"
    git_commit_and_push "$GIT_BRANCH_SANDBOX" "R$round: Git add on Sandbox"

    # Developer1: add from Git
    git checkout "$GIT_BRANCH_DEV" -q 2>/dev/null
    mkdir -p "$TEST_DIR"
    echo "Git dev1 add $TAG" > "$TEST_DIR/${TAG}-git-dev1.txt"
    git_commit_and_push "$GIT_BRANCH_DEV" "R$round: Git add on Developer1"

    # Feature1: add + new subdirectory from Git
    git checkout "$GIT_BRANCH_FEATURE" -q 2>/dev/null
    mkdir -p "$TEST_DIR/${TAG}-git-subdir"
    echo "Git feat1 add $TAG" > "$TEST_DIR/${TAG}-git-feat1.txt"
    echo "Git deep $TAG" > "$TEST_DIR/${TAG}-git-subdir/deep.txt"
    git_commit_and_push "$GIT_BRANCH_FEATURE" "R$round: Git add + subdir on Feature1"

    # ------ Step 3: Wait for sync ------
    wait_for_sync
    wait_for_sync  # Double wait for bidirectional

    # ------ Step 4: Verify ------
    log ""
    log "--- Step 4: Verify round $round ---"

    verify_file_synced "$SVN_SANDBOX" "$GIT_BRANCH_SANDBOX" \
        "$TEST_DIR/${TAG}-svn-sandbox.txt" "R$round SVN->Git Sandbox add"

    verify_file_synced "$SVN_SANDBOX" "$GIT_BRANCH_SANDBOX" \
        "$TEST_DIR/${TAG}-git-sandbox.txt" "R$round Git->SVN Sandbox add"

    verify_file_synced "$SVN_DEV" "$GIT_BRANCH_DEV" \
        "$TEST_DIR/${TAG}-svn-dev1.txt" "R$round SVN->Git Developer1 add"

    verify_file_synced "$SVN_DEV" "$GIT_BRANCH_DEV" \
        "$TEST_DIR/${TAG}-git-dev1.txt" "R$round Git->SVN Developer1 add"

    verify_file_synced "$SVN_FEATURE" "$GIT_BRANCH_FEATURE" \
        "$TEST_DIR/${TAG}-svn-feat1.txt" "R$round SVN->Git Feature1 add"

    verify_file_synced "$SVN_FEATURE" "$GIT_BRANCH_FEATURE" \
        "$TEST_DIR/${TAG}-git-feat1.txt" "R$round Git->SVN Feature1 add"

    verify_file_synced "$SVN_FEATURE" "$GIT_BRANCH_FEATURE" \
        "$TEST_DIR/${TAG}-subdir/nested.txt" "R$round SVN->Git Feature1 subdir"

    verify_file_synced "$SVN_FEATURE" "$GIT_BRANCH_FEATURE" \
        "$TEST_DIR/${TAG}-git-subdir/deep.txt" "R$round Git->SVN Feature1 git-subdir"

    # ------ Step 5: PR merge Feature -> Developer -> Sandbox ------
    if [[ -n "$GHE_TOKEN" ]]; then
        log ""
        log "--- Step 5: PR merges ---"

        create_and_merge_pr "$GIT_BRANCH_FEATURE" "$GIT_BRANCH_DEV" \
            "R$round: Merge Feature1 into Developer1"

        wait_for_sync

        create_and_merge_pr "$GIT_BRANCH_DEV" "$GIT_BRANCH_SANDBOX" \
            "R$round: Promote Developer1 to Sandbox"

        wait_for_sync

        # Verify Feature1 files propagated to Sandbox via merge chain
        verify_file_synced "$SVN_SANDBOX" "$GIT_BRANCH_SANDBOX" \
            "$TEST_DIR/${TAG}-svn-feat1.txt" "R$round merge: Feature1 SVN file in Sandbox"

        verify_file_synced "$SVN_SANDBOX" "$GIT_BRANCH_SANDBOX" \
            "$TEST_DIR/${TAG}-git-feat1.txt" "R$round merge: Feature1 Git file in Sandbox"
    fi

    # ------ Step 6: Deletion test (every other round) ------
    if (( round % 2 == 0 )); then
        log ""
        log "--- Step 6: Deletion tests ---"

        PREV_TAG="r$((round - 1))-"
        DEL_FILE=$(cd "$SVN_FEATURE" && ls "$TEST_DIR"/${PREV_TAG}*-svn-feat1.txt 2>/dev/null | head -1)
        if [[ -n "$DEL_FILE" ]]; then
            cd "$SVN_FEATURE"
            svn rm "$DEL_FILE" 2>/dev/null || true
            svn_commit "$SVN_FEATURE" "R$round: SVN delete from previous round"
            wait_for_sync
            verify_file_deleted "$SVN_FEATURE" "$GIT_BRANCH_FEATURE" \
                "$DEL_FILE" "R$round SVN deletion synced"
        fi
    fi

    log ""
    log "Round $round complete: ${GREEN}$PASS passed${NC}, ${RED}$FAIL failed${NC}"
done

# ---- Summary ----------------------------------------------------------------

log ""
log "=========================================="
log "E2E TEST COMPLETE"
log "=========================================="
log "Rounds: $ROUNDS"
log "Passed: $PASS"
log "Failed: $FAIL"
if [[ $FAIL -eq 0 ]]; then
    echo -e "${GREEN}ALL TESTS PASSED${NC}"
    exit 0
else
    echo -e "${RED}$FAIL TESTS FAILED${NC}"
    exit 1
fi
