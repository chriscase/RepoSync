# RepoSync Deployment & Operations Runbook

## Document Information

| Field | Value |
|-------|-------|
| **Status** | Draft |
| **Author** | [Your Name] |
| **Date** | April 2026 |
| **Audience** | Whoever is deploying and maintaining the RepoSync instance |

---

## Prerequisites

Before deploying RepoSync, ensure you have:

### Infrastructure

- A Linux VM or container host (Debian/Ubuntu recommended)
- Minimum 1 CPU, 512 MB RAM (2 CPU / 1 GB recommended for multi-repo setups)
- Disk space: 500 MB for the application + space for SQLite database (grows with sync history)
- Network access from the RepoSync server to both the SVN server and GitHub Enterprise

### Software

- `svn` CLI client installed on the RepoSync server
- `git` installed on the RepoSync server (with git-lfs if your repos use large files)
- Docker (if deploying via container) OR systemd (if deploying as a service)

### Accounts & Credentials

| Credential | Purpose | How to Obtain |
|------------|---------|---------------|
| **SVN service account** | Read/write access to SVN repository | Create a dedicated SVN user (e.g., `reposync-sync`) with commit permissions |
| **GitHub PAT** | Read/write access to GHE repository | Generate a Personal Access Token with `repo` scope on GitHub Enterprise |
| **Webhook secret** | Verify incoming GitHub webhooks | Generate a random string (e.g., `openssl rand -hex 32`) |
| **Admin password** | Log in to web dashboard | Choose a strong password |
| **Session secret** | Sign web dashboard cookies | Generate with `openssl rand -hex 32` |

---

## Installation

Choose one of the three deployment methods below.

### Option A: Binary Install (Recommended for simplicity)

```bash
# Download and run the install script
curl -fsSL https://github.com/chriscase/RepoSync/releases/latest/download/install.sh | bash
```

The install script will:

- Detect your platform (Linux x86_64/aarch64, macOS x86_64/aarch64)
- Download the latest release binaries
- Install `reposync-daemon` and `reposync` to `/usr/local/bin/`
- Create a `reposync` system user
- Create `/etc/reposync/` (config) and `/var/lib/reposync/` (data) directories
- Install the systemd service file

### Option B: Docker

```bash
docker pull ghcr.io/chriscase/reposync:latest
```

### Option C: Build from Source

```bash
git clone https://github.com/chriscase/RepoSync.git
cd RepoSync
cargo build --release
sudo install -m 755 target/release/reposync-daemon /usr/local/bin/
sudo install -m 755 target/release/reposync /usr/local/bin/
```

---

## Configuration

### Step 1: Create the Config File

```bash
# If you used the install script, the directory already exists
sudo mkdir -p /etc/reposync

# Generate a default config
reposync init --config /etc/reposync/config.toml

# Or copy the example
sudo cp config.example.toml /etc/reposync/config.toml
```

### Step 2: Edit config.toml

Open `/etc/reposync/config.toml` and configure for your environment. Below is a complete example for **SVN + GitHub Enterprise**:

```toml
[daemon]
poll_interval_secs = 15
log_level = "info"
data_dir = "/var/lib/reposync"

[svn]
url = "https://svn.yourcompany.com/repos/project"
username = "reposync-sync"
password_env = "REPOSYNC_SVN_PASSWORD"
layout = "standard"
# webhook_secret_env = "REPOSYNC_SVN_WEBHOOK_SECRET"

[github]
# IMPORTANT: For GitHub Enterprise, use your GHE API URL
api_url = "https://github.yourcompany.com/api/v3"
# Optional: only needed if your Git clone host differs from the API host
# git_base_url = "https://github.yourcompany.com"
repo = "your-org/your-repo"
token_env = "REPOSYNC_GITHUB_TOKEN"
webhook_secret_env = "REPOSYNC_WEBHOOK_SECRET"
default_branch = "main"

[identity]
mapping_file = "/etc/reposync/authors.toml"
email_domain = "yourcompany.com"
# Optional LDAP:
# ldap_url = "ldaps://ldap.yourcompany.com:636"
# ldap_base_dn = "dc=yourcompany,dc=com"
# ldap_bind_dn = "cn=reposync,ou=services,dc=yourcompany,dc=com"
# ldap_bind_password_env = "REPOSYNC_LDAP_PASSWORD"

[web]
listen = "0.0.0.0:8080"
session_secret_env = "REPOSYNC_SESSION_SECRET"
auth_mode = "simple"
admin_password_env = "REPOSYNC_ADMIN_PASSWORD"

[notifications]
# slack_webhook_url_env = "REPOSYNC_SLACK_WEBHOOK"
# email_smtp = "smtp.yourcompany.com:587"
# email_from = "reposync@yourcompany.com"
# email_recipients = ["team-lead@yourcompany.com"]

[sync]
mode = "direct"
auto_merge = true
sync_branches = true
sync_tags = true
```

> **Note for GitHub Enterprise**: The key setting is `api_url`. For GHE, this must be `https://your-ghe-host/api/v3`. The clone URL is derived automatically from this. Only set `git_base_url` if your Git operations go through a different host than the API.

### Step 3: Create the Identity Mapping File

```bash
sudo tee /etc/reposync/authors.toml << 'EOF'
[authors]
jsmith = { name = "John Smith", email = "jsmith@yourcompany.com" }
janedoe = { name = "Jane Doe", email = "jane.doe@yourcompany.com" }
# Add all team members who commit to SVN

[defaults]
email_domain = "yourcompany.com"
EOF
```

Any SVN username not listed in `[authors]` will fall back to `username@yourcompany.com`.

### Step 4: Create the Secrets Environment File

```bash
sudo tee /etc/reposync/env << EOF
REPOSYNC_SVN_PASSWORD=your-svn-service-account-password
REPOSYNC_GITHUB_TOKEN=ghp_your-github-enterprise-pat
REPOSYNC_ADMIN_PASSWORD=your-dashboard-admin-password
REPOSYNC_SESSION_SECRET=$(openssl rand -hex 32)
REPOSYNC_WEBHOOK_SECRET=$(openssl rand -hex 32)
EOF

# Restrict permissions — only the reposync user should read this
sudo chmod 600 /etc/reposync/env
sudo chown reposync:reposync /etc/reposync/env
```

> **Important**: Never put passwords or tokens directly in `config.toml`. Always use `_env` references that point to environment variables loaded from this file.

---

## GitHub Enterprise Webhook Setup

Webhooks allow RepoSync to react to GitHub pushes immediately instead of waiting for the next poll cycle.

1. Go to your repository on GitHub Enterprise: `https://github.yourcompany.com/your-org/your-repo/settings/hooks`
2. Click **Add webhook**
3. Configure:

| Field | Value |
|-------|-------|
| **Payload URL** | `https://reposync.yourcompany.com/api/webhooks/github` (or `http://reposync-server:8080/api/webhooks/github` if no reverse proxy) |
| **Content type** | `application/json` |
| **Secret** | The same value you set for `REPOSYNC_WEBHOOK_SECRET` |
| **Events** | Select: **Pushes** and **Pull requests** |
| **Active** | Checked |

4. Click **Add webhook**
5. GitHub will send a ping event — check the webhook delivery log to confirm it was accepted

---

## SVN Post-Commit Hook (Optional)

To get immediate sync on SVN commits (instead of waiting for the poll interval):

1. On your SVN server, create/edit the `post-commit` hook:

```bash
# In your SVN repo's hooks/ directory
cat > hooks/post-commit << 'HOOK'
#!/bin/bash
REPOS="$1"
REV="$2"
# Notify RepoSync of the new commit
curl -s -X POST \
  -H "X-Webhook-Secret: YOUR_SVN_WEBHOOK_SECRET" \
  -H "Content-Type: application/json" \
  -d "{\"repository\": \"$REPOS\", \"revision\": $REV}" \
  http://reposync-server:8080/api/webhooks/svn
HOOK
chmod +x hooks/post-commit
```

2. If using SVN webhook authentication, set `webhook_secret_env = "REPOSYNC_SVN_WEBHOOK_SECRET"` in `config.toml` and add `REPOSYNC_SVN_WEBHOOK_SECRET=your-secret` to the env file.

---

## Starting the Service

### Systemd (Binary Install)

```bash
# Install the service file (if not done by install script)
sudo cp scripts/reposync.service /etc/systemd/system/
sudo systemctl daemon-reload

# Enable and start
sudo systemctl enable reposync
sudo systemctl start reposync

# Verify it's running
sudo systemctl status reposync
```

### Docker

```bash
docker run -d \
  --name reposync \
  --restart=unless-stopped \
  -p 8080:8080 \
  -v /etc/reposync:/etc/reposync:ro \
  -v /var/lib/reposync:/var/lib/reposync \
  --env-file /etc/reposync/env \
  ghcr.io/chriscase/reposync:latest
```

### Docker Compose

Create a `docker-compose.yml`:

```yaml
version: "3.8"
services:
  reposync:
    image: ghcr.io/chriscase/reposync:latest
    restart: unless-stopped
    ports:
      - "8080:8080"
    volumes:
      - ./config:/etc/reposync:ro
      - reposync-data:/var/lib/reposync
    env_file:
      - ./secrets.env

volumes:
  reposync-data:
```

Then: `docker compose up -d`

---

## Initial Setup and First Sync

### Step 1: Access the Web Dashboard

Open your browser to `http://reposync-server:8080`. Log in with the admin password you configured.

### Step 2: Add a Repository (if not pre-configured)

In the web dashboard:

1. Navigate to **Repositories**
2. Click **Add Repository**
3. Enter your SVN URL and GitHub repo details
4. Test connections to both SVN and GitHub
5. Save

### Step 3: Import SVN History (Optional)

If you want the existing SVN history to appear in Git:

- From the web dashboard: Go to **Repositories → [Your Repo] → Import** and click **Start Full Import**
- From the CLI: `reposync import --repo-id 1 --full`

This replays every SVN revision as a Git commit. For large repositories, this can take a while. Progress is visible on the repo detail page.

### Step 4: Verify Sync

1. Make a test commit in SVN
2. Wait up to `poll_interval_secs` (default 15 seconds) or check the dashboard
3. Verify the commit appears in the GitHub Enterprise repository
4. Make a test commit/PR merge in GitHub Enterprise
5. Verify it appears in SVN

---

## Monitoring and Health Checks

### Health Endpoint

```bash
curl http://localhost:8080/api/status/health
# Expected: {"ok": true}
```

Use this for load balancer health checks or monitoring tools.

### Checking Status

```bash
# Via CLI
reposync status

# Via API
curl http://localhost:8080/api/status
```

### Viewing Logs

```bash
# Systemd
journalctl -u reposync -f

# Docker
docker logs -f reposync
```

### Key Log Messages to Watch For

| Log Message | Meaning | Action |
|-------------|---------|--------|
| `Sync cycle completed successfully` | Normal operation | None |
| `Conflict detected` | Both sides changed the same file | Resolve via dashboard or CLI |
| `SVN authentication failed` | Bad SVN credentials | Check `REPOSYNC_SVN_PASSWORD` |
| `GitHub API returned 401` | Bad GitHub token | Check `REPOSYNC_GITHUB_TOKEN` |
| `Echo suppressed` | Daemon correctly skipping its own commit | None (normal) |

### Setting Up Slack Notifications

1. Create a Slack incoming webhook in your workspace
2. Add to env file: `REPOSYNC_SLACK_WEBHOOK=https://hooks.slack.com/services/T.../B.../xxx`
3. Uncomment in config.toml: `slack_webhook_url_env = "REPOSYNC_SLACK_WEBHOOK"`
4. Restart the daemon

You'll receive Slack messages when:

- A conflict requires manual resolution
- Sync errors occur
- Import operations complete

---

## Backup Procedures

### What to Back Up

The critical data is the SQLite database at `/var/lib/reposync/reposync.db`. This contains:

- Commit mapping history (SVN rev ↔ Git SHA)
- Watermarks (last synced position)
- Conflict history
- Audit log
- User accounts

### How to Back Up

```bash
# Option 1: SQLite backup command (safe while daemon is running)
sqlite3 /var/lib/reposync/reposync.db ".backup /backup/reposync-$(date +%Y%m%d).db"

# Option 2: File copy (also safe with WAL mode while daemon is running)
cp /var/lib/reposync/reposync.db /backup/reposync-$(date +%Y%m%d).db
```

### Recommended Schedule

- **Daily**: Automated backup of the SQLite database
- **Retention**: Keep at least 7 days of backups

### Restore

```bash
# Stop daemon
sudo systemctl stop reposync

# Replace database
cp /backup/reposync-YYYYMMDD.db /var/lib/reposync/reposync.db
chown reposync:reposync /var/lib/reposync/reposync.db

# Start daemon
sudo systemctl start reposync
```

---

## Upgrading RepoSync

### Binary Upgrade

```bash
# Download new version
curl -fsSL https://github.com/chriscase/RepoSync/releases/latest/download/install.sh | bash

# The install script overwrites the binaries and restarts the service
```

### Docker Upgrade

```bash
docker pull ghcr.io/chriscase/reposync:latest
docker stop reposync
docker rm reposync
# Re-run with the same docker run command as before
```

### Before Upgrading

1. Back up the SQLite database
2. Review the release notes for breaking changes
3. Test in a non-production environment if possible

---

## Troubleshooting

### Daemon Won't Start

```bash
# Check logs
journalctl -u reposync -n 50 --no-pager

# Common causes:
# - Config file not found → verify path in service file
# - Missing environment variables → check /etc/reposync/env
# - Port 8080 already in use → ss -tlnp | grep 8080
# - Database permissions → verify reposync user owns /var/lib/reposync/
```

### SVN Authentication Failure

```bash
# Test credentials manually
svn info --username reposync-sync --password "$REPOSYNC_SVN_PASSWORD" \
  https://svn.yourcompany.com/repos/project
```

Common causes: wrong password, incorrect SVN URL (check for trailing slash), network/firewall blocking access.

### GitHub Authentication Failure

```bash
# Test GitHub Enterprise token manually
curl -H "Authorization: token $REPOSYNC_GITHUB_TOKEN" \
  https://github.yourcompany.com/api/v3/user
```

Common causes: token expired or revoked, token missing `repo` scope, wrong `api_url` in config.

### Commits Not Syncing

```bash
# Check sync status
reposync status
reposync audit --limit 10

# Common causes:
# - Daemon not running
# - Unresolved conflict blocking sync → reposync conflicts list
# - Webhook not configured → check poll interval
# - Echo suppression false positive → check commit map
```

### Author Mapping Not Working

```bash
# Check current mappings
reposync identity list

# Common causes:
# - User not in authors.toml → add the mapping
# - Typo in SVN username
# - LDAP connection issue (if using LDAP)
```

### Database Corruption (Rare)

```bash
# Stop daemon
sudo systemctl stop reposync

# Attempt recovery
sqlite3 /var/lib/reposync/reposync.db ".recover" | \
  sqlite3 /var/lib/reposync/reposync-recovered.db

# Replace and restart
mv /var/lib/reposync/reposync.db /var/lib/reposync/reposync.db.corrupt
mv /var/lib/reposync/reposync-recovered.db /var/lib/reposync/reposync.db
chown reposync:reposync /var/lib/reposync/reposync.db
sudo systemctl start reposync
```

---

## Emergency Procedures

### Stop All Syncing Immediately

```bash
# Systemd
sudo systemctl stop reposync

# Docker
docker stop reposync
```

The daemon stops cleanly (graceful shutdown with WAL checkpoint). No data is lost. When restarted, it resumes from where it left off.

### Resolve a Blocking Conflict

If sync is blocked because of an unresolved conflict:

```bash
# List conflicts
reposync conflicts list

# View conflict details
reposync conflicts show <conflict-id>

# Resolve (choose one)
reposync conflicts resolve <conflict-id> --accept git
reposync conflicts resolve <conflict-id> --accept svn
```

Or resolve via the web dashboard at **Conflicts → [Conflict] → Resolve**.

### Reset Sync State (Last Resort)

If sync is in a bad state and you need to start fresh:

```bash
sudo systemctl stop reposync

# Back up current database
cp /var/lib/reposync/reposync.db /backup/reposync-before-reset.db

# Delete database (sync will rebuild from scratch)
rm /var/lib/reposync/reposync.db

sudo systemctl start reposync
```

> **Warning**: This loses all commit mapping history and audit logs. The daemon will need to re-import or re-establish watermarks. Only do this as a last resort.

---

## Security Checklist

Use this checklist when deploying or auditing the RepoSync installation:

- [ ] Daemon runs as unprivileged `reposync` user (not root)
- [ ] Secrets are in environment variables via `/etc/reposync/env`, not in config files
- [ ] `/etc/reposync/env` has file permissions `600` and is owned by `reposync`
- [ ] Web dashboard is behind an HTTPS reverse proxy (nginx, Caddy, etc.)
- [ ] GitHub webhook secret is configured and matches the GHE webhook setting
- [ ] SVN service account has minimal required permissions (read/write to target repo only)
- [ ] GitHub token has minimal required scopes (`repo`)
- [ ] Session secret is a random 32+ byte hex string
- [ ] Firewall allows only necessary traffic (see network requirements in Architecture doc)

---

## Quick Reference

### File Locations

| Path | Purpose |
|------|---------|
| `/usr/local/bin/reposync-daemon` | Server daemon binary |
| `/usr/local/bin/reposync` | CLI management tool |
| `/etc/reposync/config.toml` | Main configuration file |
| `/etc/reposync/authors.toml` | Identity mapping file |
| `/etc/reposync/env` | Secrets environment file |
| `/var/lib/reposync/reposync.db` | SQLite database (all state) |
| `/etc/systemd/system/reposync.service` | Systemd unit file |

### Common CLI Commands

```bash
reposync status                              # Show sync status
reposync conflicts list                      # List active conflicts
reposync conflicts resolve <id> --accept git # Resolve a conflict
reposync sync now                            # Trigger immediate sync
reposync identity list                       # Show author mappings
reposync audit --limit 20                    # Recent sync history
reposync validate --config /etc/reposync/config.toml  # Validate config
```

### Environment Variables

| Variable | Required | Purpose |
|----------|----------|---------|
| `REPOSYNC_SVN_PASSWORD` | Yes | SVN service account password |
| `REPOSYNC_GITHUB_TOKEN` | Yes | GitHub PAT with `repo` scope |
| `REPOSYNC_ADMIN_PASSWORD` | Yes | Web dashboard admin password |
| `REPOSYNC_SESSION_SECRET` | Yes | Cookie signing key (32+ hex bytes) |
| `REPOSYNC_WEBHOOK_SECRET` | Recommended | GitHub webhook HMAC secret |
| `REPOSYNC_SLACK_WEBHOOK` | Optional | Slack incoming webhook URL |
| `REPOSYNC_LDAP_PASSWORD` | Optional | LDAP bind password (if using LDAP) |
| `REPOSYNC_SVN_WEBHOOK_SECRET` | Optional | SVN post-commit webhook auth |
