#!/usr/bin/env bash
# Build outside the guard; run only compiled tests in a fixture-only container.
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
mode="${1:---diagnostics}"
case "$mode" in
  --diagnostics|--candidate|--all) ;;
  *) echo "Usage: $0 [--diagnostics|--candidate|--all]" >&2; exit 2 ;;
esac
command -v docker >/dev/null || { echo "NOT RUN: Docker unavailable" >&2; exit 2; }
docker info >/dev/null || { echo "NOT RUN: Docker daemon unavailable" >&2; exit 2; }
source_head="$(git rev-parse HEAD)"
source_tree="$(git rev-parse HEAD^{tree})"
goal_hash="$(shasum -a 256 docs/reliability/GOAL.md | awk '{print $1}')"
lock_hash="$(shasum -a 256 docs/reliability/fixtures/Cargo.lock | awk '{print $1}')"
[[ "$goal_hash" == 16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8 ]] || {
  echo "FAIL: original GOAL.md changed" >&2; exit 1;
}
image="reposync-reliability:$source_head"
docker build --file Dockerfile.reliability --tag "$image" .
artifact_dir="$repo_root/artifacts/reliability-phase0/$(date -u +%Y%m%dT%H%M%SZ)-${mode#--}"
mkdir -p "$artifact_dir"
chmod 1777 "$artifact_dir"
host_private="$(mktemp -d "${TMPDIR:-/tmp}/reposync-host-private.XXXXXX")"
trap 'rm -rf "$host_private"' EXIT
printf 'synthetic host-private file\n' > "$host_private/canary"

# A real listener in the host namespace must remain unreachable inside the
# container's separate --network none namespace.
listener_info="$host_private/listener-port"
python3 - "$listener_info" <<'PY' &
import socket, sys, time
s = socket.socket()
s.bind(("127.0.0.1", 0))
s.listen(1)
with open(sys.argv[1], "w") as output:
    output.write(str(s.getsockname()[1]))
time.sleep(300)
PY
listener_pid=$!
trap 'kill "$listener_pid" 2>/dev/null || true; rm -rf "$host_private"' EXIT
for _ in {1..50}; do [[ -s "$listener_info" ]] && break; sleep 0.1; done
[[ -s "$listener_info" ]] || { echo "FAIL: host canary listener unavailable" >&2; exit 1; }

docker run --rm --network none --read-only --cap-drop ALL \
  --security-opt no-new-privileges --pids-limit 256 \
  --tmpfs /fixture:rw,nosuid,nodev,size=2g,mode=1777 \
  --tmpfs /tmp:rw,nosuid,nodev,size=64m,mode=1777 \
  --mount "type=bind,src=$artifact_dir,dst=/evidence,readonly=false" \
  --workdir /fixture \
  --env HOME=/fixture/home --env TMPDIR=/fixture/tmp \
  --env REPOSYNC_FIXTURE_ROOT=/fixture/tmp \
  --env XDG_CONFIG_HOME=/fixture/config --env GIT_CONFIG_NOSYSTEM=1 \
  --env GIT_CONFIG_GLOBAL=/dev/null --env GIT_TERMINAL_PROMPT=0 \
  --env GIT_ASKPASS=/usr/bin/false --env GIT_SSH_COMMAND=/usr/bin/false \
  --env SVN_SSH=/usr/bin/false --env REPOSYNC_TEST_SVN_PW=REPOSYNC_SYNTHETIC_SECRET_CANARY_72 \
  --env REPOSYNC_TEST_GH_TOKEN=REPOSYNC_SYNTHETIC_SECRET_CANARY_72 \
  --env REPOSYNC_HOST_CANARY_PATH="$host_private/canary" \
  --env REPOSYNC_HOST_LISTENER_PORT="$(cat "$listener_info")" \
  --env REPOSYNC_SOURCE_HEAD="$source_head" --env REPOSYNC_SOURCE_TREE="$source_tree" \
  --env REPOSYNC_GOAL_SHA256="$goal_hash" --env REPOSYNC_LOCK_SHA256="$lock_hash" \
  "$image" "${mode#--}"
[[ "$(cat "$host_private/canary")" == 'synthetic host-private file' ]] || {
  echo "FAIL: host-private canary changed" >&2; exit 1;
}
if rg -l 'REPOSYNC_SYNTHETIC_SECRET_CANARY_72' "$artifact_dir"; then
  echo "FAIL: synthetic secret canary appeared in exported evidence" >&2; exit 1
fi
echo "Artifact: $artifact_dir"
