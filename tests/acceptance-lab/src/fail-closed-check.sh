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
printf 'fail-closed checks passed\n'
