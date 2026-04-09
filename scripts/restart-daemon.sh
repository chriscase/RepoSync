#!/usr/bin/env bash
# =============================================================================
# restart-daemon.sh - Safely stop and start the RepoSync daemon
# =============================================================================
# Usage:
#   scripts/restart-daemon.sh [--config <path>] [--data-dir <path>]
#
# Stops any running reposync-daemon, waits for clean shutdown, starts a new
# instance with PID file management, and verifies health.
# =============================================================================

set -euo pipefail

# ---- Configuration -----------------------------------------------------------
DAEMON_BIN="${REPOSYNC_BIN:-$(dirname "$(readlink -f "$0")")/../target/release/reposync-daemon}"
CONFIG="${REPOSYNC_CONFIG:-/home/chrisc/gitsvnsync.toml}"
DATA_DIR="${REPOSYNC_DATA_DIR:-/opt/reposync}"
PID_FILE="${REPOSYNC_PID_FILE:-/tmp/reposync-daemon.pid}"
LOG_FILE="${REPOSYNC_LOG:-/tmp/reposync.log}"
HEALTH_URL="http://localhost:8080/api/status/health"
SHUTDOWN_TIMEOUT=10
STARTUP_TIMEOUT=15

# Parse arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --config)  CONFIG="$2"; shift 2 ;;
        --data-dir) DATA_DIR="$2"; shift 2 ;;
        *) echo "Unknown argument: $1"; exit 1 ;;
    esac
done

# ---- Helpers -----------------------------------------------------------------
log() { echo "[restart] $(date '+%H:%M:%S') $*"; }

# ---- Phase 1: Stop existing daemon(s) ---------------------------------------
log "Stopping existing daemon process(es)..."

# Kill by PID file first
if [[ -f "$PID_FILE" ]]; then
    OLD_PID=$(cat "$PID_FILE" 2>/dev/null || true)
    if [[ -n "$OLD_PID" ]] && kill -0 "$OLD_PID" 2>/dev/null; then
        log "Sending SIGTERM to PID $OLD_PID (from PID file)"
        kill "$OLD_PID" 2>/dev/null || true
    fi
    rm -f "$PID_FILE"
fi

# Also kill any stray processes (handles the case where PID file is stale)
STRAY_PIDS=$(pgrep -f 'reposync-daemon' 2>/dev/null || true)
if [[ -n "$STRAY_PIDS" ]]; then
    for pid in $STRAY_PIDS; do
        log "Sending SIGTERM to stray PID $pid"
        kill "$pid" 2>/dev/null || true
    done
fi

# Wait for clean shutdown
WAITED=0
while [[ $WAITED -lt $SHUTDOWN_TIMEOUT ]]; do
    if ! pgrep -f 'reposync-daemon' >/dev/null 2>&1; then
        log "All daemon processes stopped"
        break
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done

# Force kill if still running
if pgrep -f 'reposync-daemon' >/dev/null 2>&1; then
    log "Force-killing remaining processes (SIGKILL)"
    pkill -9 -f 'reposync-daemon' 2>/dev/null || true
    sleep 1
fi

# ---- Phase 2: Start new daemon ----------------------------------------------
if [[ ! -x "$DAEMON_BIN" ]]; then
    echo "ERROR: daemon binary not found or not executable: $DAEMON_BIN"
    exit 1
fi

log "Starting daemon: $DAEMON_BIN --config $CONFIG"
cd "$DATA_DIR"
nohup "$DAEMON_BIN" --config "$CONFIG" >> "$LOG_FILE" 2>&1 &
NEW_PID=$!
echo "$NEW_PID" > "$PID_FILE"
log "Daemon started with PID $NEW_PID (PID file: $PID_FILE)"

# ---- Phase 3: Verify health -------------------------------------------------
log "Waiting for health check..."
WAITED=0
while [[ $WAITED -lt $STARTUP_TIMEOUT ]]; do
    if curl -sf "$HEALTH_URL" >/dev/null 2>&1; then
        VERSION=$(curl -sf "$HEALTH_URL" 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin).get('version','?'))" 2>/dev/null || echo "?")
        log "Health check passed (v$VERSION) - daemon is ready"
        exit 0
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done

# Health check failed
log "WARNING: Health check did not pass within ${STARTUP_TIMEOUT}s"
log "Check logs: tail -50 $LOG_FILE"
if kill -0 "$NEW_PID" 2>/dev/null; then
    log "Process $NEW_PID is still running — may still be initializing"
    exit 0
else
    log "ERROR: Process $NEW_PID is not running!"
    exit 1
fi
