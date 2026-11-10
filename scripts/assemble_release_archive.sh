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
manifest="${ROOT}/scripts/release-archive.manifest"
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
done < "$manifest"

python3 - "$dist" "$stage" "$manifest" <<'PY'
import sys
from pathlib import Path

dist = Path(sys.argv[1])
stage = Path(sys.argv[2])
manifest = Path(sys.argv[3]).read_text().splitlines()
expected = {line[len("static/"):] for line in manifest if line.startswith("static/")}
actual = {path.relative_to(dist).as_posix() for path in dist.rglob("*") if path.is_file() and path.suffix != ".map"}
if actual != expected:
    print("product web-ui files differ from the manifest", file=sys.stderr)
    print("dist " + " ".join(sorted(actual)), file=sys.stderr)
    print("manifest " + " ".join(sorted(expected)), file=sys.stderr)
    sys.exit(1)
sentinel = 'window.reposyncStatic = "synthetic"'
for rel in expected:
    data = (stage / "static" / rel).read_text(errors="replace")
    if sentinel in data:
        print(f"synthetic fixture leaked into {rel}", file=sys.stderr)
        sys.exit(1)
PY

tar -C "$stage" -czf "$OUT" "${tar_members[@]}"
out_dir="$(cd -- "$(dirname -- "$OUT")" && pwd)"
out_name="$(basename -- "$OUT")"
(
    cd "$out_dir"
    sha256sum "$out_name" > "${out_name}.sha256"
)
echo "assembled ${out_dir}/${out_name}"
