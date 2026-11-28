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

Use Node.js 22 and npm to build the React dashboard. The daemon reads `static/index.html` and its assets from a `static/` directory beside the executable.

```bash
git clone https://github.com/chriscase/RepoSync.git
cd RepoSync
cargo build --release
npm ci --prefix web-ui
npm run build --prefix web-ui
sudo install -m 755 target/release/reposync-daemon /usr/local/bin/
sudo install -m 755 target/release/reposync /usr/local/bin/
sudo install -d -m 755 /usr/local/bin/static
sudo cp -R web-ui/dist/. /usr/local/bin/static/
```

If you run the binaries in place, copy the contents of `web-ui/dist/` into `target/release/static/` instead.

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

Optionally, add a post-commit hook for instant sync (instead of polling). On
the SVN server, run:

```bash
reposync svn-hook install \
  --repository /path/to/svn/repos \
  --server-url http://your-reposync-server:8080 \
  --secret-file /etc/reposync/svn-webhook-secret
```

It writes `hooks/reposync-post-commit` and, when the repository has no
`post-commit` hook yet, a `post-commit` that calls it. An existing
`post-commit` that RepoSync did not create is never changed; the command
prints the one line to add to it. Use `--dry-run` to see the plan first, and
`reposync svn-hook remove --repository ...` to take it out again.

SVN runs hooks with an empty environment, so the secret cannot come from an
environment variable. Put the same value the server reads through
`svn.webhook_secret_env` in the `--secret-file`, readable only by the account
that runs SVN hooks. The hook reads it at commit time and passes it to curl on
standard input, so it does not appear on a command line. Without a secret
file, notifications carry no secret and a server with `svn.webhook_secret_env`
set refuses them.

The hook sends `X-Webhook-Delivery: <repository path>@<revision>`; RepoSync
uses it only when a secret is configured, acknowledging a repeated value
without starting another inspection. The notification only asks for an early
poll, so a lost or late one changes nothing, and the hook never delays or
fails a commit.

## Step 3: Prepare Your GitHub Repository

Create an empty Git repository for the initial SVN import. Leave the target branch absent, including any starter README or license commit that would create it. RepoSync checks this before publishing the mirror.

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

Edit `/etc/reposync/config.toml` with your endpoints. Use an absolute `[daemon].data_dir` the daemon can write. This directory holds the Team database and repository checkouts. The generated `[web].listen` is `127.0.0.1:3000`. This example uses port 8080. [Full example configuration](../config.example.toml)

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

Validate the edited file before startup. This checks the configuration locally. Review the reported settings before connecting to the repositories.

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

Before the first start on a fresh database, set `REPOSYNC_ADMIN_PASSWORD` to a unique, nonempty password in the daemon's environment. The daemon uses this variable when it creates its initial `admin` account. An existing account keeps its password across restarts.

If you generated the file with an older CLI and have yet to start the daemon, check `[web].admin_password_env`. Use `admin_password_env = "REPOSYNC_ADMIN_PASSWORD"` so the configuration and first-run setup use the same variable.

For the supplied systemd service, create `/etc/reposync/env` with your own values.

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

For a direct foreground run, export the variables named in your configuration in that shell. The supplied systemd service reads `/etc/reposync/env` itself.

```bash
reposync-daemon --config /etc/reposync/config.toml
```

## Step 8: Import and Verify

1. Open the dashboard as an administrator. On the daemon host, use `http://127.0.0.1:3000` for the generated settings or `http://127.0.0.1:8080` for the example above.
2. Open **Repositories**. On the first start with an empty repository list, the daemon registers the SVN and Git pair from the configuration under its Git repository name. Select that entry. On **Settings**, check the SVN path, Git target branch, and **Credentials** for any access the repositories require.
3. On **Overview**, find **SVN Import**. Keep **Full SVN history** selected and choose **Start full history import** to replay the SVN branch into Git. To start at a single revision, choose **Snapshot at pinned SVN revision**, enter the revision or `HEAD`, and choose **Start snapshot import**. Earlier SVN history stays outside that snapshot.
4. Wait for **SVN Import Complete** and check the target Git branch. The scheduler skips a fresh registration until it has a verified mapping from an SVN revision to a Git commit. If the import reports an error or a hold, inspect the repository's import details before proceeding.
5. For a direct-sync pair, check the next ordinary SVN change on the Git branch and the next ordinary Git change on the SVN branch. **Sync Records** on the repository overview shows the processed commits. The generated polling interval is 60 seconds and the example above uses 15 seconds. Allow time for the next eligible poll and the sync itself.

To read recent audit entries, run `reposync audit --limit 5 --config /etc/reposync/config.toml` in another terminal on the daemon host. Use the same configuration path you passed to the daemon.

## Troubleshooting

- **Daemon won't start**: Check `journalctl -u reposync -f` for errors
- **SVN auth failure**: Verify credentials with `svn info --username sync-service <url>`
- **GitHub auth failure**: Verify token with `curl -H "Authorization: token <TOKEN>" <api_url>/user`
- For an unexpected author mapping, run `reposync identity lookup jsmith --config /etc/reposync/config.toml` with the SVN username you want to check.

See [troubleshooting.md](troubleshooting.md) for more.
