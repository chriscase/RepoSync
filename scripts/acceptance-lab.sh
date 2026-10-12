#!/usr/bin/env bash
# RepoSync acceptance lab launcher.
# One command owns one run id, Compose project, port set, and artifact directory.
# down, reset, report, and clean require --run-id. There is no latest pointer.
# Cleanup removes only that run's labeled disposable resources.
#
#   scripts/acceptance-lab.sh test --suite smoke|full|soak --candidate <full-SHA> --seed <n>
#   scripts/acceptance-lab.sh prepare|up|down|reset|report|clean [flags]
#
# GitHub-hosted Ubuntu is the runtime qualification host. This script does not
# prune Docker, register runners, or contact a company endpoint.

set -euo pipefail

LAB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../tests/acceptance-lab" && pwd)"
REPO_ROOT="$(cd "$LAB_DIR/../.." && pwd)"
COMPOSE_FILE="$LAB_DIR/compose/docker-compose.yml"

# Recorded when this harness was written. These are not a live fetch and they
# are not the candidate under test. write_identity also records the live refs.
RECORDED_LIVE_MAIN="af394285869bdffa5f156c651cd1104027832749"
RECORDED_LIVE_MAIN_PARENT="068562a90bfafd9b2a142cea524a89574814cba5"
RECORDED_PR194_HEAD="6c84adf858a8a2e5d7ac3a9f618bee1834f4d297"
CLEANING=0

SUITE="smoke"
CANDIDATE=""
SEED="1"
RUN_ID=""
KEEP=0
CMD=""

usage() {
  echo "usage: scripts/acceptance-lab.sh prepare|up|down|reset|report|test|clean [--suite smoke|full|soak] [--candidate SHA] [--seed N] [--run-id ID] [--keep]" >&2
  exit 2
}

log() {
  local line
  line="$(date -u +%H:%M:%S) $*"
  echo "$line" >&2
  if [ -n "${RUN_DIR:-}" ]; then
    printf '%s\n' "$line" >> "$RUN_DIR/logs/launcher.log"
  fi
}

file_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

isolate_host_credentials() {
  unset GITHUB_TOKEN || true
  unset GH_TOKEN || true
  unset SSH_AUTH_SOCK || true
  unset SSH_AGENT_PID || true
  unset GIT_ASKPASS || true
}

artifact_root() {
  if [ -n "${ACCEPTANCE_LAB_ARTIFACT_ROOT:-}" ]; then
    ARTIFACT_ROOT="$ACCEPTANCE_LAB_ARTIFACT_ROOT"
  elif [ -n "${RUNNER_TEMP:-}" ]; then
    ARTIFACT_ROOT="${RUNNER_TEMP}/acceptance-lab"
  else
    ARTIFACT_ROOT="/tmp/acceptance-lab"
  fi
  case "$ARTIFACT_ROOT" in
    "$REPO_ROOT"|"$REPO_ROOT"/*)
      echo "artifact root must stay outside the checkout: $ARTIFACT_ROOT" >&2
      exit 2
      ;;
  esac
  mkdir -p "$ARTIFACT_ROOT"
}

require_tools() {
  local missing=0
  local tool
  for tool in node python3 openssl git; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      echo "required tool is not on PATH: $tool" >&2
      missing=1
    fi
  done
  if [ "$missing" -ne 0 ]; then
    exit 1
  fi
}

require_docker() {
  if ! command -v docker >/dev/null 2>&1; then
    echo "docker is not available; runtime qualification is NOT_RUN on this host" >&2
    return 1
  fi
  docker compose version >/dev/null 2>&1 || {
    echo "docker compose is not available" >&2
    return 1
  }
}

resolve_candidate() {
  local head
  head="$(git -C "$REPO_ROOT" rev-parse HEAD)"
  if [ -z "$CANDIDATE" ]; then
    CANDIDATE="$head"
  fi
  if ! printf '%s\n' "$CANDIDATE" | grep -Eq '^[0-9a-f]{40}$'; then
    echo "candidate must be the full 40-character lowercase SHA" >&2
    exit 2
  fi
  if [ "$CANDIDATE" != "$head" ]; then
    echo "candidate $CANDIDATE does not equal HEAD $head" >&2
    exit 2
  fi
  HEAD_SHA="$head"
}

load_pins() {
  # pins.env is a fixed assignment file in the harness. It has no secrets.
  set -a
  # shellcheck disable=SC1091
  . "$LAB_DIR/pins.env"
  set +a
  SVN_IMAGE="rsacc-svn:lab"
  SVNSERVE_IMAGE="rsacc-svnserve:lab"
  GITEA_LAB_IMAGE="rsacc-gitea:lab"
  GITHTTP_IMAGE="rsacc-githttp:lab"
  PYENV_IMAGE="rsacc-pyenv:lab"
  OBSERVER_IMAGE="rsacc-observer:lab"
  FORWARDER_IMAGE="rsacc-forwarder:lab"
  REPOSYNC_IMAGE="rsacc-reposync:${CANDIDATE}"
}

new_secret() {
  printf '%s!Aa\n' "$(openssl rand -hex 16)"
}

parse_args() {
  if [ "$#" -lt 1 ]; then
    usage
  fi
  CMD="$1"
  shift
  case "$CMD" in
    prepare|up|down|reset|report|test|clean) ;;
    *) usage ;;
  esac
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --suite)
        [ "$#" -ge 2 ] || usage
        SUITE="$2"
        shift 2
        ;;
      --candidate)
        [ "$#" -ge 2 ] || usage
        CANDIDATE="$2"
        shift 2
        ;;
      --seed)
        [ "$#" -ge 2 ] || usage
        SEED="$2"
        shift 2
        ;;
      --run-id)
        [ "$#" -ge 2 ] || usage
        RUN_ID="$2"
        shift 2
        ;;
      --keep)
        KEEP=1
        shift
        ;;
      *)
        usage
        ;;
    esac
  done
  case "$SUITE" in
    smoke|full|soak) ;;
    *)
      echo "suite must be smoke, full, or soak" >&2
      exit 2
      ;;
  esac
  case "$SEED" in
    ''|*[!0-9]*)
      echo "seed must be an integer" >&2
      exit 2
      ;;
  esac
  if [ -n "$RUN_ID" ] && ! printf '%s\n' "$RUN_ID" | grep -Eq '^rs[0-9a-f]{8}$'; then
    echo "run id must look like rs followed by 8 hex characters" >&2
    exit 2
  fi
  case "$CMD" in
    down|reset|report|clean)
      if [ -z "$RUN_ID" ]; then
        echo "$CMD requires --run-id and does not guess a stack" >&2
        exit 2
      fi
      ;;
  esac
}

remember_run_id() {
  if [ -n "${GITHUB_ACTIONS:-}" ] && [ -n "${RUNNER_TEMP:-}" ]; then
    printf '%s\n' "$RUN_ID" > "${RUNNER_TEMP}/acceptance-lab-run-id"
  fi
  if [ -n "${ACCEPTANCE_LAB_RUN_ID_FILE:-}" ]; then
    printf '%s\n' "$RUN_ID" > "$ACCEPTANCE_LAB_RUN_ID_FILE"
  fi
}

write_compose_env() {
  local token
  token="$(awk -F= '$1=="GITEA_ADMIN_TOKEN" { print substr($0, index($0, "=")+1); exit }' "$RUN_DIR/secrets.env")"
  umask 077
  cat > "$RUN_DIR/compose.env" <<EOF
LDAP_IMAGE=${LDAP_IMAGE}
DEBIAN_IMAGE=${DEBIAN_IMAGE}
RUST_IMAGE=${RUST_IMAGE}
NODE_IMAGE=${NODE_IMAGE}
GITEA_IMAGE=${GITEA_IMAGE}
RUST_REPO_DIGEST=${RUST_REPO_DIGEST:-unrecorded}
SVN_IMAGE=${SVN_IMAGE}
SVNSERVE_IMAGE=${SVNSERVE_IMAGE}
GITEA_LAB_IMAGE=${GITEA_LAB_IMAGE}
GITHTTP_IMAGE=${GITHTTP_IMAGE}
PYENV_IMAGE=${PYENV_IMAGE}
OBSERVER_IMAGE=${OBSERVER_IMAGE}
FORWARDER_IMAGE=${FORWARDER_IMAGE}
REPOSYNC_IMAGE=${REPOSYNC_IMAGE}
LDAP_ADMIN_PASSWORD=$(awk -F= '$1=="LDAP_ADMIN_PASSWORD"{print substr($0,index($0,"=")+1); exit}' "$RUN_DIR/secrets.env")
LDAP_BIND_PASSWORD=$(awk -F= '$1=="LDAP_BIND_PASSWORD"{print substr($0,index($0,"=")+1); exit}' "$RUN_DIR/secrets.env")
SVN_SYNC_PASSWORD=$(awk -F= '$1=="SVN_SYNC_PASSWORD"{print substr($0,index($0,"=")+1); exit}' "$RUN_DIR/secrets.env")
GITEA_ADMIN_TOKEN=${token}
REPOSYNC_ADMIN_PASSWORD=$(awk -F= '$1=="REPOSYNC_ADMIN_PASSWORD"{print substr($0,index($0,"=")+1); exit}' "$RUN_DIR/secrets.env")
REPOSYNC_SESSION_SECRET=$(awk -F= '$1=="REPOSYNC_SESSION_SECRET"{print substr($0,index($0,"=")+1); exit}' "$RUN_DIR/secrets.env")
REPOSYNC_WEBHOOK_SECRET=$(awk -F= '$1=="REPOSYNC_WEBHOOK_SECRET"{print substr($0,index($0,"=")+1); exit}' "$RUN_DIR/secrets.env")
REPOSYNC_SVN_WEBHOOK_SECRET=$(awk -F= '$1=="REPOSYNC_SVN_WEBHOOK_SECRET"{print substr($0,index($0,"=")+1); exit}' "$RUN_DIR/secrets.env")
LAB_RUN_DIR=${RUN_DIR}
RUN_ID=${RUN_ID}
COMPOSE_PROJECT_NAME=${PROJECT}
LAB_SVN_PORT=${LAB_SVN_PORT}
LAB_SVNSERVE_PORT=${LAB_SVNSERVE_PORT}
LAB_GITEA_PORT=${LAB_GITEA_PORT}
LAB_GITHTTP_PORT=${LAB_GITHTTP_PORT}
LAB_FAULT_PORT=${LAB_FAULT_PORT}
LAB_FAULT_CONTROL_PORT=${LAB_FAULT_CONTROL_PORT}
LAB_UI_PORT=${LAB_UI_PORT}
EOF
  chmod 600 "$RUN_DIR/compose.env"
  umask 022
}

compose() {
  docker compose --env-file "$RUN_DIR/compose.env" -f "$COMPOSE_FILE" -p "$PROJECT" "$@"
}

pick_ports() {
  python3 - <<'PY'
import socket
socks = []
ports = []
for _ in range(7):
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    ports.append(str(sock.getsockname()[1]))
    socks.append(sock)
print(" ".join(ports))
for sock in socks:
    sock.close()
PY
}

write_certs() {
  local dir="$RUN_DIR/certs"
  mkdir -p "$dir"
  chmod 755 "$dir"
  cat > "$dir/ca.cnf" <<'EOF'
[req]
distinguished_name = dn
x509_extensions = v3_ca
prompt = no
[dn]
CN = RepoSync Lab CA
[v3_ca]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
subjectKeyIdentifier = hash
EOF
  openssl req -x509 -new -nodes -newkey rsa:2048 \
    -keyout "$dir/ca.key" -out "$dir/ca.crt" -days 2 \
    -config "$dir/ca.cnf" -extensions v3_ca >/dev/null 2>&1
  chmod 644 "$dir/ca.key" "$dir/ca.crt"
  write_one_cert ldap ldap
  write_one_cert svn svn
  write_one_cert gitea gitea
  write_one_cert denied denied-target
  write_one_cert fault fault-proxy
  write_one_cert githttp git-http
  rm -f "$dir/ca.cnf" "$dir"/*.csr "$dir"/*.cnf
}

write_one_cert() {
  local name="$1"
  local san="$2"
  local dir="$RUN_DIR/certs"
  cat > "$dir/$name.cnf" <<EOF
[req]
distinguished_name = dn
req_extensions = v3_req
prompt = no
[dn]
CN = ${san}
[v3_req]
subjectAltName = DNS:${san}
keyUsage = digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth
EOF
  openssl req -new -nodes -newkey rsa:2048 \
    -keyout "$dir/$name.key" -out "$dir/$name.csr" -config "$dir/$name.cnf" >/dev/null 2>&1
  openssl x509 -req -in "$dir/$name.csr" \
    -CA "$dir/ca.crt" -CAkey "$dir/ca.key" -CAcreateserial \
    -out "$dir/$name.crt" -days 2 -extfile "$dir/$name.cnf" -extensions v3_req >/dev/null 2>&1
  chmod 644 "$dir/$name.key" "$dir/$name.crt"
  rm -f "$dir/$name.csr" "$dir/$name.cnf"
}

render_configs() {
  python3 - "$LAB_DIR" "$RUN_DIR" <<'PY'
import pathlib, sys
lab, run = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
secrets = {}
for line in (run / "secrets.env").read_text().splitlines():
    if not line or line.startswith("#") or "=" not in line:
        continue
    key, value = line.split("=", 1)
    secrets[key] = value
ldif = (lab / "docker/ldap/seed.ldif.template").read_text()
mapping = {
    "@@LDAP_BIND_PASSWORD@@": secrets["LDAP_BIND_PASSWORD"],
    "@@SVN_SYNC_PASSWORD@@": secrets["SVN_SYNC_PASSWORD"],
    "@@ALICE_PASSWORD@@": secrets["ALICE_PASSWORD"],
    "@@BOB_PASSWORD@@": secrets["BOB_PASSWORD"],
    "@@OBSERVER_PASSWORD@@": secrets["OBSERVER_PASSWORD"],
    "@@NOMAIL_PASSWORD@@": secrets["NOMAIL_PASSWORD"],
    "@@DUP_PASSWORD@@": secrets["DUP_PASSWORD"],
    "@@SAM_PASSWORD@@": secrets["SAM_PASSWORD"],
}
for key, value in mapping.items():
    if key not in ldif:
        raise SystemExit(f"missing LDIF placeholder {key}")
    ldif = ldif.replace(key, value)
if "@@" in ldif:
    raise SystemExit("unreplaced LDIF placeholder")
# osixia rewrites this file with sed -i. A single-file mount cannot be
# renamed, so the seed is a normal file inside a mounted directory.
seed_dir = run / "ldap-seed"
seed_dir.mkdir(exist_ok=True)
(seed_dir / "50-lab.ldif").write_text(ldif)
apache = (lab / "docker/svn/apache.conf.template").read_text()
apache = apache.replace("@@LDAP_BIND_DN@@", "uid=svc-bind,ou=services,dc=lab,dc=invalid")
apache = apache.replace("@@LDAP_BIND_PASSWORD@@", secrets["LDAP_BIND_PASSWORD"])
if "@@" in apache:
    raise SystemExit("unreplaced Apache placeholder")
(run / "svn-apache.conf").write_text(apache)
passwd = "\n".join([
    "[users]",
    f"svc-sync = {secrets['SVN_SYNC_PASSWORD']}",
    f"alice = {secrets['ALICE_PASSWORD']}",
    f"bob = {secrets['BOB_PASSWORD']}",
    f"observer = {secrets['OBSERVER_PASSWORD']}",
    "",
])
(run / "svnserve-passwd").write_text(passwd)
PY
  cp "$LAB_DIR/config/daemon.toml.template" "$RUN_DIR/daemon.toml"
  python3 - "$RUN_DIR/daemon.toml" "$LAB_UI_PORT" <<'PY'
import pathlib, sys
path, port = pathlib.Path(sys.argv[1]), sys.argv[2]
if not port.isdigit():
    raise SystemExit("ui port is not an integer")
text = path.read_text()
needle = 'admin_password_env = "REPOSYNC_ADMIN_PASSWORD"\n'
origin = f'cors_origins = ["http://127.0.0.1:{port}"]\n'
for raw in text.splitlines():
    if raw.lstrip().startswith("#"):
        continue
    if raw.strip().startswith("cors_origins"):
        raise SystemExit("daemon template already sets cors_origins")
if needle not in text:
    raise SystemExit("daemon template has no admin_password_env line")
path.write_text(text.replace(needle, needle + origin, 1))
PY
  cp "$LAB_DIR/config/authors.toml" "$RUN_DIR/authors.toml"
  cp "$LAB_DIR/config/svn-authz" "$RUN_DIR/svn-authz"
  cp "$LAB_DIR/config/svn-authz" "$RUN_DIR/svnserve-authz"
  cp "$LAB_DIR/docker/ldap/svc-bind-acl.ldif" "$RUN_DIR/ldap-seed/60-svc-bind-acl.ldif"
  chmod 755 "$RUN_DIR/ldap-seed"
  chmod 644 "$RUN_DIR/ldap-seed/50-lab.ldif" "$RUN_DIR/ldap-seed/60-svc-bind-acl.ldif" \
    "$RUN_DIR/svn-apache.conf" "$RUN_DIR/svnserve-passwd" \
    "$RUN_DIR/daemon.toml" "$RUN_DIR/authors.toml" "$RUN_DIR/svn-authz" "$RUN_DIR/svnserve-authz"
}

scan_rendered_config() {
  python3 - "$RUN_DIR" <<'PY'
import pathlib, re, sys
run = pathlib.Path(sys.argv[1])
pattern = re.compile(r"company\.com|github\.com|github\.company|mentorg|svn\.company|api\.github\.com|test-token", re.I)
names = [
    "daemon.toml", "authors.toml", "ldap-seed/50-lab.ldif", "ldap-seed/60-svc-bind-acl.ldif",
    "svn-apache.conf",
    "svn-authz", "svnserve-passwd", "svnserve-authz", "compose.env",
]
bad = []
for name in names:
    text = (run / name).read_text(errors="replace")
    if pattern.search(text):
        bad.append(name)
if bad:
    print("rendered run file names a forbidden host or the literal test-token:", ", ".join(bad))
    sys.exit(1)
PY
}

write_run_json() {
  python3 - "$RUN_DIR/run.json" "$RUN_ID" "$PROJECT" "$SUITE" "$SEED" "$CANDIDATE" \
    "$LAB_SVN_PORT" "$LAB_SVNSERVE_PORT" "$LAB_GITEA_PORT" "$LAB_GITHTTP_PORT" \
    "$LAB_FAULT_PORT" "$LAB_FAULT_CONTROL_PORT" "$LAB_UI_PORT" "$REPOSYNC_IMAGE" <<'PY'
import json, sys
path, run_id, project, suite, seed, candidate = sys.argv[1:7]
ports = {
    "svn": int(sys.argv[7]),
    "svnserve": int(sys.argv[8]),
    "gitea": int(sys.argv[9]),
    "githttp": int(sys.argv[10]),
    "fault": int(sys.argv[11]),
    "fault_control": int(sys.argv[12]),
    "ui": int(sys.argv[13]),
}
payload = {
    "run_id": run_id,
    "project": project,
    "suite": suite,
    "seed": int(seed),
    "candidate": candidate,
    "ports": ports,
    "reposync_image": sys.argv[14],
}
open(path, "w").write(json.dumps(payload, indent=2) + "\n")
PY
  chmod 600 "$RUN_DIR/run.json"
}

load_run() {
  RUN_DIR="$ARTIFACT_ROOT/$RUN_ID"
  if [ ! -f "$RUN_DIR/run.json" ]; then
    echo "no run.json for $RUN_ID under $ARTIFACT_ROOT; refusing to guess a stack" >&2
    exit 1
  fi
  if [ ! -f "$RUN_DIR/compose.env" ]; then
    echo "run $RUN_ID is missing compose.env; refusing to guess a stack" >&2
    exit 1
  fi
  eval "$(python3 - "$RUN_DIR/run.json" "$RUN_ID" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
expected = sys.argv[2]
if data.get("run_id") != expected:
    raise SystemExit("run.json does not match --run-id")
project = str(data.get("project", ""))
if expected not in project or not project.startswith("rsacc"):
    raise SystemExit("compose project is not bound to this run id")
def emit(name, value):
    text = str(value)
    if not text or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.:/-" for c in text):
        raise SystemExit(f"unsafe run.json value for {name}")
    print(f"{name}={text}")
emit("PROJECT", project)
emit("SUITE", data["suite"])
emit("SEED", data["seed"])
emit("CANDIDATE", data["candidate"])
ports = data["ports"]
emit("LAB_SVN_PORT", ports["svn"])
emit("LAB_SVNSERVE_PORT", ports["svnserve"])
emit("LAB_GITEA_PORT", ports["gitea"])
emit("LAB_GITHTTP_PORT", ports["githttp"])
emit("LAB_FAULT_PORT", ports["fault"])
emit("LAB_FAULT_CONTROL_PORT", ports["fault_control"])
emit("LAB_UI_PORT", ports["ui"])
emit("REPOSYNC_IMAGE", data["reposync_image"])
PY
)"
  mkdir -p "$RUN_DIR/logs" "$RUN_DIR/evidence"
  EVIDENCE="$RUN_DIR/evidence"
}

ensure_layout() {
  if [ -z "$RUN_ID" ]; then
    RUN_ID="rs$(openssl rand -hex 4)"
  fi
  PROJECT="rsacc${RUN_ID}"
  RUN_DIR="$ARTIFACT_ROOT/$RUN_ID"
  if [ -f "$RUN_DIR/run.json" ]; then
    load_run
    load_pins
    return 0
  fi
  umask 077
  mkdir -p "$RUN_DIR/logs" "$RUN_DIR/evidence" "$RUN_DIR/certs"
  chmod 700 "$RUN_DIR"
  EVIDENCE="$RUN_DIR/evidence"
  local ports
  ports="$(pick_ports)"
  # shellcheck disable=SC2086
  set -- $ports
  LAB_SVN_PORT="$1"
  LAB_SVNSERVE_PORT="$2"
  LAB_GITEA_PORT="$3"
  LAB_GITHTTP_PORT="$4"
  LAB_FAULT_PORT="$5"
  LAB_FAULT_CONTROL_PORT="$6"
  LAB_UI_PORT="$7"
  cat > "$RUN_DIR/secrets.env" <<EOF
LDAP_ADMIN_PASSWORD=$(new_secret)
LDAP_BIND_PASSWORD=$(new_secret)
SVN_SYNC_PASSWORD=$(new_secret)
ALICE_PASSWORD=$(new_secret)
BOB_PASSWORD=$(new_secret)
OBSERVER_PASSWORD=$(new_secret)
NOMAIL_PASSWORD=$(new_secret)
DUP_PASSWORD=$(new_secret)
SAM_PASSWORD=$(new_secret)
GITEA_ADMIN_PASSWORD=$(new_secret)
REPOSYNC_ADMIN_PASSWORD=$(new_secret)
REPOSYNC_SESSION_SECRET=$(new_secret)
REPOSYNC_WEBHOOK_SECRET=$(new_secret)
REPOSYNC_SVN_WEBHOOK_SECRET=$(new_secret)
LAB_LOCAL_PASSWORD=$(new_secret)
GITEA_ADMIN_TOKEN=
EOF
  chmod 600 "$RUN_DIR/secrets.env"
  load_pins
  write_certs
  render_configs
  write_compose_env
  scan_rendered_config
  write_run_json
  remember_run_id
  umask 022
  log "created run $RUN_ID project $PROJECT"
}

refresh_secret_values() {
  awk -F= 'NF >= 2 {
    value = substr($0, index($0, "=") + 1)
    if (length(value) >= 8) print value
  }' "$RUN_DIR/secrets.env" > "$RUN_DIR/secret-values.txt"
  chmod 600 "$RUN_DIR/secret-values.txt"
}

set_token() {
  local token="$1"
  python3 - "$RUN_DIR/secrets.env" "$token" <<'PY'
import pathlib, sys
path, token = pathlib.Path(sys.argv[1]), sys.argv[2]
lines = path.read_text().splitlines()
found = False
out = []
for line in lines:
    if line.startswith("GITEA_ADMIN_TOKEN="):
        out.append("GITEA_ADMIN_TOKEN=" + token)
        found = True
    else:
        out.append(line)
if not found:
    out.append("GITEA_ADMIN_TOKEN=" + token)
path.write_text("\n".join(out) + "\n")
PY
  chmod 600 "$RUN_DIR/secrets.env"
  printf '%s\n' "$token" > "$RUN_DIR/gitea.token"
  chmod 600 "$RUN_DIR/gitea.token"
  write_compose_env
}

clear_token() {
  set_token ""
  rm -f "$RUN_DIR/gitea.token"
}

extract_token() {
  awk 'NF { line=$NF } END { print line }' "$1"
}

seed_gitea() {
  local attempt ec token stdout
  stdout="$RUN_DIR/logs/gitea-cli.out"
  : > "$RUN_DIR/logs/seed.log"
  log "seeding gitea admin"
  attempt=0
  ec=1
  while [ "$attempt" -lt 5 ]; do
    attempt=$((attempt + 1))
    set +e
    compose exec -T -u git gitea gitea admin user create \
      --config /data/gitea/conf/app.ini \
      --admin \
      --username labadmin \
      --password "$(awk -F= '$1=="GITEA_ADMIN_PASSWORD"{print substr($0,index($0,"=")+1); exit}' "$RUN_DIR/secrets.env")" \
      --email labadmin@lab.invalid >"$stdout" 2>>"$RUN_DIR/logs/seed.err"
    ec=$?
    set -e
    log "gitea admin user create attempt $attempt exit $ec"
    if [ "$ec" -eq 0 ]; then
      break
    fi
    sleep 3
  done
  rm -f "$stdout"
  if [ "$ec" -ne 0 ]; then
    echo "gitea admin user was not created" >&2
    return 1
  fi
  token=""
  for variant in scopes-raw scopes plain; do
    set +e
    case "$variant" in
      scopes-raw)
        compose exec -T -u git gitea gitea admin user generate-access-token \
          --config /data/gitea/conf/app.ini \
          --username labadmin --token-name "lab-${RUN_ID}" --scopes all --raw >"$stdout" 2>>"$RUN_DIR/logs/seed.err"
        ;;
      scopes)
        compose exec -T -u git gitea gitea admin user generate-access-token \
          --config /data/gitea/conf/app.ini \
          --username labadmin --token-name "lab-${RUN_ID}" --scopes all >"$stdout" 2>>"$RUN_DIR/logs/seed.err"
        ;;
      plain)
        compose exec -T -u git gitea gitea admin user generate-access-token \
          --config /data/gitea/conf/app.ini \
          --username labadmin --token-name "lab-${RUN_ID}" >"$stdout" 2>>"$RUN_DIR/logs/seed.err"
        ;;
    esac
    ec=$?
    set -e
    log "gitea token variant $variant exit $ec"
    if [ "$ec" -eq 0 ]; then
      token="$(extract_token "$stdout")"
      rm -f "$stdout"
      if [ "$token" = "test-token" ] || ! printf '%s\n' "$token" | grep -Eq '^[0-9a-fA-F]{20,}$'; then
        token=""
      fi
      if [ -n "$token" ]; then
        break
      fi
    else
      rm -f "$stdout"
    fi
  done
  if [ -z "$token" ]; then
    echo "gitea did not return a usable access token" >&2
    return 1
  fi
  set_token "$token"
  unset token
  export_gitea_token
  set +e
  compose exec -T -u git -e LAB_TOKEN gitea sh -c 'curl --fail --silent --show-error --cacert /certs/ca.crt --resolve gitea:3000:127.0.0.1 -H "Authorization: token ${LAB_TOKEN}" -H "Content-Type: application/json" -d "{\"username\":\"lab\"}" https://gitea:3000/api/v1/orgs' >"$RUN_DIR/logs/gitea-org.out" 2>>"$RUN_DIR/logs/seed.err"
  ec=$?
  set -e
  log "gitea org create exit $ec"
  set +e
  compose exec -T -u git -e LAB_TOKEN gitea sh -c 'curl --fail --silent --show-error --cacert /certs/ca.crt --resolve gitea:3000:127.0.0.1 -H "Authorization: token ${LAB_TOKEN}" -H "Content-Type: application/json" -d "{\"name\":\"bootstrap\",\"private\":true,\"auto_init\":false,\"default_branch\":\"main\"}" https://gitea:3000/api/v1/orgs/lab/repos' >"$RUN_DIR/logs/gitea-repo.out" 2>>"$RUN_DIR/logs/seed.err"
  ec=$?
  set -e
  log "gitea bootstrap repo create exit $ec"
  if [ "$ec" -ne 0 ]; then
    echo "gitea bootstrap repository was not created" >&2
    return 1
  fi
  set +e
  compose exec -T -u git gitea gitea admin auth add-ldap \
    --config /data/gitea/conf/app.ini \
    --name lab-ldap \
    --security-protocol ldaps \
    --host ldap \
    --port 636 \
    --bind-dn "uid=svc-bind,ou=services,dc=lab,dc=invalid" \
    --bind-password "$(awk -F= '$1=="LDAP_BIND_PASSWORD"{print substr($0,index($0,"=")+1); exit}' "$RUN_DIR/secrets.env")" \
    --user-search-base "ou=people,dc=lab,dc=invalid" \
    --user-filter '(&(objectClass=inetOrgPerson)(uid=%s))' \
    --username-attribute uid \
    --firstname-attribute cn \
    --surname-attribute sn \
    --email-attribute mail >"$RUN_DIR/logs/gitea-ldap.out" 2>>"$RUN_DIR/logs/seed.err"
  ec=$?
  set -e
  log "gitea ldap cli exit $ec"
  if [ "$ec" -ne 0 ]; then
    set +e
    compose exec -T -u git -e LAB_TOKEN -e LDAP_BIND_PASSWORD gitea sh -c 'curl --fail --silent --show-error --cacert /certs/ca.crt --resolve gitea:3000:127.0.0.1 -H "Authorization: token ${LAB_TOKEN}" -H "Content-Type: application/json" -d "{\"type\":2,\"name\":\"lab-ldap\",\"host\":\"ldap\",\"port\":636,\"security_protocol\":1,\"skip_verify\":false,\"active\":true,\"bind_dn\":\"uid=svc-bind,ou=services,dc=lab,dc=invalid\",\"bind_password\":\"${LDAP_BIND_PASSWORD}\",\"user_base\":\"ou=people,dc=lab,dc=invalid\",\"user_filter\":\"(&(objectClass=inetOrgPerson)(uid=%s))\",\"attribute_username\":\"uid\",\"attribute_name\":\"cn\",\"attribute_surname\":\"sn\",\"attribute_mail\":\"mail\"}" https://gitea:3000/api/v1/admin/auths' >"$RUN_DIR/logs/gitea-ldap-api.out" 2>>"$RUN_DIR/logs/seed.err"
    ec=$?
    set -e
    log "gitea ldap api exit $ec"
  fi
  rm -f "$RUN_DIR/logs/gitea-org.out" "$RUN_DIR/logs/gitea-repo.out" "$RUN_DIR/logs/gitea-ldap.out" "$RUN_DIR/logs/gitea-ldap-api.out"
  unset LAB_TOKEN || true
}

export_gitea_token() {
  LAB_TOKEN="$(awk -F= '$1=="GITEA_ADMIN_TOKEN"{print substr($0,index($0,"=")+1); exit}' "$RUN_DIR/secrets.env")"
  export LAB_TOKEN
}

build_one() {
  local tag="$1"
  local dockerfile="$2"
  local context="$3"
  shift 3
  log "docker build $tag"
  docker build --pull -t "$tag" -f "$dockerfile" "$@" "$context"
}

graph_dest() {
  if [ -n "${EVIDENCE:-}" ]; then
    printf '%s\n' "$EVIDENCE/build-graph"
  else
    printf '%s\n' "$ARTIFACT_ROOT/build-graph"
  fi
}

record_rust_image_identity() {
  local dest digest rustc_line cargo_line
  dest="$(graph_dest)"
  mkdir -p "$dest"
  log "pulling harness compiler ${RUST_IMAGE}"
  docker pull "$RUST_IMAGE"
  digest="$(docker image inspect "$RUST_IMAGE" --format '{{index .RepoDigests 0}}')"
  rustc_line="$(docker run --rm "$RUST_IMAGE" rustc --version)"
  cargo_line="$(docker run --rm "$RUST_IMAGE" cargo --version)"
  RUST_REPO_DIGEST="$digest"
  {
    echo "time=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "candidate=${CANDIDATE}"
    echo "rust_image=${RUST_IMAGE}"
    echo "rust_toolchain=${RUST_TOOLCHAIN}"
    echo "aligned_with=Dockerfile.reliability rust:1.98.1-bookworm"
    echo "previous_lab_pin=rust:1.83.0-bookworm"
    echo "product_dockerfile=unchanged rust:1.83-bookworm"
    echo "note=this pin does not prove the product Dockerfile or its MSRV"
    echo "rust_image_id=$(docker image inspect "$RUST_IMAGE" --format '{{.Id}}')"
    echo "rust_repo_digest=${digest}"
    echo "rustc=${rustc_line}"
    echo "cargo=${cargo_line}"
  } > "$dest/image-identity.txt"
  if [ -z "$digest" ]; then
    echo "rust image ${RUST_IMAGE} has no RepoDigest after pull" >&2
    return 1
  fi
  case "$rustc_line" in
    "rustc ${RUST_TOOLCHAIN} "*|"rustc ${RUST_TOOLCHAIN}") ;;
    *)
      echo "rustc identity does not match ${RUST_TOOLCHAIN}: ${rustc_line}" >&2
      return 1
      ;;
  esac
  case "$cargo_line" in
    "cargo ${RUST_TOOLCHAIN} "*|"cargo ${RUST_TOOLCHAIN}") ;;
    *)
      echo "cargo identity does not match ${RUST_TOOLCHAIN}: ${cargo_line}" >&2
      return 1
      ;;
  esac
  log "recorded ${RUST_IMAGE} ${digest}"
}

# Local images often have an empty RepoDigests list. `index .RepoDigests 0`
# exits 1 on that list and drops the immutable id. Record the id, size, and
# the whole list. An empty list stays empty. This does not invent a digest.
inspect_image_metadata() {
  local image="$1"
  local line
  line="$(docker image inspect "$image" --format 'id={{.Id}} size={{.Size}} repo_digests={{json .RepoDigests}}')" || return 1
  case "$line" in
    id=sha256:*" size="*" repo_digests="\[*\]) ;;
    *)
      echo "image identity for ${image} lacks an immutable id or digest list: ${line}" >&2
      return 1
      ;;
  esac
  printf '%s\n' "$line"
}

extract_build_graph() {
  local dest cid copy_lock copy_sum copy_meta copy_rustc rm_ec expected actual name
  dest="$(graph_dest)"
  mkdir -p "$dest"
  cid="$(docker create "$REPOSYNC_IMAGE")"
  set +e
  docker cp "$cid:/usr/local/share/acceptance-lab/Cargo.lock" "$dest/Cargo.lock"
  copy_lock=$?
  docker cp "$cid:/usr/local/share/acceptance-lab/Cargo.lock.sha256" "$dest/Cargo.lock.sha256"
  copy_sum=$?
  docker cp "$cid:/usr/local/share/acceptance-lab/cargo-metadata.json" "$dest/cargo-metadata.json"
  copy_meta=$?
  docker cp "$cid:/usr/local/share/acceptance-lab/rustc-version.txt" "$dest/rustc-version.txt"
  copy_rustc=$?
  docker rm "$cid" >/dev/null
  rm_ec=$?
  set -e
  if [ "$copy_lock" -ne 0 ] || [ "$copy_sum" -ne 0 ] || [ "$copy_meta" -ne 0 ] || [ "$copy_rustc" -ne 0 ] || [ "$rm_ec" -ne 0 ]; then
    echo "failed to copy the resolved build graph out of ${REPOSYNC_IMAGE}" >&2
    return 1
  fi
  for name in Cargo.lock Cargo.lock.sha256 cargo-metadata.json rustc-version.txt; do
    if [ ! -s "$dest/$name" ]; then
      echo "build graph evidence is empty: $dest/$name" >&2
      return 1
    fi
  done
  if grep -q '^rust_repo_digest=unrecorded$' "$dest/rustc-version.txt"; then
    echo "image did not record a rust repo digest" >&2
    return 1
  fi
  if [ -n "${RUST_REPO_DIGEST:-}" ]; then
    if ! grep -F -x -q "rust_repo_digest=${RUST_REPO_DIGEST}" "$dest/rustc-version.txt"; then
      echo "image rust digest does not match the pulled RepoDigest" >&2
      return 1
    fi
  fi
  expected="$(awk 'NR==1 { print $1 }' "$dest/Cargo.lock.sha256")"
  actual="$(file_sha256 "$dest/Cargo.lock")"
  if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
    echo "Cargo.lock sha256 does not match the image record" >&2
    return 1
  fi
  python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$dest/cargo-metadata.json"
  if [ ! -s "$dest/image-identity.txt" ]; then
    {
      echo "time=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      echo "candidate=${CANDIDATE}"
      echo "identity_source=loaded image rustc-version.txt"
      echo "aligned_with=Dockerfile.reliability rust:1.98.1-bookworm"
      echo "previous_lab_pin=rust:1.83.0-bookworm"
      echo "product_dockerfile=unchanged rust:1.83-bookworm"
      cat "$dest/rustc-version.txt"
    } > "$dest/image-identity.txt"
  fi
  local meta
  meta="$(inspect_image_metadata "$REPOSYNC_IMAGE")" || return 1
  {
    echo "reposync_image=${REPOSYNC_IMAGE}"
    printf 'reposync_%s\n' "$meta"
    echo "lock_sha256=${actual}"
  } > "$dest/reposync-image.txt"
  if [ ! -s "$dest/image-identity.txt" ] || [ ! -s "$dest/reposync-image.txt" ]; then
    echo "image identity evidence is empty" >&2
    return 1
  fi
  log "recorded build graph at $dest"
}

prepare_images() {
  require_docker
  load_pins
  local scope="${ACCEPTANCE_LAB_PREPARE_SCOPE:-all}"
  if [ "$scope" = "all" ] || [ "$scope" = "fixtures" ]; then
    build_one "$SVN_IMAGE" "$LAB_DIR/docker/Dockerfile.svn" "$LAB_DIR/docker" --build-arg "DEBIAN_IMAGE=${DEBIAN_IMAGE}"
    build_one "$SVNSERVE_IMAGE" "$LAB_DIR/docker/Dockerfile.svnserve" "$LAB_DIR/docker" --build-arg "DEBIAN_IMAGE=${DEBIAN_IMAGE}"
    build_one "$GITEA_LAB_IMAGE" "$LAB_DIR/docker/Dockerfile.gitea" "$LAB_DIR/docker" --build-arg "GITEA_IMAGE=${GITEA_IMAGE}"
    build_one "$GITHTTP_IMAGE" "$LAB_DIR/docker/Dockerfile.githttp" "$LAB_DIR/docker" --build-arg "DEBIAN_IMAGE=${DEBIAN_IMAGE}"
    build_one "$PYENV_IMAGE" "$LAB_DIR/docker/Dockerfile.pyenv" "$LAB_DIR/docker" --build-arg "DEBIAN_IMAGE=${DEBIAN_IMAGE}"
    build_one "$OBSERVER_IMAGE" "$LAB_DIR/docker/Dockerfile.observer" "$LAB_DIR/docker" --build-arg "DEBIAN_IMAGE=${DEBIAN_IMAGE}"
    build_one "$FORWARDER_IMAGE" "$LAB_DIR/docker/Dockerfile.forwarder" "$LAB_DIR/docker" --build-arg "DEBIAN_IMAGE=${DEBIAN_IMAGE}"
    docker pull "$LDAP_IMAGE"
  fi
  if [ "${ACCEPTANCE_LAB_SKIP_REPOSYNC_BUILD:-}" != "1" ] && [ "$scope" != "fixtures" ]; then
    record_rust_image_identity
    build_one "$REPOSYNC_IMAGE" "$LAB_DIR/docker/Dockerfile.reposync" "$REPO_ROOT" \
      --build-context "labfiles=${LAB_DIR}/docker" \
      --build-arg "DEBIAN_IMAGE=${DEBIAN_IMAGE}" \
      --build-arg "RUST_IMAGE=${RUST_IMAGE}" \
      --build-arg "NODE_IMAGE=${NODE_IMAGE}" \
      --build-arg "RUST_REPO_DIGEST=${RUST_REPO_DIGEST}"
  elif [ "${ACCEPTANCE_LAB_SKIP_REPOSYNC_BUILD:-}" = "1" ]; then
    docker image inspect "$REPOSYNC_IMAGE" >/dev/null
  fi
  if [ "$scope" != "fixtures" ]; then
    extract_build_graph
  fi
  record_images
}

record_images() {
  local out meta
  if [ -n "${EVIDENCE:-}" ]; then
    out="$EVIDENCE/images.txt"
  else
    out="$ARTIFACT_ROOT/image-prepare-${CANDIDATE}.txt"
  fi
  mkdir -p "$(dirname "$out")"
  {
    echo "time=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "candidate=${CANDIDATE}"
    docker version --format 'docker={{.Server.Version}}' 2>/dev/null || true
    docker compose version 2>/dev/null || true
    local image
    for image in "$DEBIAN_IMAGE" "$RUST_IMAGE" "$NODE_IMAGE" "$GITEA_IMAGE" "$LDAP_IMAGE" \
      "${SVN_IMAGE:-}" "${SVNSERVE_IMAGE:-}" "${GITEA_LAB_IMAGE:-}" "${GITHTTP_IMAGE:-}" \
      "${PYENV_IMAGE:-}" "${OBSERVER_IMAGE:-}" "${FORWARDER_IMAGE:-}" "${REPOSYNC_IMAGE:-}"; do
      [ -n "$image" ] || continue
      if docker image inspect "$image" >/dev/null 2>&1; then
        meta="$(inspect_image_metadata "$image")" || return 1
        printf '%s %s\n' "$image" "$meta"
      else
        echo "$image absent"
      fi
    done
  } >> "$out"
}

write_resources() {
  mkdir -p "$EVIDENCE"
  {
    echo "time=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "documented_github_ubuntu=4 vCPU, 16 GB RAM, 14 GB SSD"
    echo "cargo_build_jobs=2"
    echo "browser_workers=1"
    if command -v nproc >/dev/null 2>&1; then
      echo "nproc=$(nproc)"
    fi
    if [ -r /proc/meminfo ]; then
      grep -E '^(MemTotal|MemAvailable):' /proc/meminfo || true
    fi
    df -Pk "$ARTIFACT_ROOT" || true
    if command -v docker >/dev/null 2>&1; then
      docker system df || true
      local ids
      ids="$(docker ps -q --filter "label=acceptance-lab.run-id=${RUN_ID:-none}" || true)"
      if [ -n "$ids" ]; then
        # shellcheck disable=SC2086
        docker stats --no-stream --format '{{.Name}} mem={{.MemUsage}} cpu={{.CPUPerc}}' $ids || true
      fi
    fi
    echo "---"
  } >> "$EVIDENCE/resources.txt"
}

capture_service_logs() {
  local service
  mkdir -p "$RUN_DIR/logs"
  for service in "$@"; do
    compose logs --no-color --tail 120 "$service" > "$RUN_DIR/logs/${service}.log" 2>&1 || true
  done
  if [ -f "$RUN_DIR/secret-values.txt" ]; then
    node "$LAB_DIR/src/scrub.mjs" --root "$RUN_DIR/logs" --secrets-file "$RUN_DIR/secret-values.txt" >/dev/null || true
  fi
}

probe_published_ui() {
  mkdir -p "$EVIDENCE"
  local fid rid
  fid="$(compose --profile app ps -q ui-forwarder || true)"
  rid="$(compose --profile app ps -q reposync || true)"
  {
    echo "time=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "ui_port=${LAB_UI_PORT}"
    if [ -n "$fid" ]; then
      echo "forwarder=$fid"
      docker port "$fid" || true
    else
      echo "forwarder=absent"
    fi
    if [ -n "$rid" ]; then
      echo "reposync=$rid"
      docker port "$rid" || true
    else
      echo "reposync=absent"
    fi
  } > "$EVIDENCE/ui-port.txt"
  if curl --fail --silent --max-time 5 "http://127.0.0.1:${LAB_UI_PORT}/api/status/health" >/dev/null; then
    echo "host_health=ok" >> "$EVIDENCE/ui-port.txt"
    return 0
  fi
  echo "host_health=fail" >> "$EVIDENCE/ui-port.txt"
  return 1
}

up_stack() {
  require_docker || return 1
  local started
  started="$(date +%s)"
  write_resources || true
  log "starting services except reposync"
  # This function is called from `if`, where set -e does not stop the body.
  # Each step returns on its own status.
  if ! compose up -d --wait ldap svn svnserve gitea git-http denied notify fault observer; then
    capture_service_logs ldap svn gitea || true
    return 1
  fi
  if ! seed_gitea; then
    capture_service_logs gitea ldap || true
    return 1
  fi
  log "starting reposync"
  if ! compose --profile app up -d --wait reposync ui-forwarder; then
    capture_service_logs reposync gitea ldap svn ui-forwarder || true
    return 1
  fi
  if ! probe_published_ui; then
    capture_service_logs reposync gitea ldap svn ui-forwarder || true
    return 1
  fi
  write_resources || true
  log "up finished in $(( $(date +%s) - started ))s"
}

verify_project_resources() {
  local id label disp vol net
  local ids vols nets
  ids="$(docker ps -aq --filter "label=com.docker.compose.project=${PROJECT}" || true)"
  for id in $ids; do
    label="$(docker inspect --format '{{index .Config.Labels "acceptance-lab.run-id"}}' "$id")"
    disp="$(docker inspect --format '{{index .Config.Labels "acceptance-lab.disposable"}}' "$id")"
    if [ "$label" != "$RUN_ID" ] || [ "$disp" != "true" ]; then
      echo "refusing container $id: run=$label disposable=$disp" >&2
      return 1
    fi
  done
  vols="$(docker volume ls -q --filter "label=com.docker.compose.project=${PROJECT}" || true)"
  for vol in $vols; do
    label="$(docker volume inspect --format '{{index .Labels "acceptance-lab.run-id"}}' "$vol")"
    disp="$(docker volume inspect --format '{{index .Labels "acceptance-lab.disposable"}}' "$vol")"
    if [ "$label" != "$RUN_ID" ] || [ "$disp" != "true" ]; then
      echo "refusing volume $vol: run=$label disposable=$disp" >&2
      return 1
    fi
  done
  nets="$(docker network ls -q --filter "label=com.docker.compose.project=${PROJECT}" || true)"
  for net in $nets; do
    label="$(docker network inspect --format '{{index .Labels "acceptance-lab.run-id"}}' "$net")"
    disp="$(docker network inspect --format '{{index .Labels "acceptance-lab.disposable"}}' "$net")"
    if [ "$label" != "$RUN_ID" ] || [ "$disp" != "true" ]; then
      echo "refusing network $net: run=$label disposable=$disp" >&2
      return 1
    fi
  done
}

cleanup_run() {
  require_docker || return 1
  verify_project_resources || return 1
  # reposync is behind the app profile. down without that profile left its
  # container and volume on run 38188946403.
  compose --profile app down -v --remove-orphans || return 1
  local left
  left="$(docker ps -aq --filter "label=acceptance-lab.run-id=${RUN_ID}" || true)"
  if [ -n "$left" ]; then
    echo "containers remain for $RUN_ID" >&2
    docker ps -a --filter "label=acceptance-lab.run-id=${RUN_ID}" --format '{{.Names}} {{.Status}}' >&2 || true
    return 1
  fi
  left="$(docker volume ls -q --filter "label=acceptance-lab.run-id=${RUN_ID}" || true)"
  if [ -n "$left" ]; then
    echo "volumes remain for $RUN_ID" >&2
    return 1
  fi
  if [ "${ACCEPTANCE_LAB_REMOVE_IMAGES:-}" = "1" ]; then
    local tag
    for tag in $(docker images --format '{{.Repository}}:{{.Tag}}' || true); do
      case "$tag" in
        *"$RUN_ID"*) docker rmi "$tag" || return 1 ;;
      esac
    done
  fi
  log "cleaned run $RUN_ID"
}

export_lab_env() {
  set -a
  # shellcheck disable=SC1090
  . "$RUN_DIR/secrets.env"
  set +a
  export LAB_UI_URL="http://127.0.0.1:${LAB_UI_PORT}"
  export LAB_COMPOSE_PROJECT="$PROJECT"
  export LAB_COMPOSE_FILE="$COMPOSE_FILE"
  export LAB_SUITES="$SUITE"
  export LAB_WORKERS=1
  export LAB_PLAYWRIGHT_JSON="$EVIDENCE/playwright.json"
  export LAB_PLAYWRIGHT_OUTPUT="$EVIDENCE/playwright-output"
  export LAB_ADMIN_USER=admin
  export LAB_ADMIN_PASSWORD="$REPOSYNC_ADMIN_PASSWORD"
  export GITEA_ADMIN_USER=labadmin
  export LAB_WEBHOOK_SECRET="$REPOSYNC_WEBHOOK_SECRET"
  export LAB_SVN_WEBHOOK_SECRET="$REPOSYNC_SVN_WEBHOOK_SECRET"
  export LAB_FAULT_CONTROL_URL="http://127.0.0.1:${LAB_FAULT_CONTROL_PORT}"
  export LAB_RUN_DIR LAB_SVN_PORT LAB_SVNSERVE_PORT LAB_GITEA_PORT LAB_GITHTTP_PORT
  export LAB_FAULT_PORT LAB_FAULT_CONTROL_PORT LAB_UI_PORT RUN_ID COMPOSE_PROJECT_NAME
  export LDAP_IMAGE DEBIAN_IMAGE RUST_IMAGE NODE_IMAGE GITEA_IMAGE SVN_IMAGE SVNSERVE_IMAGE
  export GITEA_LAB_IMAGE GITHTTP_IMAGE PYENV_IMAGE OBSERVER_IMAGE FORWARDER_IMAGE REPOSYNC_IMAGE
}

write_synthetic_playwright() {
  local reason="$1"
  python3 - "$EVIDENCE/playwright.json" "$reason" <<'PY'
import json, sys
reason = sys.argv[2]
report = {
    "suites": [{
        "title": "startup",
        "specs": [{
            "title": "LAB_SMOKE_STACK",
            "annotations": [{"type": "labCase", "description": "LAB_SMOKE_STACK"}],
            "tests": [{
                "annotations": [
                    {"type": "labCase", "description": "LAB_SMOKE_STACK"},
                    {"type": "labStatus", "description": "FAIL"},
                    {"type": "labQualification", "description": "UNQUALIFIED"},
                    {"type": "labReason", "description": reason[:500]},
                ],
                "results": [{"status": "failed", "retry": 0, "error": {"message": reason[:500]}}],
            }],
        }],
    }],
}
open(sys.argv[1], "w").write(json.dumps(report, indent=2) + "\n")
PY
}

live_ref() {
  local ref="$1" sha="" url="https://github.com/chriscase/RepoSync.git"
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    sha="$(git -c "http.extraHeader=AUTHORIZATION: bearer ${GITHUB_TOKEN}" ls-remote "$url" "$ref" 2>/dev/null | awk 'NR==1 { print $1 }' || true)"
  fi
  if ! printf '%s' "$sha" | grep -Eq '^[0-9a-f]{40}$'; then
    sha="$(git ls-remote "$url" "$ref" 2>/dev/null | awk 'NR==1 { print $1 }' || true)"
  fi
  if printf '%s' "$sha" | grep -Eq '^[0-9a-f]{40}$'; then
    printf '%s' "$sha"
  else
    printf '%s' "NOT_ESTABLISHED"
  fi
}

interrupt_cleanup() {
  local sig="$1"
  if [ "$CLEANING" = "1" ]; then
    exit 130
  fi
  CLEANING=1
  echo "acceptance-lab received ${sig}; cleaning owned run ${RUN_ID:-unset}" >&2
  if [ -n "${RUN_ID:-}" ] && [ -n "${RUN_DIR:-}" ] && [ -f "${RUN_DIR}/run.json" ]; then
    cleanup_run || echo "cleanup after ${sig} failed" >&2
  fi
  exit 130
}

write_identity() {
  local inspected catalog_sha script_sha live_main live_pr194 live_pr195
  inspected="$(python3 - "$LAB_DIR/catalog/operations.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1])).get("inspected_sha", ""))
PY
)"
  catalog_sha="$(file_sha256 "$LAB_DIR/catalog/operation-catalog.json")"
  script_sha="$(file_sha256 "$REPO_ROOT/scripts/acceptance-lab.sh")"
  live_main="$(live_ref refs/heads/main)"
  live_pr194="$(live_ref refs/heads/codex/pair-refresh-20261011)"
  live_pr195="$(live_ref refs/heads/grok/reposync-acceptance-lab-20261011)"
  python3 - "$EVIDENCE/identity.json" "$CANDIDATE" "$HEAD_SHA" "$inspected" "$catalog_sha" \
    "$script_sha" "$RUN_ID" "$SEED" "$SUITE" "$RECORDED_LIVE_MAIN" "$RECORDED_LIVE_MAIN_PARENT" \
    "$RECORDED_PR194_HEAD" "${LAB_PR_HEAD_SHA:-}" "${LAB_GITHUB_SHA:-}" "${LAB_GITHUB_EVENT:-}" \
    "$live_main" "$live_pr194" "$live_pr195" <<'PY'
import json, sys
(candidate, head, inspected, catalog_sha, script_sha, run_id, seed, suite,
 live_main, live_parent, pr194, pr_head, github_sha, event,
 fetched_main, fetched_pr194, fetched_pr195) = sys.argv[2:19]
covers = bool(candidate) and candidate == inspected
note = (
    "The frozen audit inspected %s. Candidate %s is the checkout HEAD. "
    "The authored snapshot of main is %s (parent %s) and of PR 194 is %s. "
    "This run fetched main %s, PR 194 %s, and PR 195 %s. "
    "A fetched value of NOT_ESTABLISHED was not observed. "
    "A pull_request runner checks out a merge commit, which can include product changes the audit did not inspect."
) % (inspected, candidate, live_main, live_parent, pr194, fetched_main, fetched_pr194, fetched_pr195)
payload = {
    "candidate_sha": candidate,
    "head_sha": head,
    "candidate_matches_head": candidate == head,
    "audit_inspected_sha": inspected,
    "frozen_catalog_sha256": catalog_sha,
    "launcher_sha256": script_sha,
    "audit_covers_candidate": covers,
    "recorded_live_main_sha": live_main,
    "recorded_live_main_parent": live_parent,
    "recorded_pr194_head": pr194,
    "live_main_sha": fetched_main,
    "live_pr194_head": fetched_pr194,
    "live_pr195_head": fetched_pr195,
    "pr_head_sha": pr_head,
    "github_sha": github_sha,
    "github_event": event,
    "run_id": run_id,
    "seed": int(seed) if seed else None,
    "suite": suite,
    "runtime_qualification": "NOT_RUN",
    "note": note + " runtime_qualification stays NOT_RUN until report.json exists; that report is the runtime result.",
}
open(sys.argv[1], "w").write(json.dumps(payload, indent=2) + "\n")
PY
}

stamp_runtime_result() {
  if [ ! -f "$EVIDENCE/identity.json" ] || [ ! -f "$EVIDENCE/report.json" ]; then
    return 0
  fi
  python3 - "$EVIDENCE/identity.json" "$EVIDENCE/report.json" <<'PY'
import json, pathlib, sys
ident_path, report_path = map(pathlib.Path, sys.argv[1:])
ident = json.loads(ident_path.read_text())
report = json.loads(report_path.read_text())
ident["runtime_qualification"] = report.get("suite_result") or "NOT_RUN"
ident["full_qualification"] = report.get("full_qualification") or "NOT_RUN"
ident_path.write_text(json.dumps(ident, indent=2) + "\n")
PY
}

run_self_test() {
  mkdir -p "$EVIDENCE"
  node "$LAB_DIR/src/self-test.mjs" "$EVIDENCE"
}

run_playwright() {
  if [ ! -x "$LAB_DIR/node_modules/.bin/playwright" ]; then
    write_synthetic_playwright "playwright package is not installed under tests/acceptance-lab"
    return 0
  fi
  export_lab_env
  local ec
  set +e
  npm --prefix "$LAB_DIR" exec -- playwright test --config "$LAB_DIR/playwright.config.ts" --retries=0
  ec=$?
  set -e
  log "playwright exit $ec"
  if [ ! -s "$EVIDENCE/playwright.json" ]; then
    write_synthetic_playwright "playwright exited $ec without a report"
  fi
}

run_engine() {
  local summary ec
  log "starting real-engine suite without the lab compose stack"
  set +e
  "$REPO_ROOT/scripts/real-engine-scenario-suite.sh"
  ec=$?
  set -e
  log "real-engine exit $ec"
  printf 'engine_exit=%s\n' "$ec" > "$EVIDENCE/engine-exit.txt"
  summary="$(find "$REPO_ROOT/artifacts/real-engine-scenarios" -name summary.json -type f 2>/dev/null | sort | tail -n 1 || true)"
  if [ -n "$summary" ]; then
    cp "$summary" "$EVIDENCE/engine-summary.json"
  else
    log "real-engine suite did not write summary.json"
  fi
}

run_aggregate() {
  local args
  mkdir -p "$EVIDENCE"
  args=(
    "$LAB_DIR/src/aggregate.mjs"
    --catalog "$LAB_DIR/catalog/operations.json"
    --matrix "$REPO_ROOT/docs/reliability/acceptance-matrix.json"
    --rules "$LAB_DIR/catalog/wiring-rules.json"
    --audit-dir "$LAB_DIR/catalog"
    --out "$EVIDENCE/report.json"
    --markdown "$EVIDENCE/report.md"
    --suite "$SUITE"
    --candidate "$CANDIDATE"
    --seed "$SEED"
    --run-id "$RUN_ID"
  )
  if [ -s "$EVIDENCE/playwright.json" ]; then
    args+=(--playwright "$EVIDENCE/playwright.json")
  fi
  if [ -s "$EVIDENCE/self-test.json" ]; then
    args+=(--self-test "$EVIDENCE/self-test.json")
  fi
  if [ -s "$EVIDENCE/engine-summary.json" ]; then
    args+=(--engine "$EVIDENCE/engine-summary.json")
  fi
  set +e
  node "${args[@]}"
  local ec=$?
  set -e
  log "aggregate exit $ec"
  return "$ec"
}

scrub_outputs() {
  refresh_secret_values
  node "$LAB_DIR/src/scrub.mjs" --root "$EVIDENCE" --secrets-file "$RUN_DIR/secret-values.txt" >/dev/null || true
  if [ -d "$RUN_DIR/logs" ]; then
    node "$LAB_DIR/src/scrub.mjs" --root "$RUN_DIR/logs" --secrets-file "$RUN_DIR/secret-values.txt" >/dev/null || true
  fi
}

scan_outputs() {
  refresh_secret_values
  set +e
  node "$LAB_DIR/src/scan.mjs" --root "$EVIDENCE" --secrets-file "$RUN_DIR/secret-values.txt" --out "$RUN_DIR/scan.json"
  local ec=$?
  set -e
  if [ "$ec" -ne 0 ]; then
    log "evidence scan failed"
    return 1
  fi
}

publish_outputs() {
  local publish="$RUN_DIR/publish"
  rm -rf "$publish"
  mkdir -p "$publish"
  local name
  for name in report.json report.md self-test.json identity.json resources.txt timings.txt playwright.json engine-summary.json engine-exit.txt; do
    if [ -f "$EVIDENCE/$name" ]; then
      cp "$EVIDENCE/$name" "$publish/$name"
    fi
  done
  if [ -f "$RUN_DIR/scan.json" ]; then
    cp "$RUN_DIR/scan.json" "$publish/scan.json"
  fi
  if [ -f "$RUN_DIR/logs/seed.log" ]; then
    cp "$RUN_DIR/logs/seed.log" "$publish/seed.log"
  fi
  if [ -f "$EVIDENCE/ui-port.txt" ]; then
    cp "$EVIDENCE/ui-port.txt" "$publish/ui-port.txt"
  fi
  local service
  for service in ldap gitea svn reposync ui-forwarder; do
    if [ -s "$RUN_DIR/logs/${service}.log" ]; then
      cp "$RUN_DIR/logs/${service}.log" "$publish/${service}.log"
    fi
  done
  if [ -d "$EVIDENCE/build-graph" ]; then
    mkdir -p "$publish/build-graph"
    cp -R "$EVIDENCE/build-graph/." "$publish/build-graph/"
  fi
  if [ -f "$RUN_DIR/scan.json" ] && python3 - "$RUN_DIR/scan.json" <<'PY'
import json, sys
sys.exit(0 if json.load(open(sys.argv[1])).get("ok") else 1)
PY
  then
    mkdir -p "$publish/evidence"
    cp -R "$EVIDENCE/." "$publish/evidence/"
  fi
  set +e
  node "$LAB_DIR/src/scan.mjs" --root "$publish" --secrets-file "$RUN_DIR/secret-values.txt" --out "$RUN_DIR/publish-scan.json"
  local ec=$?
  set -e
  if [ "$ec" -ne 0 ]; then
    find "$publish" -type f -delete
    printf '%s\n' "withheld: sanitized publish scan failed; evidence was not copied for upload" > "$publish/WITHHELD.txt"
    if [ -f "$RUN_DIR/publish-scan.json" ]; then
      cp "$RUN_DIR/publish-scan.json" "$publish/scan.json"
    fi
    log "publish withheld after scan failure"
    return 1
  fi
  if [ -f "$RUN_DIR/logs/launcher.log" ]; then
    tail -n 40 "$RUN_DIR/logs/launcher.log" >&2 || true
  fi
}

cmd_prepare() {
  isolate_host_credentials
  resolve_candidate
  load_pins
  prepare_images
}

cmd_up() {
  isolate_host_credentials
  resolve_candidate
  ensure_layout
  prepare_images
  up_stack
}

cmd_down() {
  load_run
  cleanup_run
}

cmd_clean() {
  load_run
  cleanup_run
}

cmd_reset() {
  isolate_host_credentials
  load_run
  resolve_candidate
  load_pins
  cleanup_run
  clear_token
  up_stack
}

cmd_report() {
  load_run
  resolve_candidate
  write_identity
  local ec=0
  run_aggregate || ec=$?
  stamp_runtime_result || true
  return "$ec"
}

cmd_test() {
  local ec started pre
  ec=0
  started="$(date +%s)"
  isolate_host_credentials
  resolve_candidate
  pre="$(mktemp -d "${TMPDIR:-/tmp}/acceptance-lab-self.XXXXXX")"
  if ! node "$LAB_DIR/src/self-test.mjs" "$pre"; then
    echo "harness self-test failed; Docker was not started" >&2
    ec=1
  fi
  ensure_layout
  mkdir -p "$EVIDENCE"
  if [ -f "$pre/self-test.json" ]; then
    cp "$pre/self-test.json" "$EVIDENCE/self-test.json"
  fi
  rm -rf "$pre"
  write_identity
  printf 'start=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$EVIDENCE/timings.txt"
  if [ "$ec" -eq 0 ]; then
    if ! prepare_images; then
      ec=1
      write_synthetic_playwright "image prepare failed"
    fi
  fi
  if [ "$ec" -eq 0 ]; then
    if ! up_stack; then
      ec=1
      write_synthetic_playwright "stack did not become healthy"
    fi
  fi
  if [ "$ec" -eq 0 ]; then
    run_playwright || true
    if [ "$SUITE" != "smoke" ] && [ "${ACCEPTANCE_LAB_SKIP_ENGINE:-}" != "1" ]; then
      run_engine || true
    fi
  fi
  printf 'elapsed_before_report=%s\n' "$(( $(date +%s) - started ))" >> "$EVIDENCE/timings.txt"
  capture_service_logs ldap gitea svn reposync ui-forwarder || true
  scrub_outputs || true
  if ! scan_outputs; then
    ec=1
  fi
  if ! run_aggregate; then
    ec=1
  fi
  stamp_runtime_result || true
  scrub_outputs || true
  if ! publish_outputs; then
    ec=1
  fi
  if [ "$KEEP" != "1" ]; then
    CLEANING=1
    if ! cleanup_run; then
      ec=1
      log "cleanup failed"
    fi
    CLEANING=0
  fi
  return "$ec"
}

main() {
  parse_args "$@"
  trap 'interrupt_cleanup INT' INT
  trap 'interrupt_cleanup TERM' TERM
  artifact_root
  require_tools
  case "$CMD" in
    prepare) cmd_prepare ;;
    up) cmd_up ;;
    down) cmd_down ;;
    reset) cmd_reset ;;
    report) cmd_report ;;
    test) cmd_test ;;
    clean) cmd_clean ;;
  esac
}

main "$@"
