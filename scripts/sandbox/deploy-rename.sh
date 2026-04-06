#!/bin/bash
echo "=== Deploying RepoSync rename ==="

# Stop any running daemon
killall gitsvnsync-daemon 2>/dev/null
killall reposync-daemon 2>/dev/null
sleep 2
echo "Daemons stopped"

# Copy new binary
cp /opt/reposync/GitSvnSync/target/release/reposync-daemon /opt/reposync/GitSvnSync/target/debug/
echo "Binary copied"

# Rename DB if needed
if [ -f /opt/reposync/gitsvnsync.db ] && [ ! -f /opt/reposync/reposync.db ]; then
    cp /opt/reposync/gitsvnsync.db /opt/reposync/reposync.db
    echo "DB renamed: gitsvnsync.db → reposync.db"
elif [ -f /opt/reposync/reposync.db ]; then
    echo "reposync.db already exists"
else
    echo "WARNING: no DB found!"
fi

# Update start script
sed -i 's/gitsvnsync-daemon/reposync-daemon/g' /opt/reposync/start-daemon.sh
sed -i 's/gitsvnsync.log/reposync.log/g' /opt/reposync/start-daemon.sh
echo "Start script updated"

# Start daemon
nohup /opt/reposync/start-daemon.sh > /tmp/reposync.log 2>&1 &
sleep 6

# Verify
curl -s -m 5 http://127.0.0.1:8080/api/status/health
echo ""
echo "=== Deploy complete ==="
