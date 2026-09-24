#!/usr/bin/env bash
# Execute exact reviewed base and candidate under the same locked container runtime.
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
base_sha=87379741779a6259f7eeb52a68cc6f061174e5ef
previous_sha=f74fce855a1f1d80dd631397f436ba33906272e6
base_tree="$(git rev-parse "$base_sha^{tree}")"
previous_tree="$(git rev-parse "$previous_sha^{tree}")"
candidate_sha="$(git rev-parse HEAD)"
candidate_tree="$(git rev-parse HEAD^{tree})"
comparison_dir="$repo_root/artifacts/reliability-compare/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$comparison_dir/base" "$comparison_dir/previous" "$comparison_dir/candidate"
base_context="$(mktemp -d "${TMPDIR:-/tmp}/reposync-reviewed-base.XXXXXX")"
previous_context="$(mktemp -d "${TMPDIR:-/tmp}/reposync-previous-head.XXXXXX")"
trap 'rm -rf "$base_context" "$previous_context"' EXIT
git archive "$base_sha" | tar -x -C "$base_context"
git archive "$previous_sha" | tar -x -C "$previous_context"

# This overlay changes no base runtime source. The old test fixture needs an
# explicit synthetic SVN author when running as the container's numeric user.
python3 - "$base_context/crates/core/tests/team_mode_e2e.rs" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
source = p.read_text()
needle = '            message,\n            wc_path.to_str().unwrap(),\n            "--non-interactive",'
assert source.count(needle) == 1, 'reviewed-base fixture shape changed'
p.write_text(source.replace(needle,
    '            message,\n            wc_path.to_str().unwrap(),\n            "--username",\n            "fixture",\n            "--non-interactive",'))
PY
mkdir -p "$base_context/docs/reliability/fixtures"
cp Dockerfile.reliability .dockerignore "$base_context/"
cp scripts/reliability-prep.py scripts/reliability-runtime.py scripts/reliability_scan.py scripts/reliability-inventory.py scripts/reliability-inventory-probes.py "$base_context/scripts/"
cp docs/reliability/fixtures/Cargo.lock "$base_context/docs/reliability/fixtures/"
cp docs/reliability/required-cases.json "$base_context/docs/reliability/"
shasum -a 256 "$base_context/crates/core/tests/team_mode_e2e.rs" | awk '{print $1}' > "$comparison_dir/base-test-overlay.sha256"
cp Dockerfile.reliability .dockerignore "$previous_context/"
cp scripts/reliability-prep.py scripts/reliability-runtime.py scripts/reliability_scan.py scripts/reliability-inventory.py scripts/reliability-inventory-probes.py "$previous_context/scripts/"
cp docs/reliability/fixtures/Cargo.lock "$previous_context/docs/reliability/fixtures/"
cp docs/reliability/required-cases.json "$previous_context/docs/reliability/"

REPOSYNC_BUILD_CONTEXT="$base_context" \
REPOSYNC_SOURCE_HEAD_OVERRIDE="$base_sha" \
REPOSYNC_SOURCE_TREE_OVERRIDE="$base_tree" \
REPOSYNC_ARTIFACT_DIR="$comparison_dir/base" \
  scripts/reliability-container.sh --baseline
REPOSYNC_BUILD_CONTEXT="$previous_context" \
REPOSYNC_SOURCE_HEAD_OVERRIDE="$previous_sha" \
REPOSYNC_SOURCE_TREE_OVERRIDE="$previous_tree" \
REPOSYNC_ARTIFACT_DIR="$comparison_dir/previous" \
  scripts/reliability-container.sh --baseline
REPOSYNC_SOURCE_HEAD_OVERRIDE="$candidate_sha" \
REPOSYNC_SOURCE_TREE_OVERRIDE="$candidate_tree" \
REPOSYNC_ARTIFACT_DIR="$comparison_dir/candidate" \
  scripts/reliability-container.sh --baseline
python3 scripts/reliability-compare.py "$comparison_dir/base/baseline-results.json" \
  "$comparison_dir/candidate/baseline-results.json" "$comparison_dir/comparison.json"
python3 scripts/reliability-compare.py "$comparison_dir/previous/baseline-results.json" \
  "$comparison_dir/candidate/baseline-results.json" "$comparison_dir/previous-comparison.json"
echo "Matched comparison artifact: $comparison_dir"
