# RepoSync

**Bidirectional SVN <-> Git synchronization bridge that just works.**

RepoSync synchronizes commits between SVN and Git repositories. It can run as a server daemon for entire teams or as a lightweight personal tool on your laptop. It watches both systems, auto-merges non-conflicting changes, and alerts you only when human intervention is needed.

## Why RepoSync?

Many enterprise teams are stuck on SVN but have access to GitHub Enterprise or GitHub.com. Existing tools don't solve the problem:

| Tool | Problem |
|------|---------|
| **git-svn** | Single-developer only, corrupts merge history, no daemon mode |
| **SubGit** | Commercial, requires disabling writes to both repos |
| **svn2git** | One-way migration only |

RepoSync fills this gap as a **free, open-source, production-grade** solution.

## Two Modes

RepoSync supports two distinct workflows depending on your needs:

### Team Mode

Full bidirectional sync for entire teams. Runs as a server daemon on a VM with a web dashboard, identity mapping across your whole team, and conflict notifications via Slack and email. This is the right choice when your team needs continuous, automatic synchronization between SVN and Git.

### Personal Branch Mode

Personal mode runs on your laptop. It mirrors SVN changes to your Git branch and sends changes from merged GitHub pull requests back to SVN.

See [docs/personal-branch/](docs/personal-branch/) for the full Personal Branch Mode documentation.

## Features

- **Bidirectional sync** -- SVN commits appear in Git, Git pushes appear in SVN
- **Server daemon** -- runs on a VM, watches both repos, syncs automatically (team mode)
- **Personal branch sync** -- runs on your laptop, PR-based workflow (personal mode)
- **Author identity mapping** -- SVN usernames mapped to Git name+email, seamlessly preserved
- **Automatic conflict resolution** -- non-overlapping changes merged automatically
- **Web dashboard** -- React-based UI for monitoring sync status and resolving conflicts (team mode)
- **Conflict notifications** -- Slack and email alerts when human intervention needed (team mode)
- **GitHub Enterprise & GitHub.com** -- first-class support via GitHub API and webhooks
- **Configurable SVN layouts** -- standard (trunk/branches/tags) or custom paths
- **Two sync modes** -- direct auto-sync or PR-gated for teams wanting review
- **Crash recovery** -- transaction log ensures no data loss on restart
- **Docker & systemd** -- deploy however you prefer (team mode)

## Personal Branch Mode -- Quick Start

Create your Personal configuration with the interactive wizard.

```bash
reposync personal init
```

Before your first import, edit `~/.config/reposync/personal.toml`. Under `[personal]`, set `data_dir` to an absolute path you can write, such as `/home/alex/.local/share/reposync` for user `alex`. This directory holds your database and Git checkout. In the shell where you will run the bridge, set the environment variables named by the wizard.

Run the import command printed by the wizard. Its default, `reposync personal import --snapshot`, starts with the current SVN tree. Choose `reposync personal import --full` if you want the revision history. Once the import completes, start polling.

```bash
reposync personal start --foreground
```

`start` runs in the foreground. From another terminal, run `reposync personal status` to inspect the bridge or `reposync personal stop` to stop it. [Personal quick start](docs/personal-branch/quickstart.md)

## Team Mode -- Quick Start

### Prerequisites

- SVN client (`svn` CLI) installed
- Git installed
- Network access to both your SVN server and GitHub

### Install

**From binary release:**
```bash
curl -fsSL https://github.com/chriscase/RepoSync/releases/latest/download/install.sh | bash
```

**From source:**
```bash
git clone https://github.com/chriscase/RepoSync.git
cd RepoSync
cargo build --release
npm ci --prefix web-ui
npm run build --prefix web-ui
mkdir -p target/release/static
cp -R web-ui/dist/. target/release/static/
export PATH="$PWD/target/release:$PATH"
```

The dashboard build uses Node.js 22 and npm. If you move the daemon, put the built `static/` directory beside its executable.

**Docker:**
```bash
docker pull ghcr.io/chriscase/reposync:latest
```

### Configure

These commands assume the binaries are on your `PATH`. A source build places them in `target/release/`. To generate a new configuration, choose an existing directory you can write. The examples use `/etc/reposync`. If you already have a configuration file, edit it directly.

```bash
reposync init --output /etc/reposync/config.toml
$EDITOR /etc/reposync/config.toml
reposync validate --config /etc/reposync/config.toml
```

Set your SVN and GitHub details before validation. To use an author mapping file, set `mapping_file = "/etc/reposync/authors.toml"` in `[identity]` and create that file with your mappings. The full example includes this optional setting. [Example configuration](config.example.toml) · [Team getting started](docs/getting-started.md)

### Run

Before the first start on a fresh database, supply a unique, nonempty `REPOSYNC_ADMIN_PASSWORD` in the daemon's environment. The daemon uses it to create the initial `admin` account. Later starts keep the existing account password. The supplied systemd unit and the Docker command below read `/etc/reposync/env`. For a direct run, export the variable in the invoking shell.

**As a systemd service:**
```bash
sudo cp scripts/reposync.service /etc/systemd/system/
sudo systemctl enable reposync
sudo systemctl start reposync
```

**With Docker:**
```bash
docker run -d \
  --name reposync \
  -p 8080:8080 \
  -v /etc/reposync:/etc/reposync:ro \
  -v /var/lib/reposync:/var/lib/reposync \
  --env-file /etc/reposync/env \
  ghcr.io/chriscase/reposync:latest
```

**Directly:**
```bash
reposync-daemon --config /etc/reposync/config.toml
```

The daemon runs in the foreground and takes the environment variables named in the configuration from its shell. The supplied systemd service reads `/etc/reposync/env` for service launches. Use a second terminal for CLI commands and pass the same configuration path.

### Import the First Repository

Open the dashboard at the address configured by `[web].listen`. The generated configuration uses `http://127.0.0.1:3000` on the daemon host. The example below uses port 8080. Log in with the configured password or GitHub OAuth.

On a fresh Team database the daemon registers the SVN and Git repository from the configuration. As an administrator, open **Repositories**, select that registration, and find **SVN Import** on **Overview**. Use an empty Git repository for the new mirror. Choose **Start full history import** and wait for **SVN Import Complete**. Normal polling begins once RepoSync has verified the SVN revision and its Git commit. [Team import and verification](docs/getting-started.md#step-8-import-and-verify)

## Configuration

See [docs/configuration.md](docs/configuration.md) for the full configuration reference.

**Minimal config example (team mode):**
```toml
[daemon]
poll_interval_secs = 15
data_dir = "/var/lib/reposync"

[svn]
url = "https://svn.company.com/repos/project"
username = "sync-service"
password_env = "REPOSYNC_SVN_PASSWORD"

[github]
api_url = "https://github.company.com/api/v3"
repo = "org/project"
token_env = "REPOSYNC_GITHUB_TOKEN"

[identity]
mapping_file = "/etc/reposync/authors.toml"
email_domain = "company.com"

[web]
listen = "0.0.0.0:8080"
auth_mode = "simple"
admin_password_env = "REPOSYNC_ADMIN_PASSWORD"
```

## Author Mapping

RepoSync transparently maps identities between SVN and Git:

```toml
# authors.toml
[authors]
jsmith = { name = "John Smith", email = "jsmith@company.com" }
janedoe = { name = "Jane Doe", email = "jane.doe@company.com" }

[defaults]
email_domain = "company.com"
```

**SVN -> Git:** SVN username `jsmith` becomes Git author `John Smith <jsmith@company.com>`
**Git -> SVN:** Git author `John Smith` maps back to SVN user `jsmith`

The sync daemon is always visible as the Git committer for audit purposes.

## Architecture

```
+-------------------------------------------------+
|              RepoSync Daemon                    |
|                                                 |
|  SVN Watcher --> Sync Engine <-- Git Watcher    |
|  Identity Mapper   |   Conflict Resolution      |
|  Web UI (React) <--+   Notifications            |
|                                                 |
|  SQLite: commit map, conflicts, audit log       |
+---------+---------------------------+-----------+
          |                           |
     SVN Server              GitHub Enterprise
                             or GitHub.com
```

See [docs/architecture.md](docs/architecture.md) for the full technical design.

## CLI

### Team Mode Commands

```bash
reposync status                              # Show sync status
reposync conflicts list                      # List active conflicts
reposync conflicts resolve <id> --accept git # Resolve from CLI
reposync sync now                            # Trigger immediate sync
reposync identity lookup jsmith              # Look up one SVN user's Git identity
reposync audit --limit 20                    # Recent sync history
```

### Inspect an installed Team database before upgrading

Run the candidate CLI against a **consistent offline SQLite backup** from `legacy/team-install` or `legacy/team-install-hotfix`. These branches share the same database layout. Keep the original database and its sidecars together. A database file copied on its own from a live WAL-mode installation can omit committed history. Supply a checkpointed backup that includes those commits. Discarding journal files can invalidate a snapshot.

```bash
reposync upgrade-preflight --database /backups/team/reposync.db
```

The report identifies the installed v13 or v14 catalog and lists each registration by ID. It checks for retained Git/SVN checkpoints, mappings, sync records, revision-bearing audit entries and import progress. An exit status of zero means these checks found no retained or unattributed history. The command returns a failure status for missing historical identity, evidence of unknown origin or an unreadable, changed or unsupported snapshot. A poll timestamp alone is inconclusive. Both old daemons recorded one after empty and failed polls. Check the old deployment’s records for published work in this case. The command reads the supplied snapshot offline, without loading configuration or connecting to either remote.

For a pair with retained history, keep it paused and locate an independently attributable pre-upgrade SVN backup, its original service URL and branch creation or copy history, and the corresponding Git ref and full imported commits. A saved SVN working copy can corroborate the original URL and UUID. Assessing the live repository and authorizing recovery require a separate review. [Historical Team upgrade follow-up](https://github.com/chriscase/RepoSync/issues/316)

### Personal Mode Commands

```bash
reposync personal init                           # Interactive setup wizard
reposync personal import --full                  # Import SVN history to GitHub
reposync personal start                          # Start sync daemon
reposync personal stop                           # Stop sync daemon
reposync personal status                         # Show sync dashboard
reposync personal log                            # Show sync history
reposync personal pr-log                         # Show PR sync history
reposync personal doctor                         # Run health checks
reposync personal conflicts list                 # List conflicts
reposync personal conflicts resolve ID --accept git  # Resolve a conflict
```

## Development

### Test Environment

Spin up a complete isolated test environment with one command:

```bash
make test           # Unit tests only
make test-e2e       # E2E / integration tests (requires svn + svnadmin)
make test-all       # Unit tests + E2E / integration tests
```

### Building

```bash
cargo build                    # Debug build
cargo build --release          # Release build
cargo test                     # Unit tests
cargo clippy                   # Lint
```

### Project Structure

```
crates/
  core/       # Shared sync logic, SVN/Git clients, identity mapping, conflicts
  daemon/     # Server daemon binary (team mode)
  personal/   # Personal branch mode sync engine & binary
  web/        # Axum web server + REST API (team mode)
  cli/        # Command-line management tool (both modes)
web-ui/       # React frontend
tests/        # Integration & E2E tests with Docker Compose
docs/         # Documentation
  personal-branch/  # Personal branch mode docs
scripts/      # Install scripts, systemd units
```

## Contributing

Contributions are welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

MIT License. See [LICENSE](LICENSE) for details.
