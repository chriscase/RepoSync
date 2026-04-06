#!/bin/bash
export REPOSYNC_ADMIN_PASSWORD=changeme
export SVN_PASSWORD=changeme
export GIT_TOKEN=changeme
export WEBHOOK_SECRET=changeme
cd /opt/reposync/GitSvnSync/target/release
exec ./reposync-daemon --config /home/chrisc/gitsvnsync.toml
