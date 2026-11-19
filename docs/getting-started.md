# Getting Started

This guide walks you through setting up RepoSync to synchronize an SVN repository with a GitHub repository.

## Prerequisites

- **SVN client** (`svn` CLI) version 1.8+
- **Git** version 2.20+
- **Network access** to both your SVN server and GitHub (Enterprise or .com)
- **A VM or server** to run the daemon (Linux recommended)
- **SVN admin access** to enable the `pre-revprop-change` hook (needed for author mapping)

## Step 1: Install RepoSync

### Option A: Binary Release (Recommended)

```bash
curl -fsSL https://github.com/chriscase/RepoSync/releases/latest/download/install.sh | bash
```

### Option B: Build from Source

```bash
git clone https://github.com/chriscase/RepoSync.git
cd RepoSync
cargo build --release
sudo install -m 755 target/release/reposync-daemon /usr/local/bin/
sudo install -m 755 target/release/reposync /usr/local/bin/
```

### Option C: Docker

```bash
docker pull ghcr.io/chriscase/reposync:latest
```

## Step 2: Prepare Your SVN Server

RepoSync needs the `pre-revprop-change` hook enabled to preserve author identity when syncing Git commits back to SVN.

On your SVN server, create this hook:

```bash
# /path/to/svn/repos/hooks/pre-revprop-change
#!/bin/sh
REPOS="$1"
REV="$2"
USER="$3"
PROPNAME="$4"
ACTION="$5"

# Allow RepoSync to set the original author
if [ "$PROPNAME" = "svn:author" ] && [ "$USER" = "sync-service" ]; then
    exit 0
fi

# Allow log message edits
if [ "$PROPNAME" = "svn:log" ]; then
    exit 0
fi

exit 1
```

Make it executable:
```bash
chmod +x /path/to/svn/repos/hooks/pre-revprop-change
```

Optionally, add a post-commit hook for instant sync (instead of polling):

```bash
# /path/to/svn/repos/hooks/post-commit
#!/bin/sh
REPOS="$1"
REV="$2"
curl -s -X POST http://your-reposync-server:8080/webhook/svn \
  -H "Content-Type: application/json" \
  -d "{\"repository\": \"$REPOS\", \"revision\": $REV}" || true
```

## Step 3: Prepare Your GitHub Repository

1. Create a **Personal Access Token** (or GitHub App) with `repo` scope
2. Set up a **webhook** on the GitHub repository:
   - URL: `http://your-reposync-server:8080/webhook/github`
   - Content type: `application/json`
   - Secret: (generate a random string)
   - Events: select "Just the push event"

## Step 4: Create the Configuration

For a new configuration, choose an existing directory you can write. The commands below assume `/etc/reposync` is writable by the account generating the file. If your installation already provides `config.toml`, go straight to editing it.

```bash
reposync init --output /etc/reposync/config.toml
```

Edit `/etc/reposync/config.toml` with your endpoints and a data directory the daemon can write. The generated `[web].listen` is `127.0.0.1:3000`. This example uses port 8080. [Full example configuration](../config.example.toml)

```toml
[daemon]
poll_interval_secs = 15
data_dir = "/var/lib/reposync"

[svn]
url = "https://svn.company.com/repos/project"
username = "sync-service"
password_env = "REPOSYNC_SVN_PASSWORD"

[github]
api_url = "https://github.company.com/api/v3"  # or https://api.github.com
repo = "org/project"
token_env = "REPOSYNC_GITHUB_TOKEN"
webhook_secret_env = "REPOSYNC_WEBHOOK_SECRET"

[identity]
mapping_file = "/etc/reposync/authors.toml"
email_domain = "company.com"

[web]
listen = "0.0.0.0:8080"
auth_mode = "simple"
admin_password_env = "REPOSYNC_ADMIN_PASSWORD"
```

Validate the edited file before startup.

```bash
reposync validate --config /etc/reposync/config.toml
```

## Step 5: Set Up Author Mappings

If you use a mapping file, set `mapping_file` in `[identity]` as shown above and create `/etc/reposync/authors.toml` with your mappings.

```toml
[authors]
jsmith = { name = "John Smith", email = "jsmith@company.com" }
janedoe = { name = "Jane Doe", email = "jane.doe@company.com" }
# Add all SVN users here

[defaults]
email_domain = "company.com"
```

Any unmapped SVN user `foo` will default to `foo <foo@company.com>`.

## Step 6: Set Up Secrets

Create `/etc/reposync/env`:

```bash
REPOSYNC_SVN_PASSWORD=your-svn-service-account-password
REPOSYNC_GITHUB_TOKEN=ghp_xxxxxxxxxxxxxxxxxxxx
REPOSYNC_WEBHOOK_SECRET=your-webhook-secret
REPOSYNC_ADMIN_PASSWORD=your-dashboard-password
REPOSYNC_SESSION_SECRET=$(openssl rand -hex 32)
```

Secure the file:
```bash
sudo chmod 600 /etc/reposync/env
sudo chown reposync:reposync /etc/reposync/env
```

## Step 7: Start the Daemon

```bash
sudo systemctl enable --now reposync
```

Or run directly:
```bash
reposync-daemon --config /etc/reposync/config.toml
```

## Step 8: Verify

1. Open the dashboard using the configured address. For the generated settings, use `http://127.0.0.1:3000` on the daemon host. For the example above, use `http://your-server:8080`.
2. Make a commit to SVN — it should appear in Git within 15 seconds
3. Push a commit to Git — it should appear in SVN within 15 seconds
4. Check the audit log: `reposync audit --limit 5`

## Troubleshooting

- **Daemon won't start**: Check `journalctl -u reposync -f` for errors
- **SVN auth failure**: Verify credentials with `svn info --username sync-service <url>`
- **GitHub auth failure**: Verify token with `curl -H "Authorization: token <TOKEN>" <api_url>/user`
- For an unexpected author mapping, run `reposync identity lookup jsmith --config /etc/reposync/config.toml` with the SVN username you want to check.

See [troubleshooting.md](troubleshooting.md) for more.
