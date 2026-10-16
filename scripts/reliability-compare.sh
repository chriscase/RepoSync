#!/usr/bin/env bash
# Run every reviewed source with its own pinned graph; retain every result.
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
candidate_sha="$(git rev-parse HEAD)"
candidate_tree="$(git rev-parse HEAD^{tree})"
comparison_dir="${REPOSYNC_COMPARISON_DIR:-$repo_root/artifacts/reliability-compare/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$comparison_dir"
contexts="$(mktemp -d "${TMPDIR:-/tmp}/reposync-comparison-contexts.XXXXXX")"
trap 'rm -rf "$contexts"' EXIT
labels=(base previous immediate merged)
sources=(87379741779a6259f7eeb52a68cc6f061174e5ef f74fce855a1f1d80dd631397f436ba33906272e6 ab097580d9415ae2f8df44bed4041f740a1b38f2 54e183484961bd9ad12bd03505b3b00610de7cd9)

for index in "${!sources[@]}"; do
  source="${sources[$index]}"
  label="${labels[$index]}"
  context="$contexts/$label"
  mkdir -p "$context" "$comparison_dir/$label"
  git archive "$source" | tar -x -C "$context"
  python3 scripts/reliability-lock-overlay.py stage "$context" --source "$source" \
    --receipt "$comparison_dir/$label-lock.json"
  # Keep the reviewed synthetic author adapter; no production source changes.
  if [[ "$label" == base ]]; then
    python3 - "$context/crates/core/tests/team_mode_e2e.rs" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
source = p.read_text()
needle = '            message,\n            wc_path.to_str().unwrap(),\n            "--non-interactive",'
assert source.count(needle) == 1, 'reviewed-base fixture shape changed'
p.write_text(source.replace(needle,
    '            message,\n            wc_path.to_str().unwrap(),\n            "--username",\n            "fixture",\n            "--non-interactive",'))
PY
    shasum -a 256 "$context/crates/core/tests/team_mode_e2e.rs" | awk '{print $1}' > "$comparison_dir/base-test-overlay.sha256"
  fi
  cp Dockerfile.reliability .dockerignore "$context/"
  cp scripts/reliability-prep.py scripts/reliability-runtime.py scripts/reliability_scan.py \
    scripts/reliability-inventory.py scripts/reliability-inventory-probes.py \
    scripts/reliability-copy-consumer.mjs scripts/reliability-scoped-consumer.mjs "$context/scripts/"
  mkdir -p "$context/docs/reliability/fixtures"
  cp docs/reliability/required-cases.json docs/reliability/legacy-evidence-vocabulary.json "$context/docs/reliability/"
done

aggregate_status=0
for index in "${!sources[@]}"; do
  source="${sources[$index]}"
  label="${labels[$index]}"
  status=0
  REPOSYNC_BUILD_CONTEXT="$contexts/$label" \
  REPOSYNC_LOCK_SOURCE="$source" \
  REPOSYNC_SOURCE_HEAD_OVERRIDE="$source" \
  REPOSYNC_SOURCE_TREE_OVERRIDE="$(git rev-parse "$source^{tree}")" \
  REPOSYNC_ARTIFACT_DIR="$comparison_dir/$label" \
    scripts/reliability-container.sh --baseline || status=$?
  printf '%s\n' "$status" > "$comparison_dir/$label-exit-code.txt"
  [[ "$status" -eq 0 ]] || aggregate_status=1
done
status=0
REPOSYNC_SOURCE_HEAD_OVERRIDE="$candidate_sha" \
REPOSYNC_SOURCE_TREE_OVERRIDE="$candidate_tree" \
REPOSYNC_ARTIFACT_DIR="$comparison_dir/candidate" \
  scripts/reliability-container.sh --baseline || status=$?
printf '%s\n' "$status" > "$comparison_dir/candidate-exit-code.txt"
[[ "$status" -eq 0 ]] || aggregate_status=1

for index in "${!sources[@]}"; do
  source="${sources[$index]}"
  label="${labels[$index]}"
  report="$comparison_dir/$label-comparison.json"
  status=0
  if [[ -f "$comparison_dir/$label/baseline-results.json" && -f "$comparison_dir/candidate/baseline-results.json" ]]; then
    python3 scripts/reliability-lock-overlay.py compare \
      "$comparison_dir/$label/baseline-results.json" "$comparison_dir/candidate/baseline-results.json" \
      "$report" --source "$source" --candidate-sha "$candidate_sha" || status=$?
  else
    status=2
  fi
  if [[ ! -f "$report" ]]; then
    python3 - "$report" "$source" "$candidate_sha" "$status" <<'PY'
import json, sys
from pathlib import Path
Path(sys.argv[1]).write_text(json.dumps({
    "outcome": "NOT_RUN" if sys.argv[4] == "2" else "FAIL",
    "base_head": sys.argv[2], "candidate_head": sys.argv[3],
    "blocker": "Missing execution report or rejected comparison provenance; inspect per-source build/runtime receipts.",
    "exit_code": int(sys.argv[4]),
}, indent=2) + "\n")
PY
  fi
  [[ "$status" -eq 0 ]] || aggregate_status=1
done
echo "Complete comparison artifact: $comparison_dir; exit: $aggregate_status"
exit "$aggregate_status"
