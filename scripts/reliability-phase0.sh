#!/usr/bin/env bash
# Phase 0 actual-engine diagnostics against disposable file:// SVN and bare Git.
# Build dependencies before entering the test process's loopback-only sandbox.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
mode="${1:---diagnostics}"
case "$mode" in
    --diagnostics) filter=diagnostic_; selection=(--workspace) ;;
    --baseline) filter=; selection=(--workspace) ;;
    --baseline-personal) filter=; selection=(-p reposync-personal) ;;
    --baseline-web) filter=; selection=(-p reposync-web) ;;
    *) echo "Usage: $0 [--diagnostics|--baseline|--baseline-personal|--baseline-web]" >&2; exit 2 ;;
esac

for tool in cargo rustc svn svnadmin svnlook git sandbox-exec python3; do
    command -v "$tool" >/dev/null || { echo "NOT RUN: missing $tool" >&2; exit 2; }
done
if [[ "$(uname -s)" != Darwin ]]; then
    echo "NOT RUN: this entry point currently requires macOS sandbox-exec" >&2
    exit 2
fi

artifact_dir="$repo_root/artifacts/reliability-phase0/$(date -u +%Y%m%dT%H%M%SZ)-${mode#--}"
mkdir -p "$artifact_dir"
{ cargo --version; rustc --version; svn --version --quiet; git --version; } > "$artifact_dir/tool-versions.txt"

private_root="$(mktemp -d "${TMPDIR:-/tmp}/reposync-phase0.XXXXXX")"
trap 'rm -rf "$private_root"' EXIT
mkdir -p "$private_root/home" "$private_root/config" "$private_root/tmp"
echo "Preparing Rust dependencies and test binaries outside the network sandbox"
cargo test "${selection[@]}" --no-run --message-format=json > "$private_root/preparation.jsonl"
python3 - "$private_root/preparation.jsonl" "$private_root/test-binaries.txt" <<'PY'
import json, sys
from pathlib import Path
binaries = []
for line in Path(sys.argv[1]).read_text().splitlines():
    try: item = json.loads(line)
    except json.JSONDecodeError: continue
    if item.get('reason') == 'compiler-artifact' and item.get('profile', {}).get('test') and item.get('executable'):
        binaries.append(item['executable'])
if not binaries:
    raise SystemExit('NOT RUN: preparation produced zero test binaries')
Path(sys.argv[2]).write_text('\n'.join(dict.fromkeys(binaries)) + '\n')
print(f'Prepared {len(binaries)} test binaries')
PY
profile='(version 1) (allow default) (deny network-outbound) (allow network-outbound (remote ip "localhost:*"))'

# These probes execute under the exact policy used for the engine. The denied
# target is a documentation-only IP; a successful connection is a hard failure.
sandbox-exec -p "$profile" /usr/bin/python3 -c \
  'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); s.listen(); c=socket.socket(); c.connect(s.getsockname()); print("loopback probe: PASS")'
sandbox-exec -p "$profile" /usr/bin/python3 -c \
  'import socket; s=socket.socket(); s.settimeout(1);
try: s.connect(("192.0.2.1",80))
except PermissionError: print("external target rejection: PASS")
else: raise SystemExit("external target was not rejected")'

echo "Running $mode with private HOME, no ambient credentials, and denied non-loopback egress"
test_exit=0
test_args=(--nocapture --test-threads=1)
if [[ -n "$filter" ]]; then test_args=("$filter" "${test_args[@]}"); fi
: > "$artifact_dir/test-output.log"
while IFS= read -r binary; do
  echo "Running $(basename "$binary")" | tee -a "$artifact_dir/test-output.log"
  set +e
  env -i \
    PATH="$PATH" \
    HOME="$private_root/home" \
    TMPDIR="$private_root/tmp" \
    XDG_CONFIG_HOME="$private_root/config" \
    GIT_CONFIG_NOSYSTEM=1 \
    GIT_CONFIG_GLOBAL=/dev/null \
    GIT_TERMINAL_PROMPT=0 \
    GIT_ASKPASS=/usr/bin/false \
    GIT_SSH_COMMAND=/usr/bin/false \
    SVN_SSH=/usr/bin/false \
    REPOSYNC_TEST_SVN_PW=fixture-only \
    REPOSYNC_TEST_GH_TOKEN=fixture-only \
    sandbox-exec -p "$profile" "$binary" "${test_args[@]}" \
    2>&1 | tee -a "$artifact_dir/test-output.log"
  binary_exit="${PIPESTATUS[0]}"
  set -e
  if [[ "$binary_exit" -ne 0 ]]; then test_exit="$binary_exit"; fi
done < "$private_root/test-binaries.txt"
python3 - "$artifact_dir/test-output.log" "$artifact_dir/summary.json" "$mode" "$test_exit" "$(git rev-parse HEAD)" <<'PY'
import json, re, sys
from pathlib import Path
log = Path(sys.argv[1]).read_text()
rows = [tuple(map(int, m)) for m in re.findall(r'test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out', log)]
data = {
    'mode': sys.argv[3], 'source_head': sys.argv[5], 'exit_code': int(sys.argv[4]),
    'binaries': len(rows), 'passed': sum(r[0] for r in rows),
    'failed': sum(r[1] for r in rows), 'ignored': sum(r[2] for r in rows),
    'filtered_out': sum(r[4] for r in rows),
    'baseline_observations': re.findall(r'(?:EXPECTED BASELINE FAILURE|BASELINE OBSERVATION) R\d+(?:/R\d+)?:[^\n]+', log),
}
Path(sys.argv[2]).write_text(json.dumps(data, indent=2) + '\n')
print(json.dumps(data, indent=2))
PY
echo "Artifact: $artifact_dir"
if [[ "$test_exit" -ne 0 ]]; then exit "$test_exit"; fi
if ! grep -Eq '"passed": [1-9]' "$artifact_dir/summary.json"; then
    echo "FAIL: zero tests passed" >&2
    exit 1
fi
