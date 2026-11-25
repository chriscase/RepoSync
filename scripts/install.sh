#!/usr/bin/env bash
set -euo pipefail

# RepoSync installer
# Usage: curl -fsSL https://github.com/chriscase/RepoSync/releases/latest/download/install.sh | bash

# Explicit offline update of an already-configured, private user-owned root.
# The existing privileged/default installer flow below remains unchanged.
if [[ "${1:-}" == "--offline-update" ]]; then
    shift
    exec python3 "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/install_offline.py" "$@"
fi

INSTALL_DIR="${REPOSYNC_INSTALL_DIR:-/usr/local/bin}"
CONFIG_DIR="${REPOSYNC_CONFIG_DIR:-/etc/reposync}"
DATA_DIR="${REPOSYNC_DATA_DIR:-/var/lib/reposync}"
SYSTEMD_UNIT_DIR="${REPOSYNC_SYSTEMD_UNIT_DIR:-/etc/systemd/system}"
INSTALL_OS="$(uname -s)"

# Members install.sh will copy. Validated after the digest check and before
# any install-prefix, config, key, or data mutation.
REQUIRED_ARCHIVE_MEMBERS=(
    reposync
    reposync-daemon
    config.example.toml
    authors.example.toml
    reposync.service
    static/index.html
    static/favicon.svg
)

archive_sha256() {
    if command -v sha256sum >/dev/null; then
        sha256sum "$1"
    else
        shasum -a 256 "$1"
    fi | awk '{print $1}'
}

verify_archive_digest() {
    local archive="$1"
    local expected="$2"
    local actual
    actual="$(archive_sha256 "$archive")"
    if [[ "$actual" != "$expected" ]]; then
        echo "Error: package checksum mismatch" >&2
        exit 1
    fi
}

# Keep the release contract in the standalone installer. The assembler uses
# the same entry point to check Vite's complete non-sourcemap output.
release_members() {
    python3 - "$1" "$2" "${REQUIRED_ARCHIVE_MEMBERS[@]}" <<'PYTHON'
from html.parser import HTMLParser
from pathlib import Path
import re
import sys
import tarfile

mode, source, *required = sys.argv[1:]


def fail(message):
    sys.exit(f"Error: {message}")


class Entrypoint(HTMLParser):
    def __init__(self):
        super().__init__()
        self.assets = set()
        self.has_module = False
        self.has_style = False

    def handle_starttag(self, tag, attributes):
        attrs = dict(attributes)
        rel = (attrs.get("rel") or "").lower().split()
        if tag == "script" and attrs.get("type") == "module":
            url, suffix = attrs.get("src"), "js"
            self.has_module = True
        elif tag == "link" and "stylesheet" in rel:
            url, suffix = attrs.get("href"), "css"
            self.has_style = True
        elif tag == "link" and "modulepreload" in rel:
            url, suffix = attrs.get("href"), "js"
        else:
            return
        if not re.fullmatch(r"/assets/[A-Za-z0-9_][A-Za-z0-9_-]*-[A-Za-z0-9_-]{8}\." + suffix, url or ""):
            fail(f"unsupported release UI asset reference: {url!r}")
        self.assets.add("static" + url)


if mode == "build":
    dist = Path(source)
    present = {name for name in required if not name.startswith("static/")}
    for path in dist.rglob("*"):
        if path.is_symlink():
            fail(f"build output is a symlink: {path}")
        if path.is_file() and path.suffix != ".map":
            present.add("static/" + path.relative_to(dist).as_posix())
else:
    present = set()
    try:
        with tarfile.open(source, "r:gz") as handle:
            for member in handle:
                name = member.name[2:] if member.name.startswith("./") else member.name
                if (not name or name.startswith("/") or ".." in name.split("/")
                        or "\\" in name):
                    fail(f"archive member is not allowed: {member.name}")
                if name in present:
                    fail(f"duplicate archive member: {name}")
                if member.issym():
                    fail(f"archive member is a symlink: {name}")
                if member.islnk():
                    fail(f"archive member is a hard link: {name}")
                if not member.isfile():
                    fail(f"archive member is not a regular file: {name}")
                present.add(name)
                if name == "static/index.html":
                    index = handle.extractfile(member).read()
    except (tarfile.TarError, OSError):
        fail("corrupt or unreadable archive")

for name in required:
    if name not in present:
        fail(f"archive is missing required member: {name}")
if mode == "build":
    index = (dist / "index.html").read_bytes()
entry = Entrypoint()
entry.feed(index.decode("utf-8"))
entry.close()
if not entry.has_module or not entry.has_style:
    fail("release UI index requires a module script and stylesheet")
expected = set(required) | entry.assets
for name in sorted(expected - present):
    fail(f"archive is missing required member: {name}")
for name in sorted(present - expected):
    fail(f"archive has an extra member: {name}")
print("\n".join(sorted(expected)))
PYTHON
}

if [[ "${1:-}" == "--list-build-members" ]]; then
    release_members build "$2"
    exit 0
fi

install_from_verified_archive() {
    local archive="$1"
    local prefix="$2"
    local members="$3"
    local use_sudo=1
    local install_dir config_dir data_dir stage
    if [[ -n "$prefix" ]]; then
        use_sudo=0
        install_dir="${prefix}/bin"
        config_dir="${prefix}/etc/reposync"
        data_dir="${prefix}/var/lib/reposync"
    else
        install_dir="${INSTALL_DIR}"
        config_dir="${CONFIG_DIR}"
        data_dir="${DATA_DIR}"
        if [[ "$(id -u)" -ne 0 && "$config_dir" == "/etc/reposync" ]]; then
            use_sudo=1
        else
            use_sudo=0
        fi
    fi
    # Parents must exist before copying. The systemd unit directory is a
    # default-install destination, including a direct root install that does
    # not escalate. A prefix install never uses it.
    if [[ "$use_sudo" -eq 1 ]]; then
        sudo mkdir -p "$install_dir" "$config_dir" "$data_dir" \
            "${install_dir}/static/assets"
        if [[ -z "$prefix" && "$INSTALL_OS" == Linux ]]; then
            sudo mkdir -p "$SYSTEMD_UNIT_DIR"
        fi
    else
        mkdir -p "$install_dir" "$config_dir" "$data_dir" \
            "${install_dir}/static/assets"
        if [[ -z "$prefix" && "$INSTALL_OS" == Linux ]]; then
            mkdir -p "$SYSTEMD_UNIT_DIR"
        fi
    fi

    stage="$(mktemp -d)"
    tar -xzf "$archive" -C "$stage"

    copy_file() {
        local src="$1" dest="$2" mode="$3"
        if [[ "$use_sudo" -eq 1 ]]; then
            sudo install -m "$mode" "$src" "$dest"
        else
            install -m "$mode" "$src" "$dest"
        fi
    }
    copy_tree_file() {
        local src="$1" dest="$2"
        if [[ "$use_sudo" -eq 1 ]]; then
            sudo mkdir -p "$(dirname "$dest")"
            sudo cp "$src" "$dest"
        else
            mkdir -p "$(dirname "$dest")"
            cp "$src" "$dest"
        fi
    }

    echo "==> install mode uid=$(id -u) sudo=${use_sudo} prefix=${prefix:-default}"
    echo "==> Installing binaries to ${install_dir}"
    copy_file "${stage}/reposync-daemon" "${install_dir}/reposync-daemon" 755
    copy_file "${stage}/reposync" "${install_dir}/reposync" 755
    local member
    while IFS= read -r member; do
        case "$member" in
            static/*)
                copy_tree_file "${stage}/${member}" "${install_dir}/${member}"
                ;;
        esac
    done <<< "$members"

    # Existing configuration, keys, and data are preserved. Examples are
    # copied only when the destination file is absent.
    if [[ ! -e "${config_dir}/config.toml" ]]; then
        copy_file "${stage}/config.example.toml" "${config_dir}/config.toml" 644
    fi
    if [[ ! -e "${config_dir}/authors.toml" ]]; then
        copy_file "${stage}/authors.example.toml" "${config_dir}/authors.toml" 644
    fi
    if [[ "$use_sudo" -eq 1 && ! -d "$data_dir" ]]; then
        sudo mkdir -p "$data_dir"
    fi

    # Service destination is not the privilege decision. Default and direct
    # root installs place the unit. Prefix installs keep it under the prefix.
    if [[ -n "$prefix" ]]; then
        copy_file "${stage}/reposync.service" "${config_dir}/reposync.service" 644
    elif [[ "$INSTALL_OS" == Linux ]]; then
        echo "==> Installing systemd unit to ${SYSTEMD_UNIT_DIR}"
        copy_file "${stage}/reposync.service" "${SYSTEMD_UNIT_DIR}/reposync.service" 644
        if [[ "$SYSTEMD_UNIT_DIR" == "/etc/systemd/system" ]] && command -v systemctl &>/dev/null; then
            if [[ "$use_sudo" -eq 1 ]]; then
                sudo systemctl daemon-reload || true
            elif [[ "$(id -u)" -eq 0 ]]; then
                systemctl daemon-reload || true
            fi
        fi
    fi

    rm -rf "$stage"
}

if [[ "${1:-}" == "--from-archive" ]]; then
    shift
    ARCHIVE=""
    ARCHIVE_SHA=""
    PREFIX=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --archive) ARCHIVE="$2"; shift 2 ;;
            --sha256) ARCHIVE_SHA="$2"; shift 2 ;;
            --prefix) PREFIX="$2"; shift 2 ;;
            *) echo "Error: unknown argument: $1" >&2; exit 1 ;;
        esac
    done
    if [[ -z "$ARCHIVE" || -z "$ARCHIVE_SHA" ]]; then
        echo "Error: --from-archive requires --archive and --sha256" >&2
        exit 1
    fi
    if [[ ! -f "$ARCHIVE" ]]; then
        echo "Error: archive not found: $ARCHIVE" >&2
        exit 1
    fi
    # Digest and member checks happen before any prefix mutation.
    verify_archive_digest "$ARCHIVE" "$ARCHIVE_SHA"
    members="$(release_members archive "$ARCHIVE")"
    install_from_verified_archive "$ARCHIVE" "$PREFIX" "$members"
    if [[ -n "$PREFIX" ]]; then
        echo "==> RepoSync installed under ${PREFIX}"
    else
        echo "==> RepoSync installed; binaries ${INSTALL_DIR}; config ${CONFIG_DIR}"
    fi
    exit 0
fi

VERSION="${REPOSYNC_VERSION:-latest}"

REPO="chriscase/RepoSync"

# Detect platform
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *) echo "Error: unsupported architecture: $ARCH"; exit 1 ;;
esac

case "$OS" in
    linux) PLATFORM="unknown-linux-gnu" ;;
    darwin) PLATFORM="apple-darwin" ;;
    *) echo "Error: unsupported OS: $OS"; exit 1 ;;
esac

TARGET="${ARCH}-${PLATFORM}"

echo "==> RepoSync Installer"
echo "    Platform: ${OS}/${ARCH}"
echo "    Version:  ${VERSION}"
echo ""

# Resolve latest version
if [ "$VERSION" = "latest" ]; then
    VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')
    echo "    Resolved version: ${VERSION}"
fi

# Download
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/reposync-${VERSION}-${TARGET}.tar.gz"
echo "==> Downloading from ${DOWNLOAD_URL}"

TMP_DIR=$(mktemp -d)
trap "rm -rf ${TMP_DIR}" EXIT

curl -fsSL "$DOWNLOAD_URL" -o "${TMP_DIR}/reposync.tar.gz"
curl -fsSL "${DOWNLOAD_URL}.sha256" -o "${TMP_DIR}/reposync.tar.gz.sha256"
EXPECTED_SHA="$(awk 'NR==1 {print $1}' "${TMP_DIR}/reposync.tar.gz.sha256")"
verify_archive_digest "${TMP_DIR}/reposync.tar.gz" "$EXPECTED_SHA"
members="$(release_members archive "${TMP_DIR}/reposync.tar.gz")"
install_from_verified_archive "${TMP_DIR}/reposync.tar.gz" "" "$members"

# Create system user if running as root
if [[ "$INSTALL_OS" == Linux && ( "$(id -u)" == 0 || -n "${SUDO_USER:-}" ) ]]; then
    if ! id -u reposync &>/dev/null; then
        echo "==> Creating reposync system user"
        sudo useradd -r -s /usr/sbin/nologin -d "$DATA_DIR" reposync || true
    fi
    sudo chown -R reposync:reposync "$DATA_DIR"
    sudo chown -R reposync:reposync "$CONFIG_DIR"
fi

echo ""
echo "==> RepoSync ${VERSION} installed successfully!"
echo ""
echo "Next steps:"
echo "  1. Edit ${CONFIG_DIR}/config.toml with your SVN and GitHub settings"
echo "  2. Edit ${CONFIG_DIR}/authors.toml with your author mappings"
echo "  3. Create ${CONFIG_DIR}/env with your secrets:"
echo "       REPOSYNC_SVN_PASSWORD=your-svn-password"
echo "       REPOSYNC_GITHUB_TOKEN=your-github-token"
echo "       REPOSYNC_ADMIN_PASSWORD=your-dashboard-password"
echo "       REPOSYNC_SESSION_SECRET=$(openssl rand -hex 32)"
if [[ "$INSTALL_OS" == Linux ]]; then
    echo "  4. Start the daemon: sudo systemctl enable --now reposync"
else
    echo "  4. Export the variables above and run ${INSTALL_DIR}/reposync-daemon --config ${CONFIG_DIR}/config.toml"
fi
echo "  5. Open http://your-server:8080 for the dashboard"
