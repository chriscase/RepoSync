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

Individual developer SVN-to-Git bridge. Runs on your laptop, imports SVN history into a GitHub repo, and uses a PR-based workflow so every SVN sync is reviewable before merging. No server required. This is the right choice when you want to work in Git personally while your team stays on SVN.

See [docs/personal-branch/](docs/personal-branch/) for the full Personal Branch Mode documentation.

## Features

- **Bidirectional sync** -- SVN commits appear in Git, Git pushes appear in SVN
- **Server daemon** -- runs on a VM, watches both repos, syncs automatically (team mode)
- **Monorepo subset sync** -- mirror selected SVN paths into dedicated Git repositories with Team projected snapshots or Personal projected history. [Setup and limits](docs/monorepo-subset-sync.md)
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

Get up and running in three commands:

```bash
reposync personal init                           # Interactive setup wizard
reposync personal import --full                  # Import SVN history to GitHub
reposync personal start                          # Start sync daemon
```

The `init` wizard walks you through connecting your SVN repo and GitHub repo, configuring branch names, and setting up your identity mapping. Once `import --full` completes, `start` launches a background sync daemon on your laptop that watches for new SVN commits and creates PRs in your GitHub repo.

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
```

**Docker:**
```bash
docker pull ghcr.io/chriscase/reposync:latest
```

### Configure

```bash
# Generate a default config file
reposync init --config /etc/reposync/config.toml

# Edit with your SVN and GitHub details
$EDITOR /etc/reposync/config.toml

# Set up author mappings
$EDITOR /etc/reposync/authors.toml
```

### Run

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

### Access the Dashboard

Open `http://your-server:8080` in your browser. Log in with the configured password or GitHub OAuth.

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
reposync sync now                            # Uses service TOML without the saved repository path policy
reposync identity list                       # Show author mappings
reposync audit --limit 20                    # Recent sync history
```

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
