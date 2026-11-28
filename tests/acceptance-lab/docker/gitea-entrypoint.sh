#!/bin/sh
# Install the per-run fixture CA before Gitea's own entrypoint. Certificate
# checking stays enabled. The upstream entrypoint still owns process users.
set -eu
if [ -f /certs/ca.crt ]; then
  if [ -d /usr/local/share/ca-certificates ]; then
    cp /certs/ca.crt /usr/local/share/ca-certificates/reposync-lab-ca.crt
    update-ca-certificates >/dev/null 2>&1 || true
  fi
  if [ -f /etc/ssl/certs/ca-certificates.crt ]; then
    cat /certs/ca.crt >> /etc/ssl/certs/ca-certificates.crt
  fi
fi
if [ -f /certs/gitea.key ]; then
  chown 1000:1000 /certs/gitea.key /certs/gitea.crt 2>/dev/null || true
  chmod 640 /certs/gitea.key || true
fi
exec /usr/bin/entrypoint "$@"
