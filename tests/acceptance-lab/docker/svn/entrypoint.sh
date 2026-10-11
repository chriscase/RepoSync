#!/bin/sh
# Apache SVN over HTTPS with LDAP authentication. Certificates and the LDAP
# bind password are mounted per run; nothing here is a company endpoint.
set -eu

if [ ! -f /certs/ca.crt ] || [ ! -f /certs/svn.crt ] || [ ! -f /certs/svn.key ]; then
  echo "svn-entrypoint: fixture certificates are missing" >&2
  exit 1
fi
cp /certs/ca.crt /usr/local/share/ca-certificates/reposync-lab-ca.crt
update-ca-certificates >/dev/null

install -d -o www-data -g www-data /var/svn /var/svn/svn-config /var/run/apache2 /var/lock/apache2
if [ ! -d /var/svn/bootstrap ]; then
  svnadmin create /var/svn/bootstrap
  chown -R www-data:www-data /var/svn/bootstrap
fi

cat > /var/svn/svn-config/servers <<'EOF'
[global]
store-passwords = no
store-plaintext-passwords = no
store-ssl-client-cert-pp = no
store-ssl-client-cert-pp-plaintext = no
ssl-authority-files = /certs/ca.crt
EOF
cat > /var/svn/svn-config/config <<'EOF'
[auth]
password-stores =
EOF

# Wait until the directory accepts a real bind and search. This is infrastructure
# readiness, not a product assertion.
i=0
until ldapsearch -H ldaps://ldap:636 -o ldif-wrap=no \
      -x -D "${LDAP_BIND_DN}" -w "${LDAP_BIND_PASSWORD}" \
      -b "${LDAP_BASE_DN}" "(uid=alice)" uid cn mail >/tmp/alice.ldif
do
  i=$((i + 1))
  if [ "$i" -gt 60 ]; then
    echo "svn-entrypoint: LDAP bind/search did not become ready" >&2
    exit 1
  fi
  sleep 2
done
grep -q '^uid: alice$' /tmp/alice.ldif
rm -f /tmp/alice.ldif

if [ ! -f /etc/reposync/svn-authz ]; then
  echo "svn-entrypoint: authz file missing" >&2
  exit 1
fi
if [ ! -f /etc/reposync/svn-apache.conf ]; then
  echo "svn-entrypoint: apache config missing" >&2
  exit 1
fi

# Seed the bootstrap repository with a standard layout before Apache listens.
# file:// stays inside this disposable volume. HTTPS clients use LDAP afterwards.
seed_marker=/var/svn/.lab-bootstrap-seeded
if [ ! -f "$seed_marker" ]; then
  work=$(mktemp -d)
  svn checkout --non-interactive "file:///var/svn/bootstrap" "$work"
  mkdir -p "$work/trunk" "$work/branches" "$work/tags"
  printf 'lab bootstrap\n' > "$work/trunk/README.txt"
  svn add "$work/trunk" "$work/branches" "$work/tags"
  svn commit --username svc-sync -m "lab: bootstrap layout" "$work"
  rm -rf "$work"
  touch "$seed_marker"
fi
chown -R www-data:www-data /var/svn

exec apachectl -D FOREGROUND -f /etc/reposync/svn-apache.conf
