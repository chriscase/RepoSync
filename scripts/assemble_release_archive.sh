#!/usr/bin/env bash
# Assemble the install.sh archive from already-built native binaries and the
# real web-ui production build. Writes ARCHIVE and ARCHIVE.sha256 in the
# form sha256sum -c accepts. Does not install or deploy.
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd -- "$ROOT" && pwd)"
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

echo "building product web-ui"
npm ci --prefix "${ROOT}/web-ui"
npm run build --prefix "${ROOT}/web-ui"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

install -m 755 "${BIN_DIR}/reposync" "${stage}/reposync"
install -m 755 "${BIN_DIR}/reposync-daemon" "${stage}/reposync-daemon"
cp "${ROOT}/config.example.toml" "${stage}/config.example.toml"
cp "${ROOT}/authors.example.toml" "${stage}/authors.example.toml"
cp "${ROOT}/scripts/reposync.service" "${stage}/reposync.service"

dist="${ROOT}/web-ui/dist"
members="$(bash "${ROOT}/scripts/install.sh" --list-build-members "$dist")"
declare -a tar_members=()
while IFS= read -r member; do
    [[ -n "$member" ]] || continue
    tar_members+=("$member")
    case "$member" in
        static/*)
            rel="${member#static/}"
            if [[ ! -f "${dist}/${rel}" ]]; then
                echo "assembler missing product asset: $member" >&2
                exit 1
            fi
            mkdir -p "${stage}/$(dirname "$member")"
            cp "${dist}/${rel}" "${stage}/${member}"
            ;;
        *)
            if [[ ! -f "${stage}/${member}" ]]; then
                echo "assembler missing member: $member" >&2
                exit 1
            fi
            ;;
    esac
done <<< "$members"

if grep -rqF 'window.reposyncStatic = "synthetic"' "$stage/static"; then
    echo "synthetic fixture leaked into the product build" >&2
    exit 1
else
    [[ $? -eq 1 ]] || exit 1
fi

COPYFILE_DISABLE=1 tar -C "$stage" -czf "$OUT" "${tar_members[@]}"
out_dir="$(cd -- "$(dirname -- "$OUT")" && pwd)"
out_name="$(basename -- "$OUT")"
(
    cd "$out_dir"
    if command -v sha256sum >/dev/null; then
        sha256sum "$out_name"
    else
        shasum -a 256 "$out_name"
    fi > "${out_name}.sha256"
)
echo "assembled ${out_dir}/${out_name}"
