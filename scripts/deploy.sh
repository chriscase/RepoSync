#!/usr/bin/env bash
# =============================================================================
# deploy.sh - Push, build, install, and restart RepoSync on the dev server
# =============================================================================
# Usage:
#   scripts/deploy.sh [--branch <branch>] [--no-web-ui] [--dry-run]
#
# Requires:
#   - SSH access to the server (via rk10 alias or GIT_SSH_COMMAND override)
#   - git remote "server" pointing to rk10:GitSvnSync
#   - Rust toolchain available on the server (via rustup)
#
# Static files:
#   The daemon serves static files from a "static/" directory next to the
#   binary (target/release/static/). The web UI is built locally and synced
#   to that location via scp.
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---- Configuration -----------------------------------------------------------
SSH_HOST="${DEPLOY_SSH_HOST:-rk10}"
SSH_OPTS="${DEPLOY_SSH_OPTS:--o ControlMaster=no -o ControlPath=none}"
REMOTE_REPO_DIR="~/GitSvnSync"
BRANCH="${DEPLOY_BRANCH:-main}"
BUILD_WEB_UI=true
DRY_RUN=false
HEALTH_URL="http://localhost:8080/api/status/health"

# Parse arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --branch)    BRANCH="$2"; shift 2 ;;
        --no-web-ui) BUILD_WEB_UI=false; shift ;;
        --dry-run)   DRY_RUN=true; shift ;;
        *)           echo "Unknown argument: $1"; exit 1 ;;
    esac
done

# ---- Helpers -----------------------------------------------------------------
log()  { echo "[deploy] $*"; }
step() { echo ""; echo "==> $*"; }
ts()   { date '+%H:%M:%S'; }
ssh_cmd() { ssh $SSH_OPTS "$SSH_HOST" "bash -c '$*'"; }

run() {
    if [[ "$DRY_RUN" == "true" ]]; then
        echo "  [dry-run] $*"
    else
        "$@"
    fi
}

# ---- Pre-flight checks -------------------------------------------------------
step "Pre-flight checks ($(ts))"

cd "$REPO_ROOT"

LOCAL_SHA="$(git rev-parse --short HEAD)"
log "Local commit: $LOCAL_SHA ($(git log --oneline -1))"

if [[ -n "$(git status --porcelain)" ]]; then
    log "WARNING: working tree has uncommitted changes (they will NOT be deployed)"
fi

# Verify SSH connectivity
if [[ "$DRY_RUN" != "true" ]]; then
    log "Testing SSH connectivity to $SSH_HOST..."
    if ! ssh $SSH_OPTS -o ConnectTimeout=10 "$SSH_HOST" true 2>/dev/null; then
        echo "ERROR: Cannot connect to $SSH_HOST. Check SSH config and VPN."
        exit 1
    fi
    log "SSH OK"
fi

# ---- Phase 1: Push code ------------------------------------------------------
step "Phase 1: Push code to server ($(ts))"
# Push to a temp branch to avoid "refusing to update checked out branch" error
run env GIT_SSH_COMMAND="ssh $SSH_OPTS" git push server "$BRANCH":deploy-incoming --force

# ---- Phase 2: Build web UI locally -------------------------------------------
if [[ "$BUILD_WEB_UI" == "true" ]]; then
    step "Phase 2: Build web UI locally ($(ts))"
    run bash -c "cd '$REPO_ROOT/web-ui' && npm install --silent && npm run build"
    log "Web UI built ($(ls web-ui/dist/assets/index-*.js | xargs basename))"
else
    step "Phase 2: Skipping web UI build (--no-web-ui)"
fi

# ---- Phase 3: Remote merge, build, deploy ------------------------------------
step "Phase 3: Remote build + install + restart ($(ts))"

log "Merging deploy-incoming on server..."
ssh_cmd "cd $REMOTE_REPO_DIR && git merge deploy-incoming && git branch -d deploy-incoming"

log "Building release binary..."
ssh_cmd "cd $REMOTE_REPO_DIR && source ~/.cargo/env && cargo build --release --bin reposync-daemon 2>&1 | tail -3"

# ---- Phase 4: Deploy static files -------------------------------------------
if [[ "$BUILD_WEB_UI" == "true" ]]; then
    step "Phase 4: Deploy static files ($(ts))"

    # The binary serves from {binary_dir}/static/ — which is target/release/static/
    REMOTE_STATIC_DIR="$REMOTE_REPO_DIR/target/release/static"

    # Clean and copy
    ssh_cmd "rm -rf $REMOTE_STATIC_DIR && mkdir -p $REMOTE_STATIC_DIR"
    scp $SSH_OPTS -r "$REPO_ROOT/web-ui/dist/"* "$SSH_HOST:$REMOTE_STATIC_DIR/" 2>/dev/null

    # Verify the right file landed
    REMOTE_JS=$(ssh_cmd "ls $REMOTE_STATIC_DIR/assets/index-*.js | xargs basename")
    LOCAL_JS=$(basename web-ui/dist/assets/index-*.js)
    if [[ "$REMOTE_JS" == "$LOCAL_JS" ]]; then
        log "Static files verified: $LOCAL_JS"
    else
        echo "ERROR: Static file mismatch! Local: $LOCAL_JS, Remote: $REMOTE_JS"
        exit 1
    fi
fi

# ---- Phase 5: Restart daemon ------------------------------------------------
step "Phase 5: Restart daemon ($(ts))"
# Use systemd if the service is enabled (preferred), fall back to restart script.
ssh_cmd "sudo systemctl restart reposync 2>/dev/null && echo 'restarted via systemd' || REPOSYNC_BIN=$REMOTE_REPO_DIR/target/release/reposync-daemon bash $REMOTE_REPO_DIR/scripts/restart-daemon.sh" || true

# ---- Phase 6: Post-deploy verification --------------------------------------
step "Phase 6: Verify deployment ($(ts))"

HEALTH=$(ssh_cmd "curl -sf $HEALTH_URL 2>/dev/null || echo FAILED")
log "Health response: $HEALTH"

# Extract version and git commit from health response (parse locally)
DEPLOYED_SHA=$(echo "$HEALTH" | grep -o '"git_commit":"[^"]*"' | cut -d'"' -f4)
DEPLOYED_VER=$(echo "$HEALTH" | grep -o '"version":"[^"]*"' | cut -d'"' -f4)
DEPLOYED_SHA="${DEPLOYED_SHA:-unknown}"
DEPLOYED_VER="${DEPLOYED_VER:-unknown}"

log "Deployed: v$DEPLOYED_VER, commit: $DEPLOYED_SHA"
log "Expected: commit $LOCAL_SHA"

if [[ "$DEPLOYED_SHA" == "$LOCAL_SHA" ]]; then
    log "VERIFIED: deployed commit matches local HEAD"
elif [[ "$DEPLOYED_SHA" == "${LOCAL_SHA}+dirty" ]]; then
    log "VERIFIED: deployed commit matches (with uncommitted changes on server)"
else
    echo ""
    echo "WARNING: Deploy verification FAILED!"
    echo "  Expected commit: $LOCAL_SHA"
    echo "  Deployed commit: $DEPLOYED_SHA"
    echo "  The running binary may not match the code you intended to deploy."
    echo ""
fi

# ---- Done --------------------------------------------------------------------
step "Deploy complete ($(ts))"
log "Commit:  $DEPLOYED_SHA"
log "Version: $DEPLOYED_VER"
log "URL:     http://orw-chrisc-rk10.wv.mentorg.com:8080"
