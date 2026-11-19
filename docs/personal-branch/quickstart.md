# Personal Branch Mode: 5-Minute Quickstart

Personal Branch Mode lets a single developer mirror an SVN repository to a personal GitHub repo. You work in Git with branches and PRs, and RepoSync handles syncing changes back to SVN when PRs are merged. No server required -- it runs as a local daemon on your machine.

## Prerequisites

Before you begin, make sure you have:

- **SVN CLI** (`svn`) version 1.8+ installed and on your PATH
- **Git** version 2.20+ installed
- **Rust toolchain** (if building from source) -- install via [rustup](https://rustup.rs/)
- **A GitHub account** with a repository to use as the mirror, or permission to create one during import
- **A GitHub Personal Access Token** with `repo` scope -- [create one here](https://github.com/settings/tokens)
- **SVN credentials** for the repository you want to mirror

## Step 1: Install

### Option A: Install from crates.io

```bash
cargo install reposync-cli
```

### Option B: Build from source

```bash
git clone https://github.com/chriscase/RepoSync.git
cd RepoSync
cargo build --release
# Copy the binary to your PATH
cp target/release/reposync ~/.cargo/bin/
```

Verify the installation:

```bash
reposync --version
```

## Step 2: Initialize Configuration

Run the interactive wizard to write `~/.config/reposync/personal.toml`.

```bash
reposync personal init
```

Enter the SVN URL and username, GitHub API URL and repository, developer identity, polling interval and import mode. When prompted for credentials, enter the names of the environment variables that hold them. The defaults are `REPOSYNC_SVN_PASSWORD` and `REPOSYNC_GITHUB_TOKEN`. Set the corresponding values in the environment used for the import and daemon.

For a new installation, set `data_dir` under the existing `[personal]` section to an absolute path you own, for example `data_dir = "/home/jdoe/.local/share/reposync"`. A leading `~/` in TOML names a literal `~` directory under the working directory. For an existing installation, keep using the directory containing its `personal.db` and `git-repo`.

To use a custom config file, write it with `reposync personal init --output "/path/to/personal.toml"`. On subsequent commands put `--personal-config "/path/to/personal.toml"` immediately after `personal`, before the action.

## Step 3: Import SVN History

Choose the command that matches your wizard selection. To import the full revision history, run this command.

```bash
reposync personal import --full
```

For the wizard’s default snapshot selection, run `reposync personal import --snapshot` to create a single starting commit from the current SVN tree. The import connects to SVN and GitHub and reports the imported commit count when it finishes.

## Step 4: Start the Sync Daemon

After the import completes, start the polling loop in a terminal and leave it running.

```bash
reposync personal start --foreground
```

From a second terminal with the same config and environment, inspect the daemon and its stored sync positions.

```bash
reposync personal status
```

When you are ready to stop syncing, press Ctrl-C in the first terminal or run `reposync personal stop` in the second.

## Step 5: Work with Branches and PRs

Now that sync is running, use your normal Git workflow:

### 1. Clone your mirror repository

```bash
git clone git@github.com:jdoe/project-mirror.git
cd project-mirror
```

### 2. Create a feature branch

```bash
git checkout -b feature/add-login-page
```

### 3. Make changes and commit

```bash
# Edit files...
git add .
git commit -m "Add login page with form validation"
```

### 4. Push and open a Pull Request

```bash
git push -u origin feature/add-login-page
```

Open a PR on GitHub through the web UI or the CLI:

```bash
gh pr create --title "Add login page" --body "Implements the login form with validation"
```

### 5. Merge the PR

Once you are satisfied, merge the PR on GitHub (via the web UI or CLI). RepoSync detects the merge and commits the changes to SVN automatically.

### 6. Watch it sync

Within 30 seconds (or your configured poll interval), the merged changes appear as a new SVN revision:

```
SVN r848: Add login page with form validation
  Git-Commit: 7e2f9a1
  PR: #12 (feature/add-login-page)
  Merged-By: jdoe
```

Meanwhile, if someone else commits directly to SVN, those changes appear in your GitHub repo's `main` branch within the same poll interval.

## What to Expect

- **First sync latency**: new SVN commits and merged PRs sync within your configured poll interval (default 30 seconds).
- **SVN commit metadata**: when Git commits are synced to SVN, the SVN commit message includes trailers with the Git SHA, PR number, and branch name.
- **Git commit metadata**: when SVN commits are synced to Git, the Git commit message includes trailers with the SVN revision number, author, and timestamp.
- **Only merged PRs are synced to SVN**: direct pushes to the default branch are ignored. This prevents work-in-progress pushes from polluting SVN. (The `sync_direct_pushes` option exists in the config schema but is **not yet implemented**; the daemon rejects `sync_direct_pushes = true` at startup.)
- **No SVN server changes needed**: Personal Branch Mode uses your regular SVN credentials. There are no hooks or admin access required on the SVN side.

## Next Steps

- Read the full [Configuration Reference](configuration.md) to customize commit templates, ignore patterns, and more.
- See the main [Troubleshooting Guide](../troubleshooting.md) if anything goes wrong.
