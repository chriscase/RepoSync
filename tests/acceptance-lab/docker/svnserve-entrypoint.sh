#!/bin/sh
# svn:// transport fixture. Authentication is a per-run password file, not LDAP.
# Apache HTTPS + LDAP is the directory-authenticated SVN service. These repos
# are separate volumes so the two servers never share a repository.
set -eu
install -d -o svn -g svn /var/svnserve
if [ ! -d /var/svnserve/transport ]; then
  svnadmin create /var/svnserve/transport
  cat > /var/svnserve/transport/conf/svnserve.conf <<'EOF'
[general]
anon-access = none
auth-access = write
password-db = passwd
authz-db = authz
realm = reposync-lab
EOF
  cp /etc/reposync/svnserve-passwd /var/svnserve/transport/conf/passwd
  cp /etc/reposync/svnserve-authz /var/svnserve/transport/conf/authz
  chown -R svn:svn /var/svnserve/transport
  work=$(mktemp -d)
  svn checkout --non-interactive "file:///var/svnserve/transport" "$work"
  mkdir -p "$work/trunk" "$work/branches" "$work/tags"
  printf 'svnserve transport fixture\n' > "$work/trunk/README.txt"
  svn add "$work/trunk" "$work/branches" "$work/tags"
  svn commit --username svc-sync -m "lab: svnserve transport layout" "$work"
  rm -rf "$work"
  chown -R svn:svn /var/svnserve/transport
fi
exec runuser -u svn -- svnserve -d --foreground --listen-host 0.0.0.0 --listen-port 3690 -r /var/svnserve
