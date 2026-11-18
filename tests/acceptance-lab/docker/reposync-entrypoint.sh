#!/bin/sh
# Lab entrypoint. Isolates Git/SVN/SSH state, trusts only the mounted fixture CA,
# then drops to the reposync user. It does not read host credentials.
set -eu

if [ "$(id -u)" -ne 0 ]; then
  echo "lab-entrypoint: must start as root so the fixture CA can be installed" >&2
  exit 1
fi

umask 077
install -d -o reposync -g reposync /var/lib/reposync /var/lib/reposync/svn-config /tmp/reposync-tmp

# Refuse well-known placeholder credentials. A successful lab run must use
# secrets generated for this invocation.
reject_placeholder() {
  name="$1"
  value="${2:-}"
  case "$value" in
    ""|changeme|admin123|testpass123|test-token|password|secret)
      echo "lab-entrypoint: $name is missing or a placeholder" >&2
      exit 1
      ;;
  esac
}

reject_placeholder REPOSYNC_ADMIN_PASSWORD "${REPOSYNC_ADMIN_PASSWORD:-}"
reject_placeholder REPOSYNC_SVN_PASSWORD "${REPOSYNC_SVN_PASSWORD:-}"
reject_placeholder REPOSYNC_GITHUB_TOKEN "${REPOSYNC_GITHUB_TOKEN:-}"
reject_placeholder REPOSYNC_SESSION_SECRET "${REPOSYNC_SESSION_SECRET:-}"
reject_placeholder REPOSYNC_WEBHOOK_SECRET "${REPOSYNC_WEBHOOK_SECRET:-}"
reject_placeholder REPOSYNC_LDAP_PASSWORD "${REPOSYNC_LDAP_PASSWORD:-}"

if [ -f /certs/ca.crt ]; then
  cp /certs/ca.crt /usr/local/share/ca-certificates/reposync-lab-ca.crt
  update-ca-certificates >/dev/null
fi

# Empty global Git config. No helpers, no hooks, no insteadOf rewrites.
cat > /var/lib/reposync/lab.gitconfig <<'EOF'
[user]
  name = RepoSync Lab
  email = daemon@lab.invalid
[init]
  defaultBranch = main
[credential]
  helper =
[core]
  hooksPath = /var/lib/reposync/no-hooks
EOF
install -d -o reposync -g reposync /var/lib/reposync/no-hooks
# SVN must not reuse a host auth cache. Point it at a private empty config.
cat > /var/lib/reposync/svn-config/servers <<'EOF'
[global]
store-passwords = no
store-plaintext-passwords = no
store-ssl-client-cert-pp = no
store-ssl-client-cert-pp-plaintext = no
EOF
if [ -f /certs/ca.crt ]; then
  printf 'ssl-authority-files = /certs/ca.crt\n' >> /var/lib/reposync/svn-config/servers
fi
cat > /var/lib/reposync/svn-config/config <<'EOF'
[auth]
password-stores =
[helpers]
EOF
chown -R reposync:reposync /var/lib/reposync

unset SSH_AUTH_SOCK || true
unset GIT_ASKPASS || true
unset SSH_AGENT_PID || true
export HOME=/var/lib/reposync
export GIT_CONFIG_GLOBAL=/var/lib/reposync/lab.gitconfig
export GIT_CONFIG_SYSTEM=/dev/null
export GIT_CONFIG_NOSYSTEM=1
export GIT_TERMINAL_PROMPT=0
export SVN_CONFIG_DIR=/var/lib/reposync/svn-config
export TMPDIR=/tmp/reposync-tmp

cd /var/lib/reposync
exec runuser -u reposync -- env \
  HOME="$HOME" \
  GIT_CONFIG_GLOBAL="$GIT_CONFIG_GLOBAL" \
  GIT_CONFIG_SYSTEM=/dev/null \
  GIT_CONFIG_NOSYSTEM=1 \
  GIT_TERMINAL_PROMPT=0 \
  SVN_CONFIG_DIR="$SVN_CONFIG_DIR" \
  TMPDIR="$TMPDIR" \
  reposync-daemon --config /etc/reposync/config.toml
