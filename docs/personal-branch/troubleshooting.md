# Personal Branch Mode: Troubleshooting

Common issues and solutions for RepoSync Personal Branch Mode.

## Table of Contents

- [Quick Diagnostics with Doctor](#quick-diagnostics-with-doctor)
- [Sync Stopped Working](#sync-stopped-working)
- [Conflict Won't Resolve](#conflict-wont-resolve)
- [GitHub Push Rejected](#github-push-rejected)
- [Network Errors and Timeouts](#network-errors-and-timeouts)
- [SVN Authentication Failed](#svn-authentication-failed)
- [Database is Corrupted](#database-is-corrupted)
- [Daemon Won't Start](#daemon-wont-start)
- [PR Not Syncing to SVN](#pr-not-syncing-to-svn)
- [Files Skipped by Policy](#files-skipped-by-policy)
- [How to Reset Sync State](#how-to-reset-sync-state)
- [Log File Location and Verbosity](#log-file-location-and-verbosity)
- [Getting Help](#getting-help)

---

## Quick Diagnostics with Doctor

The `doctor` command runs a comprehensive health check on your personal branch setup. Start here whenever something seems wrong:

```bash
reposync personal doctor
```

Example output:

```
RepoSync Doctor
═════════════════

  [OK]  Configuration     Valid
  [OK]  Data Directory    /home/user/.local/share/reposync
  [OK]  Database          OK
  [OK]  Git Repository    /home/user/.local/share/reposync/git-repo
  [OK]  SVN Working Copy  /home/user/.local/share/reposync/svn-wc
  [!!]  Daemon            Not running
  [OK]  Watermarks        SVN: r1042, Git: a3f7b2c

  ! 1 issue(s) found:
    1. Start daemon with: reposync personal start
```

The doctor checks:

1. **Configuration** -- validates the TOML config file
2. **Data directory** -- verifies the directory exists and is writable
3. **Database** -- opens `personal.db` and checks the schema
4. **Git repository** -- confirms the local clone at `data_dir/git-repo` exists
5. **SVN working copy** -- checks for `data_dir/svn-wc` (created on first Git-to-SVN sync)
6. **Daemon status** -- verifies the daemon process is alive
7. **Watermark consistency** -- ensures both SVN and Git watermarks are set and consistent

## Sync Stopped Working

**Symptom**: No new SVN commits appearing in Git, or merged PRs not syncing to SVN.

**Step 1**: Run the doctor to identify the issue:

```bash
reposync personal doctor
```

**Step 2**: Check if the daemon is running:

```bash
reposync personal status
```

If it shows "Stopped", restart the daemon:

```bash
reposync personal start
```

**Step 3**: Check the log file for errors:

```bash
tail -50 ~/.local/share/reposync/personal.log
```

**Step 4**: Run the daemon in foreground mode to observe live output:

```bash
reposync personal stop
reposync personal start --foreground
```

**Step 5**: Check for unresolved conflicts blocking sync:

```bash
reposync personal conflicts list
```

If conflicts exist, resolve them (see [Handling Conflicts](workflows.md#handling-conflicts)) and the sync should resume.

**Step 6**: Verify SVN and GitHub connectivity:

```bash
# Test SVN access
svn info --username your_username https://svn.example.com/repos/project/trunk

# Test GitHub API access
curl -H "Authorization: token $REPOSYNC_GITHUB_TOKEN" https://api.github.com/user
```

## Conflict Won't Resolve

**Symptom**: Running `reposync personal conflicts resolve` fails or the conflict keeps reappearing.

**Common causes**:

1. **Invalid conflict ID**: The conflict ID shown in the list may be truncated. Use the full ID:

   ```bash
   reposync personal conflicts list
   # Note the full ID from the table, then resolve:
   reposync personal conflicts resolve abc12345-full-id --accept git
   ```

2. **Invalid resolution value**: The `--accept` flag only accepts `svn` or `git`:

   ```bash
   # Correct:
   reposync personal conflicts resolve abc12345 --accept svn
   reposync personal conflicts resolve abc12345 --accept git

   # Wrong:
   reposync personal conflicts resolve abc12345 --accept mine
   ```

3. **Conflict reappears after resolution**: If new changes arrive to the same file on both sides before the resolved version is applied, a new conflict is created. This is expected behavior. Resolve the new conflict, and coordinate with your team to avoid concurrent edits to the same file.

**Manual resolution steps** (if automated resolution is not working):

1. Stop the daemon: `reposync personal stop`
2. Identify the conflicting file from `reposync personal conflicts list`
3. Manually inspect the SVN and Git versions of the file
4. Decide which version to keep and resolve via CLI
5. Restart the daemon: `reposync personal start`

## GitHub Push Rejected

**Symptom**: Logs show "failed to push Git commit" or "rejected" errors.

**Possible causes**:

### Force-push detection / branch protection

If your GitHub repo has branch protection rules on the default branch, the daemon's push may be rejected. The daemon performs fast-forward pushes only and never force-pushes.

**Fix**: Ensure the GitHub token has permission to push to the default branch, and that branch protection rules allow pushes from your account or the token's associated identity.

### Watermark mismatch

If the Git watermark in the database points to a commit that no longer exists on `origin/main` (for example, after a force-push or history rewrite on the remote), the daemon cannot push because the histories have diverged.

**Fix**: Reset the sync state (see [How to Reset Sync State](#how-to-reset-sync-state)).

### Token permission issues

The GitHub token must have `repo` scope (or `contents:write` for fine-grained tokens).

**Verify**:

```bash
curl -H "Authorization: token $REPOSYNC_GITHUB_TOKEN" https://api.github.com/user
```

If the response shows the correct user, but pushes still fail, check token scopes:

```bash
curl -sI -H "Authorization: token $REPOSYNC_GITHUB_TOKEN" https://api.github.com/user | grep x-oauth-scopes
```

### Repository does not exist

If `auto_create = false` and the repository hasn't been created, pushes will fail.

**Fix**: Either create the repository manually on GitHub, or set `auto_create = true` in the config and re-run the import.

## Network Errors and Timeouts

**Symptom**: Logs show connection timeouts, DNS resolution failures, or HTTP errors.

### SVN connectivity

```bash
# Test basic connectivity
svn info https://svn.example.com/repos/project/trunk

# If behind a proxy
export http_proxy=http://proxy.company.com:8080
export https_proxy=http://proxy.company.com:8080
svn info https://svn.example.com/repos/project/trunk
```

### GitHub API connectivity

```bash
# Test API access
curl -v -H "Authorization: token $REPOSYNC_GITHUB_TOKEN" https://api.github.com/rate_limit
```

Check the response for:

- **401 Unauthorized**: Token is invalid or expired (see next section)
- **403 Forbidden**: Rate limit exceeded or IP blocked
- **Connection refused**: Firewall or network issue

### Token expiration

GitHub personal access tokens can expire. If you are using a fine-grained token with an expiration date, generate a new token and update the environment variable:

```bash
export REPOSYNC_GITHUB_TOKEN=ghp_new_token_here
```

Then restart the daemon:

```bash
reposync personal stop && reposync personal start
```

### Intermittent failures

The daemon retries on the next polling cycle automatically. Transient network errors are logged but do not crash the daemon. If both SVN-to-Git and Git-to-SVN phases encounter errors in the same cycle, both are logged independently and the daemon continues polling.

## SVN Authentication Failed

**Symptom**: Logs show "SVN authentication failed" or SVN commands return authorization errors.

**Step 1**: Verify the credentials work manually:

```bash
svn info --username your_username --password "$REPOSYNC_SVN_PASSWORD" \
  https://svn.example.com/repos/project/trunk
```

**Step 2**: Check that the environment variable is set:

```bash
echo $REPOSYNC_SVN_PASSWORD
```

If the password is not set, the daemon starts but cannot authenticate. Set the variable and restart:

```bash
export REPOSYNC_SVN_PASSWORD='your-password-here'
reposync personal stop && reposync personal start
```

**Step 3**: Verify the `password_env` field in your config points to the correct variable name:

```toml
[svn]
url = "https://svn.example.com/repos/project/trunk"
username = "your_username"
password_env = "REPOSYNC_SVN_PASSWORD"
```

The `password_env` field specifies the *name* of the environment variable, not the password itself. The password is never stored in the config file.

**Step 4**: Check for special characters in the password. If your SVN password contains shell-special characters (`!`, `$`, `` ` ``, `\`), make sure to use single quotes when setting the environment variable:

```bash
export REPOSYNC_SVN_PASSWORD='p@ss!w0rd$pecial'
```

**Step 5**: If the SVN server requires certificate acceptance, run an SVN command manually first to accept the certificate, then restart the daemon:

```bash
svn info --username your_username https://svn.example.com/repos/project/trunk
# Accept the certificate when prompted (press 'p' for permanent)
```

## Database is Corrupted

**Symptom**: Errors mentioning "database disk image is malformed", "database is locked", or schema-related failures.

### Option A: Attempt recovery

```bash
# Stop the daemon
reposync personal stop

# Locate the database
ls -la ~/.local/share/reposync/personal.db

# Try SQLite recovery
sqlite3 ~/.local/share/reposync/personal.db ".recover" | \
  sqlite3 ~/.local/share/reposync/personal.db.recovered

# Replace the original
mv ~/.local/share/reposync/personal.db ~/.local/share/reposync/personal.db.corrupt
mv ~/.local/share/reposync/personal.db.recovered ~/.local/share/reposync/personal.db

# Restart
reposync personal start
```

### Option B: Delete and re-import

If recovery fails, delete the database and re-import from SVN. This rebuilds all sync state from scratch:

```bash
# Stop the daemon
reposync personal stop

# Remove the corrupted database
rm ~/.local/share/reposync/personal.db

# Re-import (snapshot for speed, or full for complete history)
reposync personal import --snapshot

# Restart
reposync personal start
```

After re-import, the watermarks are reset to the current SVN HEAD. Any PRs merged before the re-import that were not yet synced to SVN will need to be manually replayed or re-merged.

### Preventing corruption

Database corruption is rare but can occur from:

- Power loss or hard shutdown during a write
- Disk full conditions
- Running multiple daemon instances simultaneously (check for stale PID files)

The database uses SQLite with WAL mode for crash resilience, but abrupt termination during a write can still cause issues in extreme cases.

## Daemon Won't Start

**Symptom**: `reposync personal start` reports an error or exits immediately.

### Config validation error (`sync_direct_pushes`)

If you see an error like:

```
Error: invalid config: options.sync_direct_pushes: sync_direct_pushes is not yet implemented in personal mode
```

This means your config has `sync_direct_pushes = true`, which is not yet supported. Remove the line or set it to `false`:

```toml
[options]
sync_direct_pushes = false   # only merged PRs are synced (default)
```

### Stale PID file

If the daemon crashed or was killed without cleanup, a stale PID file may block startup:

```bash
reposync personal status
```

If status shows "Running" but the daemon is not actually alive, the PID file is stale. The doctor command detects and cleans up stale PID files automatically:

```bash
reposync personal doctor
```

To manually clean up:

```bash
# Find the PID file
cat ~/.local/share/reposync/personal.pid

# Check if the process is actually running
ps -p $(cat ~/.local/share/reposync/personal.pid)

# If the process is not running, remove the stale PID file
rm ~/.local/share/reposync/personal.pid

# Now start the daemon
reposync personal start
```

### Already running

If the daemon is already running, the start command reports it and exits without starting a second instance:

```
Daemon is already running (PID 12345)
```

Stop the existing daemon first if you want to restart:

```bash
reposync personal stop && reposync personal start
```

### Config file not found

If the config file does not exist at the expected path:

```
Error: failed to load personal config: file not found: /home/user/.config/reposync/personal.toml
```

Either create the config with the init wizard or specify the correct path:

```bash
# Create a new config
reposync personal init

# Or specify a custom path
reposync personal --personal-config /path/to/config.toml start
```

### Missing data directory

If the data directory does not exist, the daemon creates it automatically on start. If this fails due to permissions:

```bash
mkdir -p ~/.local/share/reposync
```

### Database not initialized

If you haven't run the initial import, the daemon may fail to start because there is no database:

```bash
reposync personal import --snapshot
reposync personal start
```

## PR Not Syncing to SVN

**Symptom**: A PR was merged on GitHub but the changes have not appeared in SVN.

**Step 1**: Verify the PR was merged to the default branch. The daemon only watches for PRs merged into the branch configured as `default_branch` (typically `main`):

```toml
[github]
default_branch = "main"
```

PRs merged to other branches are ignored.

**Step 2**: Check the PR sync log:

```bash
reposync personal pr-log
```

Look for the PR number. Possible statuses:

- **synced**: The PR was successfully replayed to SVN.
- **pending**: The PR has been detected but not yet processed.
- **failed**: The PR sync failed. Check the sync log for the error:

  ```bash
  reposync personal log --limit 50
  ```

**Step 3**: Check if the daemon is running and poll timing:

```bash
reposync personal status
```

The daemon polls GitHub for merged PRs every `poll_interval_secs` (default 30 seconds). If you just merged the PR, wait for at least one polling cycle.

**Step 4**: Ensure the PR's commits are not all echo commits. If every commit in the PR contains the `[reposync]` sync marker (meaning they all originated from SVN), the daemon correctly skips the PR to avoid echo loops.

**Step 5**: Run in foreground mode to observe the next sync cycle:

```bash
reposync personal stop
reposync personal start --foreground
```

Watch the output for messages about the PR being detected, skipped, or encountering errors.

## Files Skipped by Policy

**Symptom**: Certain files in SVN do not appear in your Git mirror, or files from merged PRs are not committed to SVN. The log contains warnings like `file_policy_skip`.

The daemon enforces two file-level policies from the `[options]` section of your config:

- **`max_file_size`**: Files exceeding this byte limit are skipped. Set to `0` (default) to disable the size limit.
- **`ignore_patterns`**: Files matching any glob pattern in this list are skipped. Empty list (default) means no patterns.

These policies are enforced across all sync paths: initial import, SVN-to-Git, and Git-to-SVN.

**Step 1**: Check the audit log for skip events:

```bash
sqlite3 ~/.local/share/reposync/personal.db \
  "SELECT timestamp, action, details FROM audit_log WHERE action = 'file_policy_skip' ORDER BY timestamp DESC LIMIT 20;"
```

Each entry includes the file path, the reason (oversize or ignored pattern), and which sync direction triggered the skip.

**Step 2**: Review your config policy:

```toml
[options]
max_file_size = 10485760   # 10 MB -- files larger than this are skipped
ignore_patterns = ["*.log", "build/**", "*.tmp"]
```

**Step 3**: To allow a previously blocked file, adjust the policy and re-import:

```bash
# Edit your config to raise the size limit or remove the pattern
# Then re-import to pick up the file
reposync personal import --snapshot
```

Files blocked by policy are never silently passed through. Every skip produces a structured log warning and an audit entry.

## How to Reset Sync State

If sync state becomes inconsistent, stop the writer first. After RepoSync has already published commits to Git or SVN, do not treat a local database edit as rollback. Stop, reconcile, and roll forward under the rules in [`docs/enterprise-soak-runbook.md`](../enterprise-soak-runbook.md). Do not move a stored cursor backward, and do not restore an older database over those published commits.

A full local wipe below is only for a disposable personal data directory you are willing to re-import. It is not a production recovery step, and it is not qualified enterprise rollback.

### Full reset (disposable local data only)

```bash
# Stop the daemon
reposync personal stop

# Back up the existing database (optional)
cp ~/.local/share/reposync/personal.db ~/.local/share/reposync/personal.db.bak

# Delete the database
rm ~/.local/share/reposync/personal.db

# Optionally delete the Git repo and SVN working copy for a clean slate
rm -rf ~/.local/share/reposync/git-repo
rm -rf ~/.local/share/reposync/svn-wc

# Re-import
reposync personal import --snapshot   # fast: single commit from SVN HEAD
# OR
reposync personal import --full       # slow: replay all SVN history

# Restart
reposync personal start
```

### Cursor does not match the working copies

Stop the daemon and compare the recorded positions with the actual Git and SVN history. Reconcile from those external revisions. Do not hand-edit cursor rows to skip or replay history. A wrong cursor is a data-integrity problem: fixing it by guessing a revision can duplicate or drop commits.

## Log File Location and Verbosity

### Log file location

The log file is located in the data directory:

```
~/.local/share/reposync/personal.log
```

If you configured a custom `data_dir` in your config, the log file is at `{data_dir}/personal.log`.

The daemon also prints the log file path when it starts in background mode.

### Viewing logs

```bash
# View recent logs
tail -100 ~/.local/share/reposync/personal.log

# Follow logs in real time
tail -f ~/.local/share/reposync/personal.log
```

### Increasing verbosity

Set the log level in your config file:

```toml
[personal]
log_level = "debug"    # options: trace, debug, info, warn, error
```

For maximum verbosity, use `trace`. For normal operation, `info` is recommended.

Alternatively, override the log level with the `RUST_LOG` environment variable when running in foreground mode:

```bash
RUST_LOG=trace reposync personal start --foreground
```

The `RUST_LOG` variable supports per-module filtering:

```bash
# Debug sync engine, info for everything else
RUST_LOG=info,reposync_personal::engine=debug reposync personal start --foreground

# Trace SVN client operations
RUST_LOG=info,reposync_core::svn=trace reposync personal start --foreground
```

## Getting Help

- Run diagnostics: `reposync personal doctor`
- Check status: `reposync personal status`
- View sync history: `reposync personal log`
- View PR history: `reposync personal pr-log`
- File an issue: https://github.com/chriscase/RepoSync/issues
