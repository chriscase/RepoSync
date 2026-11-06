#!/bin/sh
# Bare Git smart HTTP fixture. This is not the self-hosted Git provider under
# test. Gitea owns API, webhooks, and LDAP login. This service exists so
# transport-specific proofs have a dumb HTTPS remote with certificate checking.
set -eu
if [ ! -f /certs/ca.crt ]; then
  echo "githttp: missing fixture CA" >&2
  exit 1
fi
cp /certs/ca.crt /usr/local/share/ca-certificates/reposync-lab-ca.crt
update-ca-certificates >/dev/null
install -d -o www-data -g www-data /var/git /var/run/apache2 /var/lock/apache2
if [ ! -d /var/git/transport.git ]; then
  git init --bare /var/git/transport.git
  work=$(mktemp -d)
  git -C "$work" init --initial-branch main
  git -C "$work" config user.email fixture@lab.invalid
  git -C "$work" config user.name "Lab Fixture"
  printf 'bare git http fixture\n' > "$work/README.txt"
  git -C "$work" add README.txt
  git -C "$work" commit -m "lab fixture"
  git -C "$work" remote add origin /var/git/transport.git
  git -C "$work" push origin main
  rm -rf "$work"
  chown -R www-data:www-data /var/git/transport.git
fi
exec apachectl -D FOREGROUND -f /etc/reposync/githttp-apache.conf
