#!/usr/bin/env bash
# Native #60 qualification using only disposable repositories and loopback services.
# This is deliberately not the Docker Compose harness.
set -euo pipefail
cd "$(dirname "$0")/../.."

for tool in cargo python3 git svn svnadmin svnlook; do
    command -v "$tool" >/dev/null || { echo "Missing required tool: $tool" >&2; exit 1; }
done
svn --version --quiet
svnadmin --version --quiet
svnlook --version --quiet

python3 -B tests/fixtures/monorepo/test_history.py
cargo test -p reposync-core --test partial_sync_53
# Keep the pre-existing unscoped test bodies unchanged.
cargo test -p reposync-core --test team_mode_e2e test_team_mode_
cargo build -p reposync-cli -p reposync-daemon -p reposync-personal
bin="${CARGO_TARGET_DIR:-target}/debug"
python3 -B scripts/team-path-rewrite-57.py "$bin/reposync-daemon"
python3 -B scripts/personal-rewrite-59.py --poll-for-rs60 --cli "$bin/reposync" \
    --personal "$bin/reposync-personal"
