#!/bin/sh
# Independent read observer. Trusts only the fixture CA. No SSH agent, no Git
# credential helper, no host configuration. Stays up so tests can exec into it.
set -eu
if [ -f /certs/ca.crt ]; then
  cp /certs/ca.crt /usr/local/share/ca-certificates/reposync-lab-ca.crt
  update-ca-certificates >/dev/null
fi
install -d -o observer -g observer /home/observer /work /home/observer/.svn /home/observer/.config/git
cat > /home/observer/.gitconfig <<'EOF'
[credential]
  helper =
[core]
  hooksPath = /home/observer/no-hooks
EOF
install -d -o observer -g observer /home/observer/no-hooks
cat > /home/observer/.svn/servers <<'EOF'
[global]
store-passwords = no
store-plaintext-passwords = no
store-ssl-client-cert-pp = no
store-ssl-client-cert-pp-plaintext = no
ssl-authority-files = /certs/ca.crt
EOF
cat > /home/observer/.svn/config <<'EOF'
[auth]
password-stores =
EOF
chown -R observer:observer /home/observer /work
exec runuser -u observer -- sleep infinity
