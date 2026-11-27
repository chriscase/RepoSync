#!/usr/bin/env bash
# Tests for release_channel.sh and for the publisher guard in release.yml.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
classify="$root/scripts/release_channel.sh"
workflow="$root/.github/workflows/release.yml"
failures=0

fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }

expect() { # <tag> <channel> <prerelease> <make_latest>
  local expected actual
  expected="$(printf 'channel=%s\nprerelease=%s\nmake_latest=%s' "$2" "$3" "$4")"
  actual="$(bash "$classify" "$1" 2>/dev/null)" || { fail "$1 was refused"; return; }
  [ "$actual" = "$expected" ] || fail "$1 classified as: $actual"
}

refuse() { # <tag>
  local output
  if output="$(bash "$classify" "$1" 2>/dev/null)"; then
    fail "'$1' was accepted"
  elif [ -n "$output" ]; then
    fail "'$1' was refused but still printed outputs"
  fi
}

expect v0.1.0 stable false true
expect v1.2.3 stable false true
expect v10.20.30 stable false true
expect v0.1.0-alpha.1 prerelease true false
expect v0.1.0-alpha.0 prerelease true false
expect v1.0.0-beta.12 prerelease true false
expect v2.3.4-rc.2 prerelease true false

for tag in '' v 1.2.3 v1.2 v1 v1.2.3.4 v01.2.3 v1.2.3-alpha v1.2.3-alpha.01 \
    v1.2.3-alpha1 v1.2.3-preview.1 v1.2.3-ALPHA.1 v1.2.3-alpha.1.2 \
    v1.2.3+build v1.2.3-alpha.1+build vlatest latest 'v1.2.3 ' ' v1.2.3' \
    $'v1.2.3\nv1.2.3-alpha.1' $'v1.2.3-alpha.1\nv1.2.3' 'v1.2.3-rc.1;true'; do
  refuse "$tag"
done

# The workflow must route every publication through the classifier.
python3 - "$workflow" <<'PY' || fail "release.yml publisher guard"
import re
import sys

text = open(sys.argv[1]).read()

def job(name):
    match = re.search(rf"(?ms)^  {name}:\n(.*?)(?=^  [A-Za-z_-]+:\n|\Z)", text)
    assert match, f"missing job {name}"
    return match.group(1)

channel, build, release, docker = (job(n) for n in ("channel", "build", "release", "docker"))
assert "scripts/release_channel.sh" in channel
# The classifier's exit status must reach the step: no pipe may follow it.
calls = [line for line in channel.splitlines() if "release_channel.sh" in line and "test_" not in line]
assert len(calls) == 1 and "|" not in calls[0], calls
# No job publishes unless the classifier named a channel.
gate = "if: needs.channel.outputs.channel == 'stable' || needs.channel.outputs.channel == 'prerelease'"
assert all(gate in body for body in (build, release, docker))
assert "contents: read" in channel
# Nothing is built or published for a tag the classifier refused.
assert re.search(r"needs:\s*channel\b", build)
for body in (release, docker):
    assert re.search(r"needs:\s*\[[^\]]*\bchannel\b[^\]]*\]", body)
assert "prerelease: ${{ needs.channel.outputs.prerelease }}" in release
assert "make_latest: ${{ needs.channel.outputs.make_latest }}" in release
# `latest` is named exactly once, on a line taken only for a stable tag.
latest = [line for line in text.splitlines() if "reposync:latest" in line]
assert len(latest) == 1, latest
assert 'if [ "$CHANNEL" = "stable" ]' in docker
position = docker.index('if [ "$CHANNEL" = "stable" ]')
assert position < docker.index("reposync:latest") < docker.index("\n          fi", position)
assert "CHANNEL: ${{ needs.channel.outputs.channel }}" in docker
assert "tags: ${{ steps.tags.outputs.tags }}" in docker
PY

# Run the workflow's own classify step the way Actions does (`bash -e`): a
# refused tag must fail the step and write no outputs.
if [ -z "${RELEASE_CHANNEL_STEP_TEST:-}" ]; then
  step="$(mktemp)"; outputs="$(mktemp)"
  python3 - "$workflow" > "$step" <<'PY'
import re
import sys

text = open(sys.argv[1]).read()
match = re.search(r"(?ms)^      - name: Classify tag\n.*?^        run: \|\n(.*?)(?=^\S|^  \S|^      - |\Z)", text)
assert match, "missing Classify tag step"
print("\n".join(line[10:] for line in match.group(1).splitlines()))
PY
  run_step() { # <tag>
    : > "$outputs"
    (cd "$root" && RELEASE_CHANNEL_STEP_TEST=1 TAG="$1" GITHUB_OUTPUT="$outputs" bash -e "$step") >/dev/null 2>&1
  }
  for tag in v1.2.3-preview.1 v2.0.0-rc1 v1.2 vlatest; do
    if run_step "$tag"; then fail "classify step accepted '$tag'"; fi
    [ ! -s "$outputs" ] || fail "classify step wrote outputs for '$tag'"
  done
  run_step v1.2.3-alpha.1 || fail "classify step refused v1.2.3-alpha.1"
  [ "$(cat "$outputs")" = "$(printf 'channel=prerelease\nprerelease=true\nmake_latest=false')" ] \
    || fail "classify step outputs for v1.2.3-alpha.1: $(cat "$outputs")"
  run_step v1.2.3 || fail "classify step refused v1.2.3"
  grep -qx 'make_latest=true' "$outputs" || fail "classify step outputs for v1.2.3"
  rm -f "$step" "$outputs"
fi

if [ "$failures" -ne 0 ]; then
  echo "$failures release channel check(s) failed" >&2
  exit 1
fi
echo "release channel checks passed"
