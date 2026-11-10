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

# Members install.sh will copy. Validated after the digest check and before
# any install-prefix, config, key, or data mutation.
REQUIRED_ARCHIVE_MEMBERS=(
    reposync
    reposync-daemon
    config.example.toml
    authors.example.toml
    reposync.service
    static/index.html
    static/assets/app.js
)

archive_sha256() {
    sha256sum "$1" | awk '{print $1}'
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

require_archive_members() {
    local archive="$1"
    local listing missing=0 name
    if ! listing="$(tar -tzf "$archive" 2>/dev/null)"; then
        echo "Error: corrupt or unreadable archive" >&2
        exit 1
    fi
    for name in "${REQUIRED_ARCHIVE_MEMBERS[@]}"; do
        if ! grep -Fxq "$name" <<<"$listing"; then
            echo "Error: archive is missing required member: $name" >&2
            missing=1
        fi
    done
    if [[ "$missing" -ne 0 ]]; then
        exit 1
    fi
}

install_from_verified_archive() {
    local archive="$1"
    local prefix="${2:-}"
    local use_sudo=1
    local install_dir config_dir data_dir stage
    if [[ -n "$prefix" ]]; then
        use_sudo=0
        install_dir="${prefix}/bin"
        config_dir="${prefix}/etc/reposync"
        data_dir="${prefix}/var/lib/reposync"
        mkdir -p "$install_dir" "$config_dir" "$data_dir"
    else
        install_dir="${INSTALL_DIR}"
        config_dir="${CONFIG_DIR}"
        data_dir="${DATA_DIR}"
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

    echo "==> Installing binaries to ${install_dir}"
    copy_file "${stage}/reposync-daemon" "${install_dir}/reposync-daemon" 755
    copy_file "${stage}/reposync" "${install_dir}/reposync" 755
    copy_tree_file "${stage}/static/index.html" "${install_dir}/static/index.html"
    copy_tree_file "${stage}/static/assets/app.js" "${install_dir}/static/assets/app.js"

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

    if [[ "$use_sudo" -eq 1 ]] && command -v systemctl &>/dev/null; then
        echo "==> Installing systemd service"
        sudo cp "${stage}/reposync.service" /etc/systemd/system/reposync.service
        sudo systemctl daemon-reload || true
    else
        cp "${stage}/reposync.service" "${config_dir}/reposync.service"
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
    if [[ -z "$ARCHIVE" || -z "$ARCHIVE_SHA" || -z "$PREFIX" ]]; then
        echo "Error: --from-archive requires --archive, --sha256, and --prefix" >&2
        exit 1
    fi
    if [[ ! -f "$ARCHIVE" ]]; then
        echo "Error: archive not found: $ARCHIVE" >&2
        exit 1
    fi
    # Digest and member checks happen before any prefix mutation.
    verify_archive_digest "$ARCHIVE" "$ARCHIVE_SHA"
    require_archive_members "$ARCHIVE"
    install_from_verified_archive "$ARCHIVE" "$PREFIX"
    echo "==> RepoSync installed under ${PREFIX}"
    exit 0
fi

VERSION="${REPOSYNC_VERSION:-latest}"
INSTALL_DIR="${REPOSYNC_INSTALL_DIR:-/usr/local/bin}"
CONFIG_DIR="/etc/reposync"
DATA_DIR="/var/lib/reposync"

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
require_archive_members "${TMP_DIR}/reposync.tar.gz"
install_from_verified_archive "${TMP_DIR}/reposync.tar.gz"

# Create system user if running as root
if [ "$(id -u)" = "0" ] || [ -n "${SUDO_USER:-}" ]; then
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
echo "  4. Start the daemon: sudo systemctl enable --now reposync"
echo "  5. Open http://your-server:8080 for the dashboard"
