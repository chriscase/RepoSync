#!/usr/bin/env bash
# Build outside the guard; run only compiled tests in a fixture-only container.
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
mode="${1:---diagnostics}"
case "$mode" in
  --diagnostics|--candidate|--all|--baseline) ;;
  *) echo "Usage: $0 [--diagnostics|--candidate|--all|--baseline]" >&2; exit 2 ;;
esac
artifact_dir="${REPOSYNC_ARTIFACT_DIR:-$repo_root/artifacts/reliability-phase0/$(date -u +%Y%m%dT%H%M%SZ)-${mode#--}}"
mkdir -p "$artifact_dir"
chmod 1777 "$artifact_dir"
phase=preflight
prepared_context=
host_private=
listener_pid=
finish() {
  original_status=$?
  trap - EXIT
  set +e
  printf '{"phase":"%s","exit_code":%d}\n' "$phase" "$original_status" > "$artifact_dir/driver-exit.json"
  if [[ "$mode" == --all && "$original_status" -ne 0 ]]; then
    # Include archive/staging failures: the unchanged workflow would otherwise
    # skip comparisons after this command's failure. Never change its exit code.
    scripts/reliability-compare.sh
    comparison_status=$?
    printf '%s\n' "$comparison_status" > "$artifact_dir/comparison-exit-code.txt"
    echo "Diagnostic driver exit: $original_status; comparison exit: $comparison_status"
  fi
  [[ -z "$listener_pid" ]] || kill "$listener_pid" 2>/dev/null
  [[ -z "$host_private" ]] || rm -rf "$host_private"
  [[ -z "$prepared_context" ]] || rm -rf "$prepared_context"
  exit "$original_status"
}
trap finish EXIT
command -v docker >/dev/null || { echo "NOT RUN: Docker unavailable" >&2; exit 2; }
docker info >/dev/null || { echo "NOT RUN: Docker daemon unavailable" >&2; exit 2; }
source_head="${REPOSYNC_SOURCE_HEAD_OVERRIDE:-$(git rev-parse HEAD)}"
source_tree="${REPOSYNC_SOURCE_TREE_OVERRIDE:-$(git rev-parse HEAD^{tree})}"
context_root="${REPOSYNC_BUILD_CONTEXT:-$repo_root}"
prepared_context="$(mktemp -d "${TMPDIR:-/tmp}/reposync-build-context.XXXXXX")"
phase=context_copy
cp "$context_root/Cargo.toml" "$context_root/Dockerfile.reliability" "$context_root/.dockerignore" "$prepared_context/"
cp -R "$context_root/crates" "$context_root/docs" "$context_root/scripts" "$context_root/web-ui" "$prepared_context/"
if [[ -n "${REPOSYNC_BUILD_CONTEXT:-}" && -z "${REPOSYNC_LOCK_SOURCE:-}" ]]; then
  echo "FAIL: archived build context requires an explicit pinned lock source" >&2; exit 1
fi
lock_source="${REPOSYNC_LOCK_SOURCE:-candidate}"
phase=lock_staging
python3 "$repo_root/scripts/reliability-lock-overlay.py" stage "$prepared_context" \
  --source "$lock_source" --receipt "$artifact_dir/build-lock.json"
cp "$prepared_context/Cargo.lock" "$prepared_context/docs/reliability/fixtures/Cargo.lock"
mkdir "$prepared_context/legacy-old"
phase=old_source_archive
git archive 87379741779a6259f7eeb52a68cc6f061174e5ef | tar -x -C "$prepared_context/legacy-old"
python3 "$repo_root/scripts/reliability-lock-overlay.py" stage "$prepared_context/legacy-old" \
  --source 87379741779a6259f7eeb52a68cc6f061174e5ef \
  --receipt "$artifact_dir/old-generator-build-lock.json"
cp "$repo_root/scripts/legacy_import_generator.rs" "$prepared_context/legacy-old/crates/web/tests/legacy_import_generator.rs"
phase=build_identity
goal_hash="$(shasum -a 256 docs/reliability/GOAL.md | awk '{print $1}')"
lock_hash="$(shasum -a 256 "$prepared_context/Cargo.lock" | awk '{print $1}')"
[[ "$goal_hash" == 16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8 ]] || {
  echo "FAIL: original GOAL.md changed" >&2; exit 1;
}
image="reposync-reliability:$source_head"
include_old=1
[[ "$mode" == --baseline ]] && include_old=0
recipe_hash="$(shasum -a 256 "$prepared_context/Dockerfile.reliability" | awk '{print $1}')"
old_lock_hash="$(shasum -a 256 "$prepared_context/legacy-old/Cargo.lock" | awk '{print $1}')"
adapter_hash="$(shasum -a 256 "$repo_root/scripts/legacy_import_generator.rs" | awk '{print $1}')"
context_hash="$(python3 - "$prepared_context" <<'PY'
import hashlib, os, sys
from pathlib import Path
root = Path(sys.argv[1])
digest = hashlib.sha256()
for path in sorted(root.rglob("*")):
    if path.is_symlink():
        data = os.readlink(path).encode()
    elif path.is_file():
        data = path.read_bytes()
    else:
        continue
    digest.update(path.relative_to(root).as_posix().encode() + b"\0")
    digest.update(hashlib.sha256(data).digest())
print(digest.hexdigest())
PY
)"
target_cache="$(printf '%s\n' "$source_head" "$source_tree" "$context_hash" "$lock_hash" "$recipe_hash" "$include_old" | shasum -a 256 | awk '{print $1}')"
old_target_cache="$(printf '%s\n' 87379741779a6259f7eeb52a68cc6f061174e5ef "$old_lock_hash" "$adapter_hash" "$recipe_hash" | shasum -a 256 | awk '{print $1}')"
python3 - "$artifact_dir/build-identity.json" "$source_head" "$source_tree" "$lock_hash" "$target_cache" "$old_lock_hash" "$old_target_cache" "$recipe_hash" "$adapter_hash" "$context_hash" <<'PY'
import json, sys
from pathlib import Path
identity = dict(zip(
    ("source_head", "source_tree", "lock_sha256", "target_cache", "old_lock_sha256", "old_target_cache", "dockerfile_sha256", "adapter_sha256", "prepared_context_sha256"),
    sys.argv[2:]))
identity.update(toolchain_image="rust:1.98.1-bookworm",
                profile={"incremental": False, "dev_debug_symbols": 0, "test_debug_symbols": 0})
Path(sys.argv[1]).write_text(json.dumps(identity, indent=2) + "\n")
PY
build_status=0
phase=build
docker build --build-arg "RELIABILITY_INCLUDE_OLD=$include_old" \
  --build-arg "RELIABILITY_TARGET_CACHE=$target_cache" \
  --build-arg "RELIABILITY_OLD_TARGET_CACHE=$old_target_cache" \
  --file "$prepared_context/Dockerfile.reliability" --tag "$image" "$prepared_context" || build_status=$?
printf '%s\n' "$build_status" > "$artifact_dir/build-exit-code.txt"
if [[ "$build_status" -ne 0 ]]; then
  exit "$build_status"
fi
phase=image_identity
docker image inspect "$image" --format '{{.Id}}' > "$artifact_dir/image-id.txt"
phase=host_canaries
host_private="$(mktemp -d "${TMPDIR:-/tmp}/reposync-host-private.XXXXXX")"
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
for _ in {1..50}; do [[ -s "$listener_info" ]] && break; sleep 0.1; done
[[ -s "$listener_info" ]] || { echo "FAIL: host canary listener unavailable" >&2; exit 1; }

runtime_status=0
phase=runtime
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
  "$image" "${mode#--}" || runtime_status=$?
printf '%s\n' "$runtime_status" > "$artifact_dir/runtime-exit-code.txt"
[[ "$(cat "$host_private/canary")" == 'synthetic host-private file' ]] || {
  echo "FAIL: host-private canary changed" >&2; exit 1;
}
command -v python3 >/dev/null || { echo "FAIL: evidence scanner interpreter unavailable" >&2; exit 1; }
[[ -f scripts/reliability_scan.py ]] || { echo "FAIL: evidence scanner unavailable" >&2; exit 1; }
scan_status=0
phase=evidence_scan
python3 scripts/reliability_scan.py --scan "$artifact_dir" \
  --status-file "$artifact_dir/scan-status.json" || scan_status=$?
[[ "$runtime_status" -eq 0 ]] || exit "$runtime_status"
[[ "$scan_status" -eq 0 ]] || exit "$scan_status"
phase=complete
echo "Artifact: $artifact_dir"
