#!/usr/bin/env bash
# Native Linux qualification for the #118 archive. Synthetic prefix only.
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${ROOT}/target/release"
OUT="${ROOT}/target/reposync-candidate-x86_64-unknown-linux-gnu.tar.gz"

: >/tmp/rs118-inner-exits.txt
echo "QUALIFY_ARGV bash scripts/qualify_release_artifact.sh"
echo "HOST $(uname -s) $(uname -m)"
echo "NOT_RUN docker"
echo "NOT_RUN aarch64-unknown-linux-gnu"
echo "NOT_RUN x86_64-apple-darwin"
echo "NOT_RUN aarch64-apple-darwin"
echo "NARROW this host builds only x86_64-unknown-linux-gnu."
echo "NARROW Docker image build is not run: no docker daemon, and Dockerfile FROM rust:1.83 cannot parse the locked edition2024 crates."

python3 "${ROOT}/scripts/check_docker_context.py"

fail() {
    echo "FAIL $*" >&2
    exit 1
}

host_unit="/etc/systemd/system/reposync.service"
if [[ -e "$host_unit" ]]; then
    host_unit_before="$(stat -c '%F %s %Y' "$host_unit")"
else
    host_unit_before="ABSENT"
fi
if [[ -e /etc/reposync ]]; then
    etc_before="$(stat -c '%F %s %Y' /etc/reposync)"
else
    etc_before="ABSENT"
fi

# Assembler output is checked with sha256sum -c from the artifact directory.
verified_digest() {
    local archive="$1"
    local label="$2"
    local dir base code
    dir="$(cd -- "$(dirname -- "$archive")" && pwd)"
    base="$(basename -- "$archive")"
    set +e
    (
        cd "$dir"
        sha256sum -c "${base}.sha256"
    ) >/tmp/rs118-${label}.out 2>/tmp/rs118-${label}.err
    code=$?
    set -e
    printf 'INNER_EXIT %s %s\n' "$label" "$code" | tee -a /tmp/rs118-inner-exits.txt >&2
    [[ "$code" -eq 0 ]] || fail "${label} rejected the checksum sidecar"
    awk 'NR==1 { print $1 }' "${dir}/${base}.sha256"
}

stub_dir="$(mktemp -d)"
printf '#!/bin/sh\nprintf "reposync stub\\n"\nexit 0\n' > "${stub_dir}/reposync"
printf '#!/bin/sh\nprintf "reposync-daemon stub\\n"\nexit 0\n' > "${stub_dir}/reposync-daemon"
chmod 755 "${stub_dir}/reposync" "${stub_dir}/reposync-daemon"
stub_archive="$(mktemp)"
bash "${ROOT}/scripts/assemble_release_archive.sh" --bin-dir "$stub_dir" --out "$stub_archive"
stub_sha="$(verified_digest "$stub_archive" sha256sum_c_stub)"

sentinel="$(mktemp)"
printf 'external-sentinel\n' > "$sentinel"

state_of() {
    local prefix="$1"
    {
        find "$prefix" -printf '%P %y %s\n' | sort
        sha256sum "$sentinel"
    } | sha256sum | awk '{print $1}'
}

write_variant() {
    python3 - "$1" "$stub_archive" "$2" <<'PY'
import io
import sys
import tarfile

kind, source, dest = sys.argv[1:]

def load():
    items = []
    with tarfile.open(source, "r:gz") as handle:
        for member in handle.getmembers():
            data = handle.extractfile(member).read() if member.isreg() else b""
            items.append((member.name, data))
    return items

def add_file(out, name, data):
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = 0o644
    info.type = tarfile.REGTYPE
    out.addfile(info, io.BytesIO(data))

items = load()
with tarfile.open(dest, "w:gz") as out:
    if kind == "missing":
        for name, data in items:
            if name != "authors.example.toml":
                add_file(out, name, data)
    elif kind == "type":
        for name, data in items:
            if name == "authors.example.toml":
                info = tarfile.TarInfo(name)
                info.type = tarfile.DIRTYPE
                info.mode = 0o755
                out.addfile(info)
            else:
                add_file(out, name, data)
    elif kind == "symlink":
        for name, data in items:
            if name == "reposync":
                info = tarfile.TarInfo(name)
                info.type = tarfile.SYMTYPE
                info.linkname = "reposync-daemon"
                out.addfile(info)
            else:
                add_file(out, name, data)
    elif kind == "hardlink":
        for name, data in items:
            if name == "reposync":
                info = tarfile.TarInfo(name)
                info.type = tarfile.LNKTYPE
                info.linkname = "reposync-daemon"
                out.addfile(info)
            else:
                add_file(out, name, data)
    elif kind == "duplicate":
        for name, data in items:
            add_file(out, name, data)
            if name == "config.example.toml":
                add_file(out, name, data)
    elif kind == "extra":
        for name, data in items:
            add_file(out, name, data)
        add_file(out, "notes.txt", b"nope\n")
    elif kind == "traversal":
        for name, data in items:
            add_file(out, name, data)
        add_file(out, "../escaped", b"nope\n")
    else:
        raise SystemExit(f"unknown variant {kind}")
PY
}

refuse() {
    local label="$1"
    local expect="$2"
    local archive prefix digest before after code
    archive="$(mktemp)"
    write_variant "$label" "$archive"
    prefix="$(mktemp -d)"
    printf 'prefix-keep\n' > "${prefix}/KEEP"
    case "$sentinel" in
        "$prefix"|"$prefix"/*) fail "sentinel is inside the ${label} prefix" ;;
    esac
    digest="$(sha256sum "$archive" | awk '{print $1}')"
    before="$(state_of "$prefix")"
    set +e
    bash "${ROOT}/scripts/install.sh" --from-archive \
        --archive "$archive" --sha256 "$digest" --prefix "$prefix" \
        >/tmp/rs118-${label}.out 2>/tmp/rs118-${label}.err
    code=$?
    set -e
    printf 'INNER_EXIT %s %s\n' "$label" "$code" | tee -a /tmp/rs118-inner-exits.txt
    after="$(state_of "$prefix")"
    [[ "$code" -ne 0 ]] || fail "${label} was accepted"
    [[ "$before" == "$after" ]] || fail "${label} changed the prefix or external sentinel"
    grep -F -q "$expect" /tmp/rs118-${label}.err || {
        cat /tmp/rs118-${label}.err >&2
        fail "${label} did not report ${expect}"
    }
    rm -rf "$prefix" "$archive"
}

# Corrupt digest is rejected before any prefix exists.
bad_prefix="$(mktemp -d)"
rmdir "$bad_prefix"
set +e
bash "${ROOT}/scripts/install.sh" --from-archive --archive "$stub_archive" --sha256 "$(printf '0%.0s' {1..64})" --prefix "$bad_prefix" >/tmp/rs118-bad-digest.out 2>/tmp/rs118-bad-digest.err
bad_digest=$?
set -e
printf 'INNER_EXIT bad_digest %s\n' "$bad_digest" | tee -a /tmp/rs118-inner-exits.txt
[[ "$bad_digest" -ne 0 ]] || fail "corrupt digest was accepted"
[[ ! -d "$bad_prefix" ]] || fail "corrupt digest mutated the prefix"

refuse missing "missing required member"
refuse type "not a regular file"
refuse symlink "is a symlink"
refuse hardlink "is a hard link"
refuse duplicate "duplicate archive member"
refuse extra "extra member"
refuse traversal "not allowed"

python3 - "$stub_archive" "${ROOT}/web-ui/dist" "${ROOT}/packaging/static/assets/app.js" <<'PY'
import sys
import tarfile
from pathlib import Path

archive, dist_root, fixture = sys.argv[1:]
dist = Path(dist_root)
sentinel = 'window.reposyncStatic = "synthetic"'
fixture_text = Path(fixture).read_text()
if sentinel not in fixture_text:
    raise SystemExit("synthetic fixture lost its sentinel")
product = {}
with tarfile.open(archive, "r:gz") as handle:
    for member in handle.getmembers():
        if not member.name.startswith("static/"):
            continue
        product[member.name[len("static/"):]] = handle.extractfile(member).read()
expected = {
    path.relative_to(dist).as_posix(): path.read_bytes()
    for path in dist.rglob("*")
    if path.is_file() and path.suffix != ".map"
}
if set(product) != set(expected):
    raise SystemExit("product static names differ from the web-ui build")
for name, data in expected.items():
    if product[name] != data:
        raise SystemExit(f"product bytes differ from the web-ui build: {name}")
    if sentinel.encode() in data:
        raise SystemExit(f"synthetic sentinel shipped in {name}")
print("PRODUCT_UI_SEPARATE_FROM_SYNTHETIC_FIXTURE")
PY

# Default/direct-root paths create parents and place the unit under a temp root.
default_root="$(mktemp -d)"
REPOSYNC_INSTALL_DIR="${default_root}/usr/local/bin" \
REPOSYNC_CONFIG_DIR="${default_root}/etc/reposync" \
REPOSYNC_DATA_DIR="${default_root}/var/lib/reposync" \
REPOSYNC_SYSTEMD_UNIT_DIR="${default_root}/etc/systemd/system" \
    bash "${ROOT}/scripts/install.sh" --from-archive --archive "$stub_archive" --sha256 "$stub_sha"
[[ -f "${default_root}/etc/reposync/config.toml" ]] || fail "default config was not installed"
[[ -x "${default_root}/usr/local/bin/reposync" ]] || fail "default binary parent was not created"
[[ -f "${default_root}/etc/systemd/system/reposync.service" ]] || fail "default install did not place the unit"
[[ -f "${default_root}/usr/local/bin/static/assets/index-CMTDqfvH.js" ]] || fail "default install missed the product asset"
rm -rf "$default_root"

# The same default destination as uid 0, still confined to a temp root.
root_root="$(mktemp -d)"
set +e
sudo env \
    REPOSYNC_INSTALL_DIR="${root_root}/usr/local/bin" \
    REPOSYNC_CONFIG_DIR="${root_root}/etc/reposync" \
    REPOSYNC_DATA_DIR="${root_root}/var/lib/reposync" \
    REPOSYNC_SYSTEMD_UNIT_DIR="${root_root}/etc/systemd/system" \
    bash "${ROOT}/scripts/install.sh" --from-archive --archive "$stub_archive" --sha256 "$stub_sha"
root_code=$?
set -e
printf 'INNER_EXIT direct_root %s\n' "$root_code" | tee -a /tmp/rs118-inner-exits.txt
[[ "$root_code" -eq 0 ]] || fail "direct-root temp install failed"
[[ -f "${root_root}/etc/systemd/system/reposync.service" ]] || fail "direct-root install did not place the unit"
sudo rm -rf "$root_root"

prefix="$(mktemp -d)"
unit_trap="$(mktemp -d)"
mkdir -p "${prefix}/etc/reposync" "${prefix}/var/lib/reposync" "${prefix}/bin"
printf 'kept-config\n' > "${prefix}/etc/reposync/config.toml"
printf 'kept-secret\n' > "${prefix}/etc/reposync/env"
printf 'kept-data\n' > "${prefix}/var/lib/reposync/reposync.db"
printf '#!/bin/sh\nexit 9\n' > "${prefix}/bin/reposync"
chmod 755 "${prefix}/bin/reposync"
cp "${prefix}/bin/reposync" "${prefix}/bin/reposync-daemon"

REPOSYNC_SYSTEMD_UNIT_DIR="${unit_trap}" \
    bash "${ROOT}/scripts/install.sh" --from-archive --archive "$stub_archive" --sha256 "$stub_sha" --prefix "$prefix"
[[ "$(cat "${prefix}/etc/reposync/config.toml")" == "kept-config" ]] || fail "config was replaced"
[[ "$(cat "${prefix}/etc/reposync/env")" == "kept-secret" ]] || fail "key file was replaced"
[[ "$(cat "${prefix}/var/lib/reposync/reposync.db")" == "kept-data" ]] || fail "data was replaced"
[[ -f "${prefix}/etc/reposync/reposync.service" ]] || fail "prefix install did not keep the unit under the prefix"
[[ -z "$(find "$unit_trap" -mindepth 1 -print -quit)" ]] || fail "prefix install wrote the systemd directory"
[[ -f "${prefix}/bin/static/index.html" && -f "${prefix}/bin/static/assets/index-CMTDqfvH.js" ]] || fail "static members missing"
grep -q "Welcome to RepoSync" "${prefix}/bin/static/assets/index-CMTDqfvH.js" || fail "installed asset is not the product bundle"
if grep -q 'window.reposyncStatic = "synthetic"' "${prefix}/bin/static/assets/index-CMTDqfvH.js"; then
    fail "installed asset is the synthetic fixture"
fi

echo "building native release binaries"
(cd "$ROOT" && rustup run stable cargo build --release --bin reposync --bin reposync-daemon)
bash "${ROOT}/scripts/assemble_release_archive.sh" --bin-dir "$BIN_DIR" --out "$OUT"
product_sha="$(verified_digest "$OUT" sha256sum_c_product)"
bash "${ROOT}/scripts/install.sh" --from-archive --archive "$OUT" --sha256 "$product_sha" --prefix "$prefix"
[[ "$(cat "${prefix}/etc/reposync/config.toml")" == "kept-config" ]] || fail "config changed during binary replacement"
[[ -f "${prefix}/etc/reposync/reposync.service" ]] || fail "prefix unit missing after binary replacement"
help_out="$("${prefix}/bin/reposync" --help)"
printf '%s\n' "$help_out" | grep -q "reposync" || fail "CLI help missing"

python3 - "$OUT" "${ROOT}/web-ui/dist" <<'PY'
import sys
import tarfile
from pathlib import Path

archive, dist_root = sys.argv[1:]
dist = Path(dist_root)
sentinel = b'window.reposyncStatic = "synthetic"'
with tarfile.open(archive, "r:gz") as handle:
    payload = {
        member.name[len("static/"):]: handle.extractfile(member).read()
        for member in handle.getmembers()
        if member.name.startswith("static/")
    }
expected = {
    path.relative_to(dist).as_posix(): path.read_bytes()
    for path in dist.rglob("*")
    if path.is_file() and path.suffix != ".map"
}
if payload != expected:
    raise SystemExit("release archive static bytes are not the web-ui build")
if any(sentinel in data for data in payload.values()):
    raise SystemExit("release archive contains the synthetic sentinel")
print("PRODUCT_ARCHIVE_MATCHES_WEB_UI_DIST")
PY

port=18080
runtime="${prefix}/etc/reposync/runtime.toml"
cat > "$runtime" <<EOF
[daemon]
poll_interval_secs = 3600
log_level = "error"
data_dir = "${prefix}/var/lib/reposync-runtime"

[svn]
url = "file:///tmp/reposync-118-no-such-repo"
username = "fixture"
password_env = "REPOSYNC_SVN_PASSWORD"
layout = "standard"

[github]
api_url = "https://example.invalid"
repo = "fixture/synthetic"
token_env = "REPOSYNC_GITHUB_TOKEN"
webhook_secret_env = "REPOSYNC_WEBHOOK_SECRET"
default_branch = "main"

[identity]
mapping_file = "${prefix}/etc/reposync/authors.toml"
email_domain = "example.invalid"

[web]
listen = "127.0.0.1:${port}"
session_secret_env = "REPOSYNC_SESSION_SECRET"
auth_mode = "simple"
admin_password_env = "REPOSYNC_ADMIN_PASSWORD"
EOF

export REPOSYNC_SVN_PASSWORD=synthetic
export REPOSYNC_GITHUB_TOKEN=synthetic
export REPOSYNC_WEBHOOK_SECRET=synthetic
export REPOSYNC_SESSION_SECRET=synthetic-session
export REPOSYNC_ADMIN_PASSWORD=synthetic-admin

log="${prefix}/daemon.log"
"${prefix}/bin/reposync-daemon" --config "$runtime" >"$log" 2>&1 &
daemon_pid=$!
trap 'kill "$daemon_pid" 2>/dev/null || true' EXIT

ready=0
for _ in $(seq 1 40); do
    if curl -fsS "http://127.0.0.1:${port}/api/status/health" >/tmp/rs118-health.out 2>/tmp/rs118-health.err; then
        ready=1
        break
    fi
    sleep 0.5
done
[[ "$ready" -eq 1 ]] || { echo "daemon did not serve health"; tail -50 "$log"; fail "daemon startup"; }
curl -fsS "http://127.0.0.1:${port}/" -o /tmp/rs118-index.html
grep -q "RepoSync Dashboard" /tmp/rs118-index.html || fail "product index missing"
asset="$(python3 - <<'PY'
import re
html = open("/tmp/rs118-index.html").read()
match = re.search(r'src="(/assets/[^"]+)"', html)
if not match:
    raise SystemExit("index has no product script")
print(match.group(1))
PY
)"
curl -fsS "http://127.0.0.1:${port}${asset}" -o /tmp/rs118-app.js
grep -q "Welcome to RepoSync" /tmp/rs118-app.js || fail "served asset is not the product bundle"
if grep -q 'window.reposyncStatic = "synthetic"' /tmp/rs118-app.js; then
    fail "served asset is the synthetic fixture"
fi
cmp /tmp/rs118-app.js "${ROOT}/web-ui/dist/assets/index-CMTDqfvH.js" || fail "served asset differs from the web-ui build"

if [[ -e "$host_unit" ]]; then
    [[ "$(stat -c '%F %s %Y' "$host_unit")" == "$host_unit_before" ]] || fail "host systemd unit changed"
else
    [[ "$host_unit_before" == "ABSENT" && ! -e "$host_unit" ]] || fail "host systemd unit was created"
fi
if [[ -e /etc/reposync ]]; then
    [[ "$(stat -c '%F %s %Y' /etc/reposync)" == "$etc_before" ]] || fail "live /etc/reposync changed"
else
    [[ "$etc_before" == "ABSENT" && ! -e /etc/reposync ]] || fail "live /etc/reposync was created"
fi

echo "INNER_EXITS_BEGIN"
cat /tmp/rs118-inner-exits.txt
echo "INNER_EXITS_END"
echo "QUALIFIED native x86_64-unknown-linux-gnu prefix=${prefix}"
kill "$daemon_pid"
wait "$daemon_pid" || true
trap - EXIT
rm -rf "$prefix" "$unit_trap" "$stub_dir"
echo "QUALIFY_EXIT 0"
