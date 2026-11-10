#!/usr/bin/env bash
# Assemble the install.sh archive from already-built native binaries.
# Writes ARCHIVE and ARCHIVE.sha256. Does not install or deploy.
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR=""
OUT=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --bin-dir) BIN_DIR="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 1 ;;
    esac
done

if [[ -z "$BIN_DIR" || -z "$OUT" ]]; then
    echo "usage: assemble_release_archive.sh --bin-dir DIR --out ARCHIVE" >&2
    exit 1
fi

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

install -m 755 "${BIN_DIR}/reposync" "${stage}/reposync"
install -m 755 "${BIN_DIR}/reposync-daemon" "${stage}/reposync-daemon"
cp "${ROOT}/config.example.toml" "${stage}/config.example.toml"
cp "${ROOT}/authors.example.toml" "${stage}/authors.example.toml"
cp "${ROOT}/scripts/reposync.service" "${stage}/reposync.service"
mkdir -p "${stage}/static/assets"
cp "${ROOT}/packaging/static/index.html" "${stage}/static/index.html"
cp "${ROOT}/packaging/static/assets/app.js" "${stage}/static/assets/app.js"
cp "${ROOT}/scripts/release-archive.manifest" "${stage}/RELEASE-MANIFEST"

while IFS= read -r member; do
    [[ -n "$member" ]] || continue
    if [[ ! -f "${stage}/${member}" ]]; then
        echo "assembler missing member: $member" >&2
        exit 1
    fi
done < "${ROOT}/scripts/release-archive.manifest"

tar -C "$stage" -czf "$OUT" \
    reposync reposync-daemon config.example.toml authors.example.toml \
    reposync.service static/index.html static/assets/app.js RELEASE-MANIFEST
sha256sum "$OUT" | awk '{print $1}' > "${OUT}.sha256"
echo "assembled ${OUT}"
