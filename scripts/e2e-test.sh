#!/usr/bin/env bash
# =============================================================================
# e2e-test.sh — End-to-end bidirectional sync test for RepoSync
# =============================================================================
#
# Setup:
#   1. Copy e2e-config.example.sh to e2e-config.sh
#   2. Fill in your paths, branch names, and optionally GHE_TOKEN
#   3. Run: bash scripts/e2e-test.sh [--rounds N] [--pause SECS]
#
# What it does (each round):
#   1. SVN commits on all 3 branches (add, modify, new subdirs)
#   2. Git commits on all 3 branches (add, modify, new subdirs)
#   3. Waits for bidirectional sync
#   4. Verifies every file exists in both SVN and Git
#   5. PR merge chain: Feature -> Developer -> Sandbox (if GHE_TOKEN set)
#   6. Verifies merged files propagated through the chain
#   7. Deletion tests (every other round)
#   8. Optionally checks daemon logs for errors
#
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ---- Load config ------------------------------------------------------------

CONFIG_FILE="${SCRIPT_DIR}/e2e-config.sh"
if [[ -f "$CONFIG_FILE" ]]; then
    source "$CONFIG_FILE"
else
    echo "ERROR: Config file not found: $CONFIG_FILE"
    echo "Copy the example and fill in your values:"
    echo "  cp scripts/e2e-config.example.sh scripts/e2e-config.sh"
    exit 1
fi

# ---- Parse CLI overrides ----------------------------------------------------

while [[ $# -gt 0 ]]; do
    case "$1" in
        --rounds) ROUNDS="$2"; shift 2 ;;
        --pause)  PAUSE="$2";  shift 2 ;;
        --config) source "$2"; shift 2 ;;
        -h|--help)
            echo "Usage: e2e-test.sh [--rounds N] [--pause SECS] [--config FILE]"
            exit 0 ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
done

# ---- Defaults for anything not set in config --------------------------------

ROUNDS="${ROUNDS:-3}"
PAUSE="${PAUSE:-90}"
TEST_DIR="${TEST_DIR:-source/tests/e2e}"
GHE_TOKEN="${GHE_TOKEN:-}"
REPOSYNC_SSH_HOST="${REPOSYNC_SSH_HOST:-}"
REPOSYNC_SSH_OPTS="${REPOSYNC_SSH_OPTS:-}"

# ---- Helpers ----------------------------------------------------------------

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'
PASS=0
FAIL=0
TOTAL_CHECKS=0

ts()   { date '+%H:%M:%S'; }
log()  { echo -e "${NC}[$(ts)] $*"; }
step() { echo -e "${CYAN}${BOLD}==> $*${NC}"; }
pass() { echo -e "  ${GREEN}[PASS]${NC} $*"; PASS=$((PASS + 1)); TOTAL_CHECKS=$((TOTAL_CHECKS + 1)); }
fail() { echo -e "  ${RED}[FAIL]${NC} $*"; FAIL=$((FAIL + 1)); TOTAL_CHECKS=$((TOTAL_CHECKS + 1)); }
warn() { echo -e "  ${YELLOW}[WARN]${NC} $*"; }

wait_for_sync() {
    local reason="${1:-sync cycle}"
    log "Waiting ${PAUSE}s for $reason..."
    sleep "$PAUSE"
}

# ---- SVN helpers ------------------------------------------------------------

svn_add_and_commit() {
    local wc="$1" msg="$2"
    (
        cd "$wc"
        svn update -q 2>/dev/null || true
        # Add any new files/dirs under the test directory
        svn add --force --parents "$TEST_DIR" 2>/dev/null || true
        svn commit -m "$msg" -q 2>/dev/null && \
            log "  SVN: $msg" || \
            warn "  SVN commit returned non-zero (may be nothing to commit): $msg"
    )
}

svn_delete_and_commit() {
    local wc="$1" file="$2" msg="$3"
    (
        cd "$wc"
        svn update -q 2>/dev/null || true
        svn rm "$file" 2>/dev/null || true
        svn commit -m "$msg" -q 2>/dev/null && \
            log "  SVN: $msg" || \
            warn "  SVN delete commit returned non-zero: $msg"
    )
}

# ---- Git helpers ------------------------------------------------------------

git_add_and_push() {
    local branch="$1" msg="$2"
    (
        cd "$GIT_REPO"
        git checkout "$branch" -q 2>/dev/null
        git fetch origin -q 2>/dev/null
        git reset --hard "origin/$branch" -q 2>/dev/null

        git add "$TEST_DIR" 2>/dev/null || true
        git commit -m "$msg" -q 2>/dev/null || { warn "  Git: nothing to commit"; return 0; }

        # Push with retry on non-fast-forward
        if ! git push origin "$branch" -q 2>/dev/null; then
            git pull origin "$branch" --rebase -q 2>/dev/null
            git push origin "$branch" -q 2>/dev/null || warn "  Git push failed after retry"
        fi
        log "  Git: $msg ($branch)"
    )
}

git_delete_and_push() {
    local branch="$1" file="$2" msg="$3"
    (
        cd "$GIT_REPO"
        git checkout "$branch" -q 2>/dev/null
        git fetch origin -q 2>/dev/null
        git reset --hard "origin/$branch" -q 2>/dev/null

        git rm "$file" -q 2>/dev/null || { warn "  Git: file not found for delete"; return 0; }
        git commit -m "$msg" -q 2>/dev/null

        if ! git push origin "$branch" -q 2>/dev/null; then
            git pull origin "$branch" --rebase -q 2>/dev/null
            git push origin "$branch" -q 2>/dev/null || warn "  Git push failed after retry"
        fi
        log "  Git: $msg ($branch)"
    )
}

# ---- PR helpers -------------------------------------------------------------

create_and_merge_pr() {
    local head="$1" base="$2" title="$3"
    if [[ -z "$GHE_TOKEN" ]]; then
        warn "No GHE_TOKEN — skipping PR: $title"
        return 0
    fi

    local pr_json
    pr_json=$(curl -s -X POST "$GHE_API/repos/$GHE_REPO/pulls" \
        -H "Authorization: token $GHE_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"title\": \"$title\", \"head\": \"$head\", \"base\": \"$base\"}" 2>/dev/null)

    local pr_num
    pr_num=$(echo "$pr_json" | grep -o '"number": [0-9]*' | head -1 | grep -o '[0-9]*')

    if [[ -z "$pr_num" ]]; then
        # PR may already exist — try to find it
        pr_num=$(curl -s "$GHE_API/repos/$GHE_REPO/pulls?state=open&head=${GHE_REPO%%/*}:$head&base=$base" \
            -H "Authorization: token $GHE_TOKEN" 2>/dev/null | \
            grep -o '"number": [0-9]*' | head -1 | grep -o '[0-9]*')
    fi

    if [[ -z "$pr_num" ]]; then
        warn "Could not create or find PR: $title"
        return 1
    fi

    sleep 3

    local merge_result
    merge_result=$(curl -s -X PUT "$GHE_API/repos/$GHE_REPO/pulls/$pr_num/merge" \
        -H "Authorization: token $GHE_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"merge_method\": \"merge\", \"commit_title\": \"$title\"}" 2>/dev/null)

    if echo "$merge_result" | grep -q '"merged": true'; then
        log "  PR #$pr_num merged: $title"
    else
        warn "  PR #$pr_num merge issue: $title (may need rebase or already merged)"
    fi
}

# ---- Verification helpers ---------------------------------------------------

verify_file_exists() {
    local svn_wc="$1" git_branch="$2" file_rel="$3" desc="$4"

    local svn_has=false git_has=false

    ( cd "$svn_wc" && svn update -q 2>/dev/null )
    [[ -f "$svn_wc/$file_rel" ]] && svn_has=true

    (
        cd "$GIT_REPO"
        git checkout "$git_branch" -q 2>/dev/null
        git fetch origin -q 2>/dev/null
        git reset --hard "origin/$git_branch" -q 2>/dev/null
    )
    [[ -f "$GIT_REPO/$file_rel" ]] && git_has=true

    if [[ "$svn_has" == true && "$git_has" == true ]]; then
        pass "$desc"
    elif [[ "$svn_has" == true ]]; then
        fail "$desc — in SVN but NOT in Git"
    elif [[ "$git_has" == true ]]; then
        fail "$desc — in Git but NOT in SVN"
    else
        fail "$desc — NOT in either"
    fi
}

verify_file_gone() {
    local svn_wc="$1" git_branch="$2" file_rel="$3" desc="$4"

    ( cd "$svn_wc" && svn update -q 2>/dev/null )
    local svn_gone=true git_gone=true
    [[ -f "$svn_wc/$file_rel" ]] && svn_gone=false

    (
        cd "$GIT_REPO"
        git checkout "$git_branch" -q 2>/dev/null
        git fetch origin -q 2>/dev/null
        git reset --hard "origin/$git_branch" -q 2>/dev/null
    )
    [[ -f "$GIT_REPO/$file_rel" ]] && git_gone=false

    if [[ "$svn_gone" == true && "$git_gone" == true ]]; then
        pass "$desc — deleted from both"
    else
        fail "$desc — SVN:$(${svn_gone} && echo gone || echo exists) Git:$(${git_gone} && echo gone || echo exists)"
    fi
}

# ---- Server log check -------------------------------------------------------

check_daemon_errors() {
    local since_secs="$1"
    if [[ -z "$REPOSYNC_SSH_HOST" ]]; then
        return 0
    fi
    local errors
    errors=$(ssh $REPOSYNC_SSH_OPTS "$REPOSYNC_SSH_HOST" \
        "journalctl -u reposync --since '${since_secs} sec ago' --no-pager 2>/dev/null | grep -c ERROR" 2>/dev/null || echo "0")

    if [[ "$errors" -gt 0 ]]; then
        fail "Daemon had $errors ERROR(s) in the last ${since_secs}s"
        ssh $REPOSYNC_SSH_OPTS "$REPOSYNC_SSH_HOST" \
            "journalctl -u reposync --since '${since_secs} sec ago' --no-pager 2>/dev/null | grep ERROR | tail -3" 2>/dev/null || true
    else
        pass "Daemon: 0 errors in last ${since_secs}s"
    fi
}

# ---- Pre-flight -------------------------------------------------------------

echo ""
echo -e "${BOLD}=========================================="
echo "  RepoSync E2E Test"
echo "==========================================${NC}"
echo ""
log "Config:  $CONFIG_FILE"
log "Rounds:  $ROUNDS"
log "Pause:   ${PAUSE}s"
log "SVN:     $SVN_SANDBOX"
log "         $SVN_DEV"
log "         $SVN_FEATURE"
log "Git:     $GIT_REPO"
log "Branches: $GIT_BRANCH_SANDBOX / $GIT_BRANCH_DEV / $GIT_BRANCH_FEATURE"
log "GHE:     ${GHE_TOKEN:+set}${GHE_TOKEN:-NOT SET (PR tests skipped)}"
log "SSH:     ${REPOSYNC_SSH_HOST:-NOT SET (log checks skipped)}"
echo ""

# Validate paths exist
for path in "$SVN_SANDBOX" "$SVN_DEV" "$SVN_FEATURE" "$GIT_REPO"; do
    if [[ ! -d "$path" ]]; then
        echo -e "${RED}ERROR: Directory not found: $path${NC}"
        echo "Check your e2e-config.sh paths."
        exit 1
    fi
done

# Ensure test directories exist
for wc in "$SVN_SANDBOX" "$SVN_DEV" "$SVN_FEATURE"; do
    mkdir -p "$wc/$TEST_DIR" 2>/dev/null || true
done

# ---- Test Rounds ------------------------------------------------------------

for round in $(seq 1 "$ROUNDS"); do
    ROUND_START=$(date +%s)
    TAG="r${round}-$(date +%s)"

    echo ""
    echo -e "${BOLD}=========================================="
    echo "  ROUND $round of $ROUNDS"
    echo "==========================================${NC}"

    # ---- SVN commits on all branches ----
    step "SVN commits (add + subdirectory)"

    # Sandbox: add file
    mkdir -p "$SVN_SANDBOX/$TEST_DIR"
    echo "SVN sandbox $TAG" > "$SVN_SANDBOX/$TEST_DIR/${TAG}-svn-sandbox.txt"
    svn_add_and_commit "$SVN_SANDBOX" "R$round: SVN add on Sandbox"

    # Developer1: add file
    mkdir -p "$SVN_DEV/$TEST_DIR"
    echo "SVN dev1 $TAG" > "$SVN_DEV/$TEST_DIR/${TAG}-svn-dev1.txt"
    svn_add_and_commit "$SVN_DEV" "R$round: SVN add on Developer1"

    # Feature1: add file + new subdirectory
    mkdir -p "$SVN_FEATURE/$TEST_DIR/${TAG}-svn-subdir"
    echo "SVN feat1 $TAG" > "$SVN_FEATURE/$TEST_DIR/${TAG}-svn-feat1.txt"
    echo "SVN nested $TAG" > "$SVN_FEATURE/$TEST_DIR/${TAG}-svn-subdir/nested.txt"
    svn_add_and_commit "$SVN_FEATURE" "R$round: SVN add + subdir on Feature1"

    # ---- Git commits on all branches ----
    step "Git commits (add + subdirectory)"

    # Sandbox: add file
    mkdir -p "$GIT_REPO/$TEST_DIR"
    echo "Git sandbox $TAG" > "$GIT_REPO/$TEST_DIR/${TAG}-git-sandbox.txt"
    git_add_and_push "$GIT_BRANCH_SANDBOX" "R$round: Git add on Sandbox"

    # Developer1: add file
    mkdir -p "$GIT_REPO/$TEST_DIR"
    echo "Git dev1 $TAG" > "$GIT_REPO/$TEST_DIR/${TAG}-git-dev1.txt"
    git_add_and_push "$GIT_BRANCH_DEV" "R$round: Git add on Developer1"

    # Feature1: add file + new subdirectory
    mkdir -p "$GIT_REPO/$TEST_DIR/${TAG}-git-subdir"
    echo "Git feat1 $TAG" > "$GIT_REPO/$TEST_DIR/${TAG}-git-feat1.txt"
    echo "Git deep $TAG" > "$GIT_REPO/$TEST_DIR/${TAG}-git-subdir/deep.txt"
    git_add_and_push "$GIT_BRANCH_FEATURE" "R$round: Git add + subdir on Feature1"

    # ---- Wait for bidirectional sync ----
    step "Waiting for bidirectional sync"
    wait_for_sync "SVN->Git sync"
    wait_for_sync "Git->SVN sync"

    # ---- Verify all files synced ----
    step "Verifying bidirectional sync"

    verify_file_exists "$SVN_SANDBOX" "$GIT_BRANCH_SANDBOX" \
        "$TEST_DIR/${TAG}-svn-sandbox.txt" "R$round SVN->Git: Sandbox file"

    verify_file_exists "$SVN_SANDBOX" "$GIT_BRANCH_SANDBOX" \
        "$TEST_DIR/${TAG}-git-sandbox.txt" "R$round Git->SVN: Sandbox file"

    verify_file_exists "$SVN_DEV" "$GIT_BRANCH_DEV" \
        "$TEST_DIR/${TAG}-svn-dev1.txt" "R$round SVN->Git: Developer1 file"

    verify_file_exists "$SVN_DEV" "$GIT_BRANCH_DEV" \
        "$TEST_DIR/${TAG}-git-dev1.txt" "R$round Git->SVN: Developer1 file"

    verify_file_exists "$SVN_FEATURE" "$GIT_BRANCH_FEATURE" \
        "$TEST_DIR/${TAG}-svn-feat1.txt" "R$round SVN->Git: Feature1 file"

    verify_file_exists "$SVN_FEATURE" "$GIT_BRANCH_FEATURE" \
        "$TEST_DIR/${TAG}-git-feat1.txt" "R$round Git->SVN: Feature1 file"

    verify_file_exists "$SVN_FEATURE" "$GIT_BRANCH_FEATURE" \
        "$TEST_DIR/${TAG}-svn-subdir/nested.txt" "R$round SVN->Git: Feature1 SVN subdirectory"

    verify_file_exists "$SVN_FEATURE" "$GIT_BRANCH_FEATURE" \
        "$TEST_DIR/${TAG}-git-subdir/deep.txt" "R$round Git->SVN: Feature1 Git subdirectory"

    # ---- PR merge chain: Feature -> Developer -> Sandbox ----
    if [[ -n "$GHE_TOKEN" ]]; then
        step "PR merge: Feature1 -> Developer1"
        create_and_merge_pr "$GIT_BRANCH_FEATURE" "$GIT_BRANCH_DEV" \
            "R$round: Merge Feature1 into Developer1"
        wait_for_sync "merge sync to SVN"

        step "PR merge: Developer1 -> Sandbox"
        create_and_merge_pr "$GIT_BRANCH_DEV" "$GIT_BRANCH_SANDBOX" \
            "R$round: Promote Developer1 to Sandbox"
        wait_for_sync "promote sync to SVN"

        step "Verifying merge chain propagation"
        verify_file_exists "$SVN_SANDBOX" "$GIT_BRANCH_SANDBOX" \
            "$TEST_DIR/${TAG}-svn-feat1.txt" "R$round merge: Feature1 SVN file reached Sandbox"

        verify_file_exists "$SVN_SANDBOX" "$GIT_BRANCH_SANDBOX" \
            "$TEST_DIR/${TAG}-git-feat1.txt" "R$round merge: Feature1 Git file reached Sandbox"

        verify_file_exists "$SVN_SANDBOX" "$GIT_BRANCH_SANDBOX" \
            "$TEST_DIR/${TAG}-svn-subdir/nested.txt" "R$round merge: Feature1 SVN subdir reached Sandbox"
    fi

    # ---- Deletion test (every other round) ----
    if (( round % 2 == 0 )) && (( round > 1 )); then
        step "Deletion tests"

        # Find a file from the previous round to delete via SVN
        local_prev="r$((round - 1))-"
        del_file=$(ls "$SVN_FEATURE/$TEST_DIR"/${local_prev}*-svn-feat1.txt 2>/dev/null | head -1 || true)
        if [[ -n "$del_file" ]]; then
            del_rel="${del_file#$SVN_FEATURE/}"
            svn_delete_and_commit "$SVN_FEATURE" "$del_rel" \
                "R$round: SVN delete previous round's Feature1 file"
            wait_for_sync "deletion sync"
            verify_file_gone "$SVN_FEATURE" "$GIT_BRANCH_FEATURE" \
                "$del_rel" "R$round SVN->Git: deletion synced"
        fi

        # Find a file from the previous round to delete via Git
        del_git_file=$(ls "$GIT_REPO/$TEST_DIR"/${local_prev}*-git-sandbox.txt 2>/dev/null | head -1 || true)
        if [[ -n "$del_git_file" ]]; then
            del_git_rel="${del_git_file#$GIT_REPO/}"
            git_delete_and_push "$GIT_BRANCH_SANDBOX" "$del_git_rel" \
                "R$round: Git delete previous round's Sandbox file"
            wait_for_sync "deletion sync"
            verify_file_gone "$SVN_SANDBOX" "$GIT_BRANCH_SANDBOX" \
                "$del_git_rel" "R$round Git->SVN: deletion synced"
        fi
    fi

    # ---- Modification test (every round) ----
    step "Modification test"
    (
        cd "$SVN_FEATURE"
        svn update -q 2>/dev/null || true
        echo "Modified by SVN at $(date +%T)" >> "$TEST_DIR/${TAG}-svn-feat1.txt"
        svn commit -m "R$round: SVN modify on Feature1" -q 2>/dev/null || true
        log "  SVN: modified ${TAG}-svn-feat1.txt"
    )

    mkdir -p "$GIT_REPO/$TEST_DIR"
    echo "Modified by Git at $(date +%T)" >> "$GIT_REPO/$TEST_DIR/${TAG}-git-dev1.txt" 2>/dev/null || true
    git_add_and_push "$GIT_BRANCH_DEV" "R$round: Git modify on Developer1"

    # ---- Check daemon logs ----
    ROUND_ELAPSED=$(( $(date +%s) - ROUND_START ))
    step "Checking daemon health"
    check_daemon_errors "$((ROUND_ELAPSED + 30))"

    # ---- Round summary ----
    echo ""
    log "Round $round complete: ${GREEN}$PASS passed${NC} / ${RED}$FAIL failed${NC} (${TOTAL_CHECKS} total checks)"
done

# ---- Final Summary ----------------------------------------------------------

echo ""
echo -e "${BOLD}=========================================="
echo "  E2E TEST COMPLETE"
echo "==========================================${NC}"
echo ""
log "Rounds:  $ROUNDS"
log "Checks:  $TOTAL_CHECKS"
log "Passed:  $PASS"
log "Failed:  $FAIL"
echo ""

if [[ $FAIL -eq 0 ]]; then
    echo -e "${GREEN}${BOLD}  ALL $PASS TESTS PASSED${NC}"
    echo ""
    exit 0
else
    echo -e "${RED}${BOLD}  $FAIL of $TOTAL_CHECKS TESTS FAILED${NC}"
    echo ""
    exit 1
fi
