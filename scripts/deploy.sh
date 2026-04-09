#!/usr/bin/env bash
# =============================================================================
# deploy.sh - Push, build, install, and restart RepoSync on the dev server
# =============================================================================
# Usage:
#   scripts/deploy.sh [--branch <branch>] [--no-web-ui] [--dry-run]
#
# Requires:
#   - SSH alias "rk10" in ~/.ssh/config (with ControlMaster recommended)
#   - git remote "server" pointing to rk10:RepoSync
#   - Rust toolchain available on rk10 (via rustup)
#   - chrisc has sudo rights for: install, systemctl daemon-reload, systemctl restart
#
# Web UI note:
#   The daemon serves static files from a "static/" directory next to the binary
#   (/usr/local/bin/static/). The web UI dist is built locally (no Node.js needed
#   on the server) and synced via rsync, then sudo-copied into place remotely.
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---- Configuration -----------------------------------------------------------
SSH_HOST="rk10"
REMOTE_REPO_DIR="~/RepoSync"
REMOTE_STATIC_DIR="/usr/local/bin/static"
BRANCH="${DEPLOY_BRANCH:-main}"
BUILD_WEB_UI=true
DRY_RUN=false

# Parse arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --branch)   BRANCH="$2"; shift 2 ;;
        --no-web-ui) BUILD_WEB_UI=false; shift ;;
        --dry-run)  DRY_RUN=true; shift ;;
        *) echo "Unknown argument: $1"; exit 1 ;;
    esac
done

# ---- Helpers -----------------------------------------------------------------
log()  { echo "[deploy] $*"; }
step() { echo ""; echo "==> $*"; }
ts()   { date '+%H:%M:%S'; }

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

if ! git remote get-url server &>/dev/null; then
    echo "ERROR: git remote 'server' not found."
    echo "       Run: git remote add server rk10:RepoSync"
    exit 1
fi

CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$CURRENT_BRANCH" != "$BRANCH" ]]; then
    log "Warning: current branch is '$CURRENT_BRANCH', deploying '$BRANCH'"
fi

if [[ -n "$(git status --porcelain)" ]]; then
    log "Warning: working tree has uncommitted changes (they will NOT be deployed)"
fi

# Ensure ControlMaster socket directory exists (SSH won't create it)
mkdir -p ~/.ssh/cm
chmod 700 ~/.ssh/cm

# Verify SSH connectivity (skip in dry-run)
if [[ "$DRY_RUN" == "true" ]]; then
    log "Skipping SSH connectivity check (dry-run)"
else
    log "Testing SSH connectivity to $SSH_HOST..."
    if ! ssh -o BatchMode=yes -o ConnectTimeout=10 "$SSH_HOST" true 2>/dev/null; then
        echo "ERROR: Cannot connect to $SSH_HOST. Check SSH config and VPN."
        exit 1
    fi
    log "SSH OK"
fi

# ---- Phase 1: Push code ------------------------------------------------------
step "Phase 1: Push code to server ($(ts))"
run git push server "$BRANCH":"$BRANCH"

# ---- Phase 2: Build web UI locally and sync to server ------------------------
# Build locally (where Node.js is available), rsync the compiled dist/ to a
# staging area on the server, then the remote script sudo-copies it into place.
if [[ "$BUILD_WEB_UI" == "true" ]]; then
    step "Phase 2: Build web UI locally ($(ts))"
    log "Running npm build in web-ui/..."
    run bash -c "cd '$REPO_ROOT/web-ui' && npm install && npm run build"

    log "Syncing web-ui/dist/ to server staging area..."
    run rsync -az --delete \
        -e "ssh" \
        "$REPO_ROOT/web-ui/dist/" \
        "$SSH_HOST:~/reposync-web-ui-dist/"
    log "Web UI synced to ~/reposync-web-ui-dist/ on server"
else
    step "Phase 2: Skipping web UI build (--no-web-ui)"
fi

# ---- Phase 3: Remote build, install, and restart (single SSH session) --------
step "Phase 3: Remote build + install + restart ($(ts))"
log "Single SSH session to $SSH_HOST for all remote operations..."

# All remote work in one heredoc — one TCP connection (reused via ControlMaster
# if the push in Phase 1 already opened a socket), one handshake.
#
# 'source ~/.cargo/env' is required: non-interactive SSH sessions don't run
# ~/.bashrc, so rustup's PATH additions are missing by default.

INSTALL_WEB_UI_CMD=""
if [[ "$BUILD_WEB_UI" == "true" && "$DRY_RUN" != "true" ]]; then
    INSTALL_WEB_UI_CMD="
echo '[remote] Installing web UI static files...'
sudo mkdir -p ${REMOTE_STATIC_DIR}
sudo rsync -a --delete ~/reposync-web-ui-dist/ ${REMOTE_STATIC_DIR}/
echo '[remote] Web UI installed to ${REMOTE_STATIC_DIR}/'
"
fi

REMOTE_SCRIPT="
set -euo pipefail

cd ${REMOTE_REPO_DIR}

echo '[remote] Checking out branch: ${BRANCH}'
git fetch origin ${BRANCH}
git checkout ${BRANCH}
git reset --hard origin/${BRANCH}

# Ensure Rust is in PATH for non-interactive SSH sessions
if [ -f \"\$HOME/.cargo/env\" ]; then
    # shellcheck source=/dev/null
    source \"\$HOME/.cargo/env\"
fi

RUST_VERSION=\$(rustc --version 2>/dev/null || echo 'not found')
echo '[remote] Rust: '\$RUST_VERSION

echo '[remote] Building release binaries...'
BUILD_START=\$(date +%s)
cargo build --release --workspace 2>&1
BUILD_END=\$(date +%s)
echo '[remote] Build complete in '\$(( BUILD_END - BUILD_START ))' seconds'

echo '[remote] Installing binaries to /usr/local/bin/...'
sudo install -m 755 target/release/reposync-daemon /usr/local/bin/reposync-daemon
sudo install -m 755 target/release/reposync        /usr/local/bin/reposync

${INSTALL_WEB_UI_CMD}

echo '[remote] Restarting reposync daemon...'
if systemctl is-enabled --quiet reposync 2>/dev/null && systemctl is-active --quiet reposync 2>/dev/null; then
    echo '[remote] Using systemd to restart...'
    sudo systemctl daemon-reload
    sudo systemctl restart reposync
    sleep 1
    if systemctl is-active --quiet reposync; then
        echo '[remote] reposync service is ACTIVE'
    else
        echo '[remote] ERROR: reposync service failed to start'
        sudo journalctl -u reposync -n 30 --no-pager
        exit 1
    fi
else
    echo '[remote] Using restart-daemon.sh (no active systemd service)...'
    REPOSYNC_BIN=${REMOTE_REPO_DIR}/target/release/reposync-daemon \
        bash ${REMOTE_REPO_DIR}/scripts/restart-daemon.sh
fi

echo '[remote] Deployed version:'
/usr/local/bin/reposync --version 2>/dev/null || true
"

if [[ "$DRY_RUN" == "true" ]]; then
    log "[dry-run] Would execute on $SSH_HOST:"
    echo "$REMOTE_SCRIPT"
else
    ssh "$SSH_HOST" bash <<< "$REMOTE_SCRIPT"
fi

# ---- Done --------------------------------------------------------------------
step "Deploy complete ($(ts))"
log "Service live at http://orw-chrisc-rk10.wv.mentorg.com:8080"
