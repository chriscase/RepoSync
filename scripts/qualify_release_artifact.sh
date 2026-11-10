#!/usr/bin/env bash
# Native Linux qualification for the #118 archive. Synthetic prefix only.
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${ROOT}/target/release"
OUT="${ROOT}/target/reposync-candidate-x86_64-unknown-linux-gnu.tar.gz"

echo "HOST $(uname -s) $(uname -m)"
echo "NARROW this host builds only x86_64-unknown-linux-gnu."
echo "NARROW aarch64-unknown-linux-gnu, x86_64-apple-darwin, and aarch64-apple-darwin are not built here."
echo "NARROW Docker image build is not run: no docker daemon, and Dockerfile FROM rust:1.83 cannot parse the locked edition2024 crates."

python3 "${ROOT}/scripts/check_docker_context.py"

# Digest and missing-member checks use stub binaries so they do not depend
# on a release build. The native startup check below uses real binaries.
stub_dir="$(mktemp -d)"
printf '#!/bin/sh\nprintf "reposync stub\\n"\nexit 0\n' > "${stub_dir}/reposync"
printf '#!/bin/sh\nprintf "reposync-daemon stub\\n"\nexit 0\n' > "${stub_dir}/reposync-daemon"
chmod 755 "${stub_dir}/reposync" "${stub_dir}/reposync-daemon"
stub_archive="$(mktemp)"
bash "${ROOT}/scripts/assemble_release_archive.sh" --bin-dir "$stub_dir" --out "$stub_archive"
stub_sha="$(awk '{print $1}' "${stub_archive}.sha256")"

fail() {
    echo "FAIL $*" >&2
    exit 1
}

# Corrupt digest is rejected before any prefix exists.
bad_prefix="$(mktemp -d)"
rmdir "$bad_prefix"
set +e
bash "${ROOT}/scripts/install.sh" --from-archive --archive "$stub_archive" --sha256 "$(printf '0%.0s' {1..64})" --prefix "$bad_prefix" >/tmp/rs118-bad-digest.out 2>/tmp/rs118-bad-digest.err
bad_digest=$?
set -e
[[ "$bad_digest" -ne 0 ]] || fail "corrupt digest was accepted"
[[ ! -d "$bad_prefix" ]] || fail "corrupt digest mutated the prefix"

# Missing member is rejected before prefix mutation.
missing_stage="$(mktemp -d)"
tar -xzf "$stub_archive" -C "$missing_stage"
rm -f "${missing_stage}/authors.example.toml"
missing_archive="$(mktemp)"
tar -C "$missing_stage" -czf "$missing_archive" .
missing_sha="$(sha256sum "$missing_archive" | awk '{print $1}')"
missing_prefix="$(mktemp -d)"
rmdir "$missing_prefix"
set +e
bash "${ROOT}/scripts/install.sh" --from-archive --archive "$missing_archive" --sha256 "$missing_sha" --prefix "$missing_prefix" >/tmp/rs118-missing.out 2>/tmp/rs118-missing.err
missing_code=$?
set -e
[[ "$missing_code" -ne 0 ]] || fail "missing member was accepted"
[[ ! -d "$missing_prefix" ]] || fail "missing member mutated the prefix"
rm -rf "$missing_stage" "$missing_archive"

# authors.example.toml is a required archive member, not an extra file.
archive_names="$(tar -tzf "$stub_archive" | sed 's#^\./##' | sed '/^$/d' | sort)"
manifest_names="$(grep -v '^$' "${ROOT}/scripts/release-archive.manifest" | sort)"
[[ "$archive_names" == "$manifest_names" ]] || fail "archive members differ from the manifest"
grep -Fxq authors.example.toml <<<"$manifest_names" || fail "authors.example.toml is not in the manifest"

# Default paths (no --prefix) create the config directory before copying.
# The root stands in for / and /etc; the live /etc is not written.
default_root="$(mktemp -d)"
etc_before=""
if [[ -e /etc/reposync ]]; then
    etc_before="$(stat -c '%Y %s' /etc/reposync)"
fi
REPOSYNC_INSTALL_DIR="${default_root}/usr/local/bin" \
REPOSYNC_CONFIG_DIR="${default_root}/etc/reposync" \
REPOSYNC_DATA_DIR="${default_root}/var/lib/reposync" \
    bash "${ROOT}/scripts/install.sh" --from-archive --archive "$stub_archive" --sha256 "$stub_sha"
[[ -d "${default_root}/etc/reposync" ]] || fail "default config directory was not created"
[[ -f "${default_root}/etc/reposync/config.toml" ]] || fail "default config was not installed"
[[ -x "${default_root}/usr/local/bin/reposync" ]] || fail "default binary parent was not created"
if [[ -n "$etc_before" ]]; then
    [[ "$(stat -c '%Y %s' /etc/reposync)" == "$etc_before" ]] || fail "live /etc/reposync changed"
else
    [[ ! -e /etc/reposync ]] || fail "live /etc/reposync was created"
fi
rm -rf "$default_root"

# Extra members and path traversal are refused before the prefix changes.
hostile_prefix="$(mktemp -d)"
printf 'unchanged\n' > "${hostile_prefix}/KEEP"
python3 - "$stub_archive" "${hostile_prefix}/hostile.tar.gz" <<'PY'
import io, sys, tarfile
source, dest = sys.argv[1], sys.argv[2]
with tarfile.open(source, "r:gz") as incoming, tarfile.open(dest, "w:gz") as outgoing:
    for member in incoming.getmembers():
        extracted = incoming.extractfile(member) if member.isfile() else None
        outgoing.addfile(member, extracted)
    payload = b"nope\n"
    extra = tarfile.TarInfo("notes.txt")
    extra.size = len(payload)
    outgoing.addfile(extra, io.BytesIO(payload))
    climb = tarfile.TarInfo("../etc/passwd")
    climb.size = len(payload)
    outgoing.addfile(climb, io.BytesIO(payload))
PY
hostile_sha="$(sha256sum "${hostile_prefix}/hostile.tar.gz" | awk '{print $1}')"
set +e
bash "${ROOT}/scripts/install.sh" --from-archive \
    --archive "${hostile_prefix}/hostile.tar.gz" --sha256 "$hostile_sha" \
    --prefix "$hostile_prefix" >/tmp/rs118-hostile.out 2>/tmp/rs118-hostile.err
hostile_code=$?
set -e
[[ "$hostile_code" -ne 0 ]] || fail "traversal or extra member was accepted"
[[ "$(cat "${hostile_prefix}/KEEP")" == "unchanged" ]] || fail "hostile archive changed the prefix"
[[ ! -e "${hostile_prefix}/bin" && ! -e "${hostile_prefix}/etc" ]] || fail "hostile archive wrote the prefix"
rm -rf "$hostile_prefix"

prefix="$(mktemp -d)"
# Seed configuration, a key file, and data that must survive replacement.
mkdir -p "${prefix}/etc/reposync" "${prefix}/var/lib/reposync" "${prefix}/bin"
printf 'kept-config\n' > "${prefix}/etc/reposync/config.toml"
printf 'kept-secret\n' > "${prefix}/etc/reposync/env"
printf 'kept-data\n' > "${prefix}/var/lib/reposync/reposync.db"
# Placeholder binaries so the prefix is an existing install.
printf '#!/bin/sh\nexit 9\n' > "${prefix}/bin/reposync"
chmod 755 "${prefix}/bin/reposync"
cp "${prefix}/bin/reposync" "${prefix}/bin/reposync-daemon"

bash "${ROOT}/scripts/install.sh" --from-archive --archive "$stub_archive" --sha256 "$stub_sha" --prefix "$prefix"
[[ "$(cat "${prefix}/etc/reposync/config.toml")" == "kept-config" ]] || fail "config was replaced"
[[ "$(cat "${prefix}/etc/reposync/env")" == "kept-secret" ]] || fail "key file was replaced"
[[ "$(cat "${prefix}/var/lib/reposync/reposync.db")" == "kept-data" ]] || fail "data was replaced"
[[ -f "${prefix}/bin/static/index.html" && -f "${prefix}/bin/static/assets/app.js" ]] || fail "static members missing"

echo "building native release binaries"
(cd "$ROOT" && rustup run stable cargo build --release --bin reposync --bin reposync-daemon)
bash "${ROOT}/scripts/assemble_release_archive.sh" --bin-dir "$BIN_DIR" --out "$OUT"
SHA="$(awk '{print $1}' "${OUT}.sha256")"
bash "${ROOT}/scripts/install.sh" --from-archive --archive "$OUT" --sha256 "$SHA" --prefix "$prefix"
[[ "$(cat "${prefix}/etc/reposync/config.toml")" == "kept-config" ]] || fail "config changed during binary replacement"
help_out="$("${prefix}/bin/reposync" --help)"
printf '%s\n' "$help_out" | grep -q "reposync" || fail "CLI help missing"

# Synthetic runtime config. The preserved config.toml is not the daemon input.
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
grep -q "RepoSync" /tmp/rs118-index.html || fail "static index missing"
curl -fsS "http://127.0.0.1:${port}/assets/app.js" -o /tmp/rs118-app.js
grep -q "reposyncStatic" /tmp/rs118-app.js || fail "static asset missing"
echo "QUALIFIED native x86_64-unknown-linux-gnu prefix=${prefix}"
kill "$daemon_pid"
wait "$daemon_pid" || true
trap - EXIT
rm -rf "$prefix"
echo "QUALIFY_EXIT 0"
