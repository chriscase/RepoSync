#!/usr/bin/env bash
# Drive the shipped launcher functions with recording stubs.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
RECORD="$(mktemp)"
export ACCEPTANCE_LAB_SOURCE_ONLY=1
# shellcheck disable=SC1091
source "$ROOT/scripts/acceptance-lab.sh"

require_docker() { return 0; }
log() { printf '%s\n' "$*" >> "$RECORD"; }
compose() { printf 'DESTRUCTIVE %s\n' "$*" >> "$RECORD"; return 0; }
PROJECT=rsaccdeadbeef
RUN_ID=rsdeadbeef
RUN_DIR="$(mktemp -d)"
mkdir -p "$RUN_DIR"
printf '{}\n' > "$RUN_DIR/run.json"

fail() { printf 'fail-closed: %s\n' "$*" >&2; exit 1; }

reset_record() { : > "$RECORD"; }

docker() { return 42; }
reset_record
set +e
cleanup_run
ec=$?
set -e
[ "$ec" -ne 0 ] || fail "pre-cleanup query failure returned success"
grep -q DESTRUCTIVE "$RECORD" && fail "pre-cleanup query failure issued a destructive command"

docker() {
  if [[ "$*" == *inspect* ]]; then return 42; fi
  if [[ "$*" == *ps* ]]; then printf 'cid123\n'; return 0; fi
  return 0
}
reset_record
set +e
cleanup_run
ec=$?
set -e
[ "$ec" -ne 0 ] || fail "inspection failure returned success"
grep -q DESTRUCTIVE "$RECORD" && fail "inspection failure issued a destructive command"

docker() {
  if [[ "$*" == *inspect* ]]; then printf 'foreign-run\n'; return 0; fi
  if [[ "$*" == *ps* ]]; then printf 'cid123\n'; return 0; fi
  printf '\n'
  return 0
}
reset_record
set +e
cleanup_run
ec=$?
set -e
[ "$ec" -ne 0 ] || fail "foreign label returned success"
grep -q DESTRUCTIVE "$RECORD" && fail "foreign label issued a destructive command"

docker() {
  if [[ "$*" == *"acceptance-lab.run-id=${RUN_ID}"* ]]; then return 42; fi
  printf '\n'
  return 0
}
reset_record
set +e
cleanup_run
ec=$?
set -e
[ "$ec" -ne 0 ] || fail "post-cleanup query failure returned success"
grep -q "cleaned run" "$RECORD" && fail "post-cleanup query failure wrote a cleanup receipt"
grep -q DESTRUCTIVE "$RECORD" || fail "post-cleanup case did not reach removal after a verified listing"

docker() { return 42; }
reset_record
set +e
( interrupt_cleanup TERM )
ec=$?
set -e
[ "$ec" -eq 130 ] || fail "interruption exited $ec"
grep -q DESTRUCTIVE "$RECORD" && fail "interruption issued a destructive command after a failed listing"

build_one() { return 42; }
docker() { return 0; }
record_images() { printf 'RECORDED\n' >> "$RECORD"; return 0; }
extract_build_graph() { return 0; }
load_pins() { return 0; }
ACCEPTANCE_LAB_PREPARE_SCOPE=fixtures
SVN_IMAGE=svn
SVNSERVE_IMAGE=svnserve
GITEA_LAB_IMAGE=gitea-lab
GITHTTP_IMAGE=githttp
PYENV_IMAGE=pyenv
OBSERVER_IMAGE=observer
FORWARDER_IMAGE=forwarder
LDAP_IMAGE=ldap
DEBIAN_IMAGE=debian
GITEA_IMAGE=gitea
reset_record
set +e
prepare_images
ec=$?
set -e
[ "$ec" -ne 0 ] || fail "prepare accepted a failed build while cached tags exist"
grep -q RECORDED "$RECORD" && fail "prepare recorded images after a failed build"

# Full prepare scope reaches record_rust_image_identity. Do not stub that
# function: fixtures scope never calls it, and a cached image would still
# inspect and run after a failed pull.
unset EVIDENCE
ARTIFACT_ROOT="$(mktemp -d)"
CANDIDATE="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
RUST_IMAGE="rust:cached-lab"
RUST_TOOLCHAIN="1.98.1"
LDAP_IMAGE="ldap:cached-lab"
DEBIAN_IMAGE="debian:cached-lab"
NODE_IMAGE="node:cached-lab"
GITEA_IMAGE="gitea:cached-lab"
SVN_IMAGE="svn:cached"
SVNSERVE_IMAGE="svnserve:cached"
GITEA_LAB_IMAGE="gitea-lab:cached"
GITHTTP_IMAGE="githttp:cached"
PYENV_IMAGE="pyenv:cached"
OBSERVER_IMAGE="observer:cached"
FORWARDER_IMAGE="forwarder:cached"
REPOSYNC_IMAGE="reposync:cached"
ACCEPTANCE_LAB_PREPARE_SCOPE=all
unset ACCEPTANCE_LAB_SKIP_REPOSYNC_BUILD || true
build_one() { printf 'BUILD %s\n' "$1" >> "$RECORD"; return 0; }
extract_build_graph() { printf 'EXTRACT\n' >> "$RECORD"; return 0; }
record_images() { printf 'RECORDED\n' >> "$RECORD"; return 0; }
load_pins() { return 0; }
docker() {
  if [ "$1" = "pull" ] && [ "${2:-}" = "$RUST_IMAGE" ]; then
    return 42
  fi
  if [ "$1" = "image" ] && [ "${2:-}" = "inspect" ]; then
    printf 'sha256:cachedimage\n'
    return 0
  fi
  if [ "$1" = "run" ]; then
    local tool="" prev="" arg
    for arg in "$@"; do
      if [ "$arg" = "--version" ]; then
        tool="$prev"
      fi
      prev="$arg"
    done
    if [ "$tool" = "rustc" ]; then
      printf 'rustc %s (cached)\n' "$RUST_TOOLCHAIN"
      return 0
    fi
    if [ "$tool" = "cargo" ]; then
      printf 'cargo %s (cached)\n' "$RUST_TOOLCHAIN"
      return 0
    fi
    return 0
  fi
  return 0
}
reset_record
set +e
if ! prepare_images; then
  ec=1
else
  ec=0
fi
set -e
[ "$ec" -ne 0 ] || fail "failed rust image pull returned success while the cached image inspects"
grep -q "pulling harness compiler" "$RECORD" || fail "prepare did not call record_rust_image_identity"
grep -q "BUILD ${REPOSYNC_IMAGE}" "$RECORD" && fail "prepare built the reposync image after a failed rust pull"
grep -q EXTRACT "$RECORD" && fail "prepare extracted a build graph after a failed rust pull"
grep -q RECORDED "$RECORD" && fail "prepare recorded images after a failed rust pull"
[ ! -f "$ARTIFACT_ROOT/build-graph/image-identity.txt" ] || fail "failed pull still wrote rust image identity"

# Exercise the real compiler identity function through prepare_images under if.
# Each command can print apparently valid cached output and still fail.
for fault in digest image_id empty_id rustc cargo; do
  docker() {
    case "$1" in
      pull) return 0 ;;
      image)
        if [ "$5" = '{{.Id}}' ]; then
          [ "$fault" = empty_id ] && return 0
          printf 'sha256:%064d\n' 0
          [ "$fault" != image_id ] || return 42
        else
          printf 'rust@sha256:%064d\n' 0
          [ "$fault" != digest ] || return 42
        fi ;;
      run)
        if [[ "$*" == *rustc* ]]; then
          printf 'rustc %s (cached)\n' "$RUST_TOOLCHAIN"
          [ "$fault" != rustc ] || return 42
        else
          printf 'cargo %s (cached)\n' "$RUST_TOOLCHAIN"
          [ "$fault" != cargo ] || return 42
        fi ;;
    esac
    return 0
  }
  reset_record
  if prepare_images; then fail "$fault failure accepted by prepare_images"; fi
  grep -q "BUILD ${REPOSYNC_IMAGE}" "$RECORD" && fail "$fault failure reached daemon build"
  grep -q RECORDED "$RECORD" && fail "$fault failure recorded success"
  [ ! -f "$ARTIFACT_ROOT/build-graph/image-identity.txt" ] || fail "$fault failure wrote identity receipt"
done

ACCEPTANCE_LAB_REMOVE_IMAGES=1
docker() {
  printf 'DOCKER %s\n' "$*" >> "$RECORD"
  if [ "$1" = "images" ]; then
    return 42
  fi
  printf '\n'
  return 0
}
reset_record
set +e
cleanup_run
ec=$?
set -e
[ "$ec" -ne 0 ] || fail "image enumeration failure returned success"
grep -q "DOCKER images" "$RECORD" || fail "cleanup did not query images"
grep -q "cleaned run" "$RECORD" && fail "image enumeration failure wrote a cleanup receipt"
unset ACCEPTANCE_LAB_REMOVE_IMAGES

docker() {
  printf 'DOCKER %s\n' "$*" >> "$RECORD"
  return 42
}
probe_root="$(mktemp -d)"
reset_record
set +e
record_interrupt_absence "$RUN_ID" 130 "$probe_root"
ec=$?
set -e
[ "$ec" -ne 0 ] || fail "post-interruption query failure returned success"
grep -q "DOCKER ps" "$RECORD" || fail "interruption absence check did not query containers"
[ ! -f "$probe_root/interrupt-proof.txt" ] || fail "post-interruption query failure wrote containers=0"
grep -q "interruption cleanup removed" "$RECORD" && fail "post-interruption query failure wrote a cleanup receipt"
rm -rf "$probe_root"

json="$(mktemp)"
regression_output="$(mktemp -d)"
set +e
LAB_REGRESSION_JSON="$json" LAB_REGRESSION_OUTPUT="$regression_output" npm --prefix "$ROOT/tests/acceptance-lab" exec -- playwright test -c "$ROOT/tests/acceptance-lab/playwright.regression.config.ts"
play_ec=$?
set -e
[ "$play_ec" -ne 0 ] || fail "false-pass playwright exited 0"
[ -s "$json" ] || fail "false-pass playwright did not write a report"
evidence="$(mktemp -d)"
node "$ROOT/tests/acceptance-lab/src/self-test.mjs" "$evidence"
cp "$json" "$evidence/playwright.json"
node --input-type=module -e 'import { writeFileSync } from "node:fs"; writeFileSync(process.argv[1], JSON.stringify({ candidate_sha: "a".repeat(40), suite: "smoke", seed: 1, run_id: "rsdeadbeef" }));' "$evidence/identity.json"
report_dir="$(mktemp -d)"
engine_dir="$(mktemp -d)"
set +e
LAB_EVENT_NAME=pull_request LAB_SUITE_INPUT= LAB_SEED_INPUT= GITHUB_RUN_ID= GITHUB_RUN_ATTEMPT= LAB_RUN_ID= \
  node "$ROOT/tests/acceptance-lab/src/ci-gate.mjs" aggregate \
    --evidence "$evidence" --engine "$engine_dir" --report-dir "$report_dir" \
    --candidate "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
agg_ec=$?
LAB_EVENT_NAME=pull_request LAB_SUITE_INPUT= LAB_SEED_INPUT= GITHUB_RUN_ID= GITHUB_RUN_ATTEMPT= LAB_RUN_ID= \
  node "$ROOT/tests/acceptance-lab/src/ci-gate.mjs" enforce \
    --evidence "$evidence" --engine "$engine_dir" --report-dir "$report_dir"
enf_ec=$?
set -e
[ "$agg_ec" -ne 0 ] || fail "aggregate accepted the false-pass Playwright report"
[ "$enf_ec" -ne 0 ] || fail "ci-gate enforce accepted the false-pass Playwright report"
node --input-type=module -e '
import { readFileSync } from "node:fs";
const report = JSON.parse(readFileSync(process.argv[1], "utf8"));
const row = (report.rows || []).find((item) => item.id === "LAB_FALSE_PASS");
if (!row || row.status !== "FAIL" || row.first_status !== "failed") {
  console.error(JSON.stringify(row));
  process.exit(1);
}
if (report.suite_result !== "FAIL") {
  console.error(report.suite_result);
  process.exit(1);
}
console.log(`suite_result ${report.suite_result}`);
console.log(`LAB_FALSE_PASS ${row.status} first=${row.first_status}`);
console.log(`aggregate_exit ${process.argv[2]}`);
console.log(`enforce_exit ${process.argv[3]}`);
' "$report_dir/report.json" "$agg_ec" "$enf_ec" || fail "false-pass report did not fail the suite"
rm -rf "$ARTIFACT_ROOT" "$probe_root" "$json" "$regression_output" "$evidence" "$report_dir" "$engine_dir"
node "$ROOT/tests/acceptance-lab/src/identity-check.mjs"
printf 'fail-closed checks passed\n'
