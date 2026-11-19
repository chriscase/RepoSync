# Personal Branch Mode

## What Is Personal Branch Mode?

Personal Branch Mode is a lightweight, single-developer configuration of RepoSync. It lets you mirror an SVN repository to your own GitHub account, work on feature branches with pull requests, and have merged commits automatically sync back to SVN -- all from a daemon running on your local machine.

Instead of deploying a shared server with webhooks and identity mapping, Personal Branch Mode runs on your machine alongside your normal development tools. You get a full Git-native workflow (branches, PRs, code review) while your commits continue to land in SVN exactly where your team expects them.

## Who Is This For?

Personal Branch Mode is designed for individual developers who:

- Work on a team that uses SVN as the canonical source of truth
- Want to use Git branches, pull requests, and code review for their own work
- Need a private GitHub mirror for CI, backups, or just a better workflow
- Do not have the authority (or desire) to migrate the whole team to Git

If your entire team is ready to move to Git, look at the full [team deployment](../deployment.md) instead. Personal Branch Mode is for the situation where SVN is staying, and you want Git anyway.

## How It Differs from Team Mode

| Capability | Team Mode | Personal Branch Mode |
|---|---|---|
| Deployment | Server/VM daemon | Local daemon (your laptop/desktop) |
| Web dashboard | Yes (React UI via Axum) | No |
| Multi-user identity mapping | Yes (LDAP/file-based) | No (single SVN user) |
| Webhook receiver | Yes (GitHub webhooks) | No (polls GitHub API) |
| Conflict resolution UI | Web-based | CLI-based |
| Setup | Config files + server provisioning | Interactive CLI wizard |
| SVN hook required | `pre-revprop-change` | None |

Personal Branch Mode strips away everything you do not need when you are the only user. No server to maintain, no hooks to install on the SVN server, no ports to open. Run the setup wizard, import your SVN starting point, then start the local daemon.

## Data Flow

```
┌──────────┐         ┌─────────────────┐         ┌──────────┐
│          │  poll   │  RepoSync       │  push   │          │
│   SVN    │────────>│  Personal       │────────>│  GitHub  │
│  Server  │<────────│  Daemon         │<────────│  Repo    │
│          │  commit │  (your laptop)  │  PR API │          │
└──────────┘         └─────────────────┘         └──────────┘
```

**SVN to Git:** The daemon polls your SVN repository on a configurable interval (default: 30 seconds). When new revisions appear, it translates them into Git commits and pushes to your GitHub repository's `main` branch.

**Git to SVN:** When you merge a pull request on GitHub, the daemon detects the new commits on `main` via the GitHub API, translates them back into SVN commits, and pushes them to the SVN server under your SVN credentials. The original Git author and commit message are preserved.

## Feature Highlights

### Bidirectional Sync

SVN revisions flow into Git as commits on `main`. Merged PRs flow back to SVN as new revisions. Both directions run while the daemon is active.

### PR-Based Workflow

Create feature branches, open pull requests, request reviews, run CI -- the full GitHub workflow. When a PR is merged into `main`, the daemon picks up the resulting commits and syncs them to SVN. Merge commits, squash merges, and rebase merges are all supported.

### Automatic Echo Suppression

When the daemon pushes an SVN revision to GitHub, it records the mapping. When it later sees that same commit on `main` while polling GitHub, it recognizes the commit as its own and skips it. This prevents infinite sync loops without any manual bookkeeping.

### Interactive CLI Setup Wizard

Run `reposync personal init` to write your Personal TOML file. The wizard asks for repository URLs, developer identity, polling interval and import mode. For credentials, enter the **environment variable names** that will hold the SVN password and GitHub token. The import command connects to the repositories after you finish configuring them.

Before your first import, use an absolute `[personal].data_dir` and set the named environment variables. Run `reposync personal import --full` for revision history or `reposync personal import --snapshot` for the current tree. After the import completes, `reposync personal start --foreground` runs the polling loop in your terminal. [Personal quickstart](quickstart.md)

### Health Check Doctor Command

`reposync personal doctor` checks the local configuration and reports on the data directory, database, Git checkout, daemon and stored sync positions. Before the first import, the database and checkout may be absent.

### Conflict Detection and Resolution

If someone commits to SVN while you have unsynced PR merges (or vice versa), the daemon detects the divergence and pauses sync. It presents the conflict in your terminal with options to rebase your changes on top, take one side, or manually resolve.

### Auto-Create GitHub Repository

The initial import checks whether the configured GitHub repository exists. If it is missing and `[github].auto_create` is enabled, the importer requests its creation using the configured privacy setting.

### Crash Recovery

The daemon persists its state to a local SQLite database: a watermark of the last synced SVN revision, a watermark of the last synced Git commit SHA, and a full commit map linking SVN revisions to Git SHAs. If the daemon crashes or your machine restarts, it reads the watermarks on startup and resumes exactly where it left off. No duplicate commits, no missed revisions.

## Further Reading

- **[Quickstart](quickstart.md)** -- Go from zero to syncing in five minutes.
- **[Configuration](configuration.md)** -- All config options for `personal.toml`.
- **[Workflows](workflows.md)** -- Branching strategies, PR patterns, and CI integration.
- **[Troubleshooting](troubleshooting.md)** -- Common problems and how to fix them.
- **[Architecture](architecture.md)** -- How Personal Branch Mode works under the hood.
- **[FAQ](faq.md)** -- Frequently asked questions.
