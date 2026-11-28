# Personal Branch Mode - Frequently Asked Questions

## Privacy and Visibility

### Will my colleagues see my Git commits?

No. When you run `reposync personal init`, the GitHub repository is created as **private** by default (`private = true` in the config). Only you have access to the GitHub mirror. Your colleagues continue working in SVN as usual and never interact with your GitHub repo.

The only thing visible in SVN is when your merged PR commits are replayed back. Those commits appear under your normal SVN username, just as if you had committed directly to SVN. The commit messages include metadata trailers (Git SHA, PR number), but this is informational and does not expose the GitHub repository itself.

### Is my GitHub token stored securely?

Tokens and passwords are **never** stored directly in the configuration file. Instead, the config uses the `_env` pattern: you specify the **name** of an environment variable that holds the secret value.

```toml
[github]
token_env = "REPOSYNC_GITHUB_TOKEN"

[svn]
password_env = "REPOSYNC_SVN_PASSWORD"
```

At startup, the daemon reads the environment variables and resolves the actual values in memory. Store your secrets in a shell profile, a `.env` file with restricted permissions (`chmod 600`), or a secrets manager.

## SVN Edge Cases

### What if someone force-pushes to SVN?

SVN does not support force-push the way Git does, but history inconsistencies can arise if an administrator replays or reverts revisions, or if the repository is restored from an older backup.

When the daemon detects that the SVN HEAD revision is **lower** than the stored watermark, or that a previously synced revision now has different content, it enters the `Error` state and stops syncing. The audit log records the discrepancy.

To recover:

1. Run `reposync personal doctor` to see the diagnostic output
2. Investigate the SVN history to understand what changed
3. If needed, reset the watermarks with a fresh import: `reposync personal import --reset`

### Can I use multiple SVN branches?

Currently, each personal mode configuration supports **one SVN path** (typically a trunk URL). If you need to sync multiple SVN branches, run separate daemon instances with separate config files and data directories:

```bash
reposync personal start --config ~/project-trunk.toml
reposync personal start --config ~/project-branch-v2.toml
```

Each instance maintains its own database, watermarks, and Git repository clone.

### What about binary files?

Binary files (images, compiled assets, archives, etc.) are copied as-is during sync. They pass through the SVN export and Git commit process just like text files.

However, **binary files cannot be 3-way merged**. If the same binary file is modified on both sides between sync cycles, a conflict is created and you must choose one version:

```bash
reposync personal conflicts resolve <id> --accept svn   # keep SVN version
reposync personal conflicts resolve <id> --accept git   # keep Git version
```

There is no "manual edit" option for binary conflicts. Use `max_file_size` in `[options]` to skip files above a certain size, and `ignore_patterns` to exclude specific paths entirely. Both options are **enforced at runtime** across all sync paths (import, SVN→Git, Git→SVN). Blocked files produce structured log warnings and `file_policy_skip` audit entries — there is no silent pass-through.

### What happens if I delete a file in Git?

When you delete a file on a Git branch and merge the PR, the deletion is synced to SVN on the next cycle. The `GitToSvnSync` engine detects that the file exists in the SVN working copy but not in the Git repository, removes it from disk, and then runs `svn rm` to stage the deletion before committing.

### What if someone force-pushes to SVN (admin revprop change)?

If an SVN administrator changes revision properties (like `svn:log` or `svn:author`) on an already-synced revision, it does not affect the sync engine. The watermark is based on revision numbers, not content hashes. The commit_map records the state at the time of sync. If you need the updated metadata reflected in Git, you would need to manually amend the corresponding Git commit.

## GitHub Configuration

### Can I use this with GitHub Enterprise?

Yes. Set the `api_url` in your configuration to point to your GitHub Enterprise instance:

```toml
[github]
api_url = "https://github.yourcompany.com/api/v3"
repo = "yourname/project-mirror"
token_env = "REPOSYNC_GITHUB_TOKEN"
```

The GitHub API client uses this URL for all requests. Generate a personal access token on your GHE instance with `repo` scope.

### How fast does sync happen?

The sync speed depends on two factors:

1. **Polling interval**: Configurable via `poll_interval_secs` in the `[personal]` section. Default is **30 seconds**. You can set it lower for faster sync or higher to reduce API calls.

2. **Processing time**: Each SVN revision requires an `svn export`, file copy, Git commit, and `git push`. Each PR replay requires an `svn update`, file copy, and `svn commit`. Typical processing time is 1-5 seconds per revision, depending on the number and size of changed files.

In practice, changes appear on the other side within one polling interval plus processing time, so roughly 30-60 seconds with the default settings.

### What is the PR workflow?

In personal mode, the Git-to-SVN direction is **PR-gated**:

1. The daemon mirrors SVN commits to the `main` branch of your private GitHub repo
2. You create feature branches and make changes using Git workflows
3. You open a pull request against `main` on your private repo
4. You merge the PR (using any strategy: merge, squash, or rebase)
5. The daemon detects the merged PR and replays its commits to SVN

Direct pushes to `main` are **not** synced to SVN. The `sync_direct_pushes` option exists in config but is **not yet implemented** — setting it to `true` will cause a validation error at startup. The PR workflow gives you a review checkpoint before changes go back to SVN.

## Platform Support

### Does this work on Windows?

Yes, with the following requirements:

- **SVN CLI** (`svn.exe`) must be installed and on your PATH (e.g., TortoiseSVN command-line tools or SlikSVN)
- **Git** must be installed and on your PATH
- The daemon runs as a background process (no Windows service support yet; use Task Scheduler or run in a terminal)

The data directory defaults to a platform-appropriate location via the `dirs` crate. On Windows this is typically `%APPDATA%\reposync\`.

### Does this work on macOS / Linux?

Yes. On Unix systems, the daemon supports proper signal handling (`SIGTERM`, `SIGINT`, `SIGHUP`) and writes a PID file for process management. Install SVN and Git via your package manager (Homebrew on macOS, apt/dnf on Linux).

## Migration and Compatibility

### How do I migrate from git-svn?

RepoSync is **not** a drop-in replacement for `git-svn`. It uses a different approach (full SVN exports rather than `git-svn fetch`), and its commit history starts fresh.

Recommended migration path:

1. Stop using `git-svn` (no more `git svn dcommit`)
2. Run `reposync personal init` and `reposync personal import` to create a fresh mirror from SVN
3. Move your in-progress work from your old git-svn repo to branches on the new GitHub mirror
4. Continue using the PR workflow from there

Attempting to reuse a `git-svn` repository as the RepoSync data directory is not supported and will likely cause watermark and commit_map inconsistencies.

## Multi-User Scenarios

### Can two people run personal mode on the same SVN repo?

Yes. Each person has their own:

- Private GitHub repository (separate mirror)
- Local daemon instance
- Local database with independent watermarks and commit_map

SVN commits from each person are independent. If Alice and Bob both run personal mode:

- Alice's SVN commits appear in Alice's GitHub mirror
- Bob's SVN commits appear in Bob's GitHub mirror
- When Alice merges a PR, her daemon commits to SVN under her username
- When Bob merges a PR, his daemon commits to SVN under his username

There is no cross-talk between their GitHub mirrors. Conflicts only arise if they both modify the same SVN files between sync cycles, which is the same conflict scenario as two people committing to SVN normally.

### What is the difference between personal and team mode?

| Aspect              | Personal Mode                   | Team Mode                          |
|---------------------|--------------------------------|-------------------------------------|
| **Runs on**         | Your laptop or personal VM     | A shared server or VM              |
| **Users**           | Single developer               | Entire team                        |
| **GitHub repo**     | Private, owned by you          | Shared org repo                    |
| **Git-to-SVN gate** | PR merge on your private repo | Direct sync |
| **Identity mapping**| Single developer config        | LDAP or mapping file (all team members) |
| **Conflict resolution** | CLI only                  | Web dashboard + CLI + notifications |
| **Web UI**          | None (optional status port)    | Full React dashboard               |
| **Notifications**   | None                           | Slack + email                      |
| **Process model**   | Background daemon with PID file| systemd service or Docker container|
| **Database**        | Local SQLite                   | Local SQLite (same engine)         |

Personal mode is designed for individual developers who want to use Git/GitHub workflows while their team uses SVN. Team mode is designed for organizations migrating an entire repository with multiple contributors.
