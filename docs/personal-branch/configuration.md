# Personal Branch Mode: Configuration Reference

Personal Branch Mode is configured via a TOML file. The default location is:

```
~/.config/reposync/personal.toml
```

You can override the config file path with the `--config` flag on any command:

```bash
reposync personal start --config /path/to/my-config.toml
```

Or set the `REPOSYNC_CONFIG` environment variable:

```bash
export REPOSYNC_CONFIG="/path/to/my-config.toml"
```

## Full Configuration File

```toml
[personal]
data_dir = "~/.local/share/reposync"
poll_interval_secs = 30

[svn]
url = "https://svn.company.com/repos/project/trunk"
username = "jdoe"
# Use ONE of the following (password_env is recommended):
# password = "secret"
password_env = "SVN_PASSWORD"

[github]
api_url = "https://api.github.com"
# Use ONE of the following (token_env is recommended):
# token = "ghp_xxxx"
token_env = "GITHUB_TOKEN"
repo = "jdoe/project-mirror"

[developer]
name = "John Doe"
email = "jdoe@company.com"
svn_username = "jdoe"

[commit_format]
# svn_to_git = "..."  # uses sensible defaults
# git_to_svn = "..."  # uses sensible defaults

[options]
normalize_line_endings = true
sync_executable_bit = true
# max_file_size = 0              # 0 = no limit (default)
# ignore_patterns = []
# sync_direct_pushes = false     # not yet implemented — leave false
auto_merge = true
# lfs_threshold = 0              # 0 = LFS disabled (default)
# lfs_patterns = []              # e.g. ["*.psd", "*.bin"]
```

---

## Section Reference

### [personal]

General settings for the Personal Branch Mode daemon.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `data_dir` | string | `"~/.local/share/reposync"` | Directory where RepoSync stores its SQLite database, cloned repositories, and sync state. Tilde (`~`) is expanded to your home directory. |
| `poll_interval_secs` | integer | `30` | How often (in seconds) the daemon checks SVN for new revisions and GitHub for merged PRs. Lower values give faster sync but increase load on both servers. |
| `log_level` | string | `"info"` | Minimum tracing level: `trace`, `debug`, `info`, `warn`, `error`. Applied as the default filter when the `RUST_LOG` environment variable is not set. If `RUST_LOG` is set, it takes precedence. |

#### Log file

The daemon writes structured log output to **`{data_dir}/personal.log`** in addition to stderr.
The file is appended across daemon restarts (not rolled automatically). For long-running
deployments, use external log rotation (e.g. `logrotate`).

### [svn]

Connection details for the SVN repository you want to mirror.

| Key | Type | Required | Default | Description |
|-----|------|----------|---------|-------------|
| `url` | string | yes | -- | Full SVN URL to mirror. This should point to the specific path you want (e.g., `trunk`, a branch, or the repo root). |
| `username` | string | yes | -- | Your SVN username. |
| `password` | string | no | -- | SVN password in plaintext. **Not recommended** -- use `password_env` instead. If both are set, `password` takes precedence. |
| `password_env` | string | no | -- | Name of an environment variable that contains your SVN password. This is the recommended approach. |

You must provide either `password` or `password_env`. If neither is set, the daemon will fail to start.

### [github]

Connection details for the GitHub repository used as the Git mirror.

| Key | Type | Required | Default | Description |
|-----|------|----------|---------|-------------|
| `api_url` | string | no | `"https://api.github.com"` | GitHub API base URL. Use `https://api.github.com` for GitHub.com. For GitHub Enterprise Server, use `https://github.yourcompany.com/api/v3`. |
| `token` | string | no | -- | GitHub Personal Access Token in plaintext. **Not recommended** -- use `token_env` instead. Requires `repo` scope. If both are set, `token` takes precedence. |
| `token_env` | string | no | -- | Name of an environment variable that contains your GitHub token. This is the recommended approach. |
| `repo` | string | yes | -- | Target GitHub repository in `owner/name` format (e.g., `jdoe/project-mirror`). |

You must provide either `token` or `token_env`. If neither is set, the daemon will fail to start.

### [developer]

Your identity for commits created by the sync process.

| Key | Type | Required | Default | Description |
|-----|------|----------|---------|-------------|
| `name` | string | yes | -- | Your full name, used as the Git commit author name when syncing SVN commits to Git. |
| `email` | string | yes | -- | Your email address, used as the Git commit author email. |
| `svn_username` | string | yes | -- | Your SVN username. Used to identify which SVN commits are yours when syncing. Should match the `username` in `[svn]` for single-user setups. |

### [commit_format]

Templates that control how commit messages are formatted when crossing between systems.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `svn_to_git` | string | (see below) | Template for Git commit messages created from SVN revisions. |
| `git_to_svn` | string | (see below) | Template for SVN commit messages created from merged Git PRs. |

#### SVN-to-Git Template Placeholders

| Placeholder | Description |
|-------------|-------------|
| `{original_message}` | The original SVN commit log message |
| `{svn_rev}` | The SVN revision number (e.g., `847`) |
| `{svn_author}` | The SVN commit author username |
| `{svn_date}` | The SVN commit timestamp in ISO 8601 format |

**Default template:**

```
{original_message}

SVN-Revision: r{svn_rev}
SVN-Author: {svn_author}
SVN-Date: {svn_date}
```

#### Git-to-SVN Template Placeholders

| Placeholder | Description |
|-------------|-------------|
| `{original_message}` | The original Git commit message (or PR merge commit message) |
| `{git_sha}` | The full Git commit SHA |
| `{pr_number}` | The GitHub PR number that was merged |
| `{pr_branch}` | The source branch name of the merged PR |

**Default template:**

```
{original_message}

Git-Commit: {git_sha}
PR: #{pr_number} ({pr_branch})
```

### [options]

Behavioral options that control how files and changes are synced.

| Key | Type | Default | Status | Description |
|-----|------|---------|--------|-------------|
| `normalize_line_endings` | boolean | `true` | Implemented | Normalize CRLF to LF during sync. |
| `sync_executable_bit` | boolean | `true` | Implemented | Sync the executable permission bit (`svn:executable` property) between SVN and Git. |
| `max_file_size` | integer | `0` | **Implemented** | Maximum file size in bytes. Files larger than this are skipped during sync with a warning and an audit log entry. `0` = no limit (default). Example: `52428800` for 50 MB. |
| `ignore_patterns` | array of strings | `[]` | **Implemented** | Glob patterns for files to exclude from sync. Patterns are matched against paths relative to the repository root. Example: `["*.log", "build/**", ".idea/**"]`. Matching files are skipped with a warning and audit entry. |
| `auto_merge` | boolean | `true` | Implemented | Automatically merge conflicts when a clean 3-way merge is possible. When disabled, all conflicts require manual resolution. |
| `sync_externals` | boolean | `false` | Not implemented | Reserved for future SVN externals support. |
| `sync_direct_pushes` | boolean | `false` | **Not implemented** | Reserved. Setting to `true` will cause validation to fail with an explicit error. Only merged PRs are synced from Git to SVN. |
| `lfs_threshold` | integer | `0` | **Implemented** | Files above this byte threshold are stored via Git LFS instead of as regular blobs. `0` = LFS disabled (default). Requires `git lfs` to be installed. Example: `5242880` for 5 MB. |
| `lfs_patterns` | array of strings | `[]` | **Implemented** | Glob patterns for files that should always be LFS-tracked regardless of size. Example: `["*.psd", "*.bin", "*.iso"]`. Files matching any pattern get `LfsTrack` treatment (added to `.gitattributes`, resolved from pointers during Git→SVN sync). Works independently of `lfs_threshold` — patterns alone enable LFS behavior. |

> **Important:** `max_file_size` and `ignore_patterns` are enforced at runtime across
> all sync paths (initial import, SVN→Git, Git→SVN). Blocked files produce structured
> log warnings and `file_policy_skip` audit entries. There is no silent pass-through.
>
> When `lfs_threshold` is set, the daemon runs a preflight check for `git lfs` at
> startup. If git-lfs is not installed, the import command fails with a clear error.
> During SVN→Git sync, files above the threshold are copied to the working tree and
> their extension is added to `.gitattributes` for LFS tracking. During Git→SVN sync,
> LFS pointer files are automatically resolved to their actual content before being
> committed to SVN.

---

## Environment Variables

Fields ending in `_env` tell RepoSync to read the actual value from an environment variable at startup. This keeps secrets out of config files.

| Config Field | Recommended Env Var | Description |
|--------------|-------------------|-------------|
| `svn.password_env` | `SVN_PASSWORD` | Your SVN password |
| `github.token_env` | `GITHUB_TOKEN` | Your GitHub Personal Access Token |

You can use any environment variable name you want. The config field value is the **name** of the variable, not the secret itself.

**Example:** if your config contains `password_env = "MY_SVN_PASS"`, then set the variable:

```bash
export MY_SVN_PASS="my-actual-password"
```

### Setting Environment Variables

Add these to your shell profile (`~/.bashrc`, `~/.zshrc`, etc.) so they persist across sessions:

```bash
# RepoSync secrets
export SVN_PASSWORD="your-svn-password"
export GITHUB_TOKEN="ghp_xxxxxxxxxxxxxxxxxxxx"
```

Reload your shell or run `source ~/.zshrc` before starting the daemon.

---

## Config File Location

The daemon searches for the config file in this order:

1. Path specified by `--config` flag
2. Path in the `REPOSYNC_CONFIG` environment variable
3. `~/.config/reposync/personal.toml` (default)

The `reposync personal init` wizard writes to the default location. The parent directory is created automatically if it does not exist.

---

## Example Configurations

### GitHub.com (minimal)

The simplest setup for mirroring an SVN trunk to a personal GitHub.com repo:

```toml
[personal]
poll_interval_secs = 30

[svn]
url = "https://svn.company.com/repos/project/trunk"
username = "jdoe"
password_env = "SVN_PASSWORD"

[github]
repo = "jdoe/project-mirror"
token_env = "GITHUB_TOKEN"

[developer]
name = "John Doe"
email = "jdoe@company.com"
svn_username = "jdoe"
```

All other fields use their defaults. The `api_url` defaults to `https://api.github.com`.

### GitHub Enterprise Server

For organizations running GitHub Enterprise on their own infrastructure:

```toml
[personal]
data_dir = "~/.local/share/reposync"
poll_interval_secs = 15

[svn]
url = "https://svn.internal.corp/repos/main-product/trunk"
username = "john.doe"
password_env = "SVN_PASSWORD"

[github]
api_url = "https://github.corp.com/api/v3"
repo = "john-doe/main-product-mirror"
token_env = "GHE_TOKEN"

[developer]
name = "John Doe"
email = "john.doe@corp.com"
svn_username = "john.doe"

[options]
ignore_patterns = [".svn/**", "*.class", "target/**"]
max_file_size = 104857600  # 100 MB
```

Note the custom `api_url` pointing to the Enterprise API endpoint.

### Custom SVN Layout

Some SVN repositories do not follow the standard `trunk/branches/tags` layout. If your project lives at a non-standard path, point the `url` directly to it:

```toml
[svn]
# Mirror only the "main" directory (no standard trunk/branches/tags)
url = "https://svn.company.com/repos/products/widget/main"
username = "jdoe"
password_env = "SVN_PASSWORD"
```

If your repository uses alternate names for trunk (e.g., the project root is the working directory itself, or the trunk is named something else), simply set the `url` to the exact path you want to sync.

### Aggressive Polling with Custom Commit Format

For workflows where low latency matters and you want minimal metadata in commit messages:

```toml
[personal]
poll_interval_secs = 10

[svn]
url = "https://svn.company.com/repos/project/trunk"
username = "jdoe"
password_env = "SVN_PASSWORD"

[github]
repo = "jdoe/project-mirror"
token_env = "GITHUB_TOKEN"

[developer]
name = "John Doe"
email = "jdoe@company.com"
svn_username = "jdoe"

[commit_format]
svn_to_git = "{original_message}\n\n(svn r{svn_rev})"
git_to_svn = "{original_message}\n\n(from {git_sha})"

[options]
normalize_line_endings = true
```

### Filtered Sync with Ignore Patterns

Exclude build artifacts, IDE files, and large binaries from sync:

```toml
[personal]
poll_interval_secs = 60

[svn]
url = "https://svn.company.com/repos/project/trunk"
username = "jdoe"
password_env = "SVN_PASSWORD"

[github]
repo = "jdoe/project-mirror"
token_env = "GITHUB_TOKEN"

[developer]
name = "John Doe"
email = "jdoe@company.com"
svn_username = "jdoe"

[options]
ignore_patterns = [
    "build/**",
    "dist/**",
    ".idea/**",
    "*.iml",
    "*.class",
    "*.jar",
    "*.war",
    "node_modules/**",
    "*.log",
]
max_file_size = 26214400  # 25 MB
sync_executable_bit = false
```

---

## Default Values Summary

For quick reference, here are all fields and their defaults:

| Field | Default |
|-------|---------|
| `personal.data_dir` | `"~/.local/share/reposync"` |
| `personal.poll_interval_secs` | `30` |
| `personal.log_level` | `"info"` |
| `svn.url` | *(required, no default)* |
| `svn.username` | *(required, no default)* |
| `svn.password` | *(none)* |
| `svn.password_env` | *(none)* |
| `github.api_url` | `"https://api.github.com"` |
| `github.token` | *(none)* |
| `github.token_env` | *(none)* |
| `github.repo` | *(required, no default)* |
| `developer.name` | *(required, no default)* |
| `developer.email` | *(required, no default)* |
| `developer.svn_username` | *(required, no default)* |
| `commit_format.svn_to_git` | *(see template section above)* |
| `commit_format.git_to_svn` | *(see template section above)* |
| `options.normalize_line_endings` | `true` |
| `options.sync_executable_bit` | `true` |
| `options.max_file_size` | `0` (no limit) |
| `options.ignore_patterns` | `[]` |
| `options.auto_merge` | `true` |
| `options.sync_externals` | `false` (not implemented) |
| `options.sync_direct_pushes` | `false` (not implemented — fails if set to true) |
| `options.lfs_threshold` | `0` (LFS disabled) |
| `options.lfs_patterns` | `[]` |


### Incremental SVN→Git scope

In an existing Personal mirror, set `allowed_paths = ["foo/"]` in `[options]`
to select files under that branch-relative prefix. Prefixes match complete path
components. An absent or empty list keeps the normal bidirectional behavior on mirrors that have never enabled scope. Once enabled, the mirror is bound to scoped operation: removing the option will hold sync and import, not silently widen publication. Use a separately prepared mirror for unrestricted operation.
The existing file-size, ignore and LFS policy applies inside the selected paths.

This incremental mode is **SVN→Git only**: it does not poll or replay merged
PRs, and refuses snapshot and full import rather than silently importing
the entire SVN tree. Start with an existing checkpoint and a clean checkout
matching the selected SVN files. If the trees diverge, positive updates stop
before copying or deleting anything; preserve the checkout and reconcile.
Do not expect a change in scope to backfill previously handled revisions.

A fully filtered revision advances the Personal SVN watermark but creates no
Git commit, push, or PR and does not move the Git cursor. Use
`reposync personal status` or `reposync personal log` to see the revision's
filtered outcome or the SHA for a published commit.


**Transport proof boundary.** Local `file://` supports directory scopes. Both
network profiles are explicit stock Apache compatibility choices, not evidence
that an account can see every path.

- Stock `svnserve` over `svn://`: set
  `network_directory_profile = "apache_svnserve_wc31"` in `[options]`.
- Qualified Apache `mod_dav_svn` over direct `https://`: set
  `network_directory_profile = "apache_mod_dav_svn_https_wc31"` and
  `svn_tls_ca_file = "/absolute/path/to/trusted-ca.pem"` in `[options]`.
  Supply an independently trusted PEM certificate bundle. The client copies
  it from a canonical regular-file path (no symlink) to its private per-cycle
  configuration, performs standard TLS hostname
  and chain validation and neither uses ambient trust nor ignores TLS failures.
  After a valid direct SVN response, the mirror binds the repository identity
  and CA bytes. A replaced CA bundle, mismatched origin, or malformed input
  holds; it is not automatically admitted. Restore the admitted CA or use a
  new separately prepared mirror after an intentional trust transition.
  Redirected DAV, `http://`, `svn+ssh://`, unqualified servers and unknown WC
  layouts are outside the profile. RepoSync compares the returned identity
  to the configured direct URL before accepting evidence. It does not
  intercept redirects in the SVN transport or promise that redirect traffic
  cannot occur; configure a canonical endpoint that does not redirect.

Full-depth checkouts are pinned independently at adjacent endpoints. Both
must agree with complete WC31 metadata: excluded, switched, sparse or
incomplete nodes hold the current revision. A separate export must agree
with proved paths, bytes and modes before the Git writer can use it. A
missing path is proved only by a pristine depth-empty parent and exact
single-child update; no sibling contents are fetched and no parent above the
configured SVN branch is read. An empty listing is not absence. In Apache DAV,
a denied child can produce the same successful update message as an absent one
but leaves a `server-excluded` WC31 row; RepoSync requires unchanged pristine
metadata as well as the exact acknowledgment.

**Fixed work limits.** Each `svn://` cycle examines at most 32 consecutive
revision numbers and uses the existing five-minute per-command SVN deadline.
HTTPS examines at most 8 consecutive numbers, with 30 seconds per SVN
command and a 90-second planning budget shared across the cycle. HTTPS
subprocesses on the qualified Unix profile have at most 64 MiB per written
file, and command capture admits at most 16 MiB stdout and 1 MiB stderr.
SVN subprocess temp files are directed to the same owned planning scratch.
Owned private planning scratch is checked every 250 ms while commands run and
at stage boundaries; 512 MiB or 25,000 entries stops planning. This is a
sampled aggregate guard, not a filesystem quota: a write can grow between
checks. The 90-second limit is a planning deadline, not a deadline for
cleanup or an already journaled Git push. Each uncached revision can require
multiple complete checkouts and exports and additional nested-lineage reads.

A branch log can omit unrelated revisions. The cursor cannot jump over an
unseen number without matched, independently proved selected endpoints.
Missing log metadata cannot authorize a changed Git tree. A timeout, trust
failure or unresolved proof leaves the current revision pending, preserves
already confirmed earlier revisions, and stops later ones. Catch-up resumes
at the next number on a later cycle.

### Scoped full history and one-prefix rewrite

For a new mirror, choose an SVN-relative scope in `reposync personal init`, or
put the following under `[options]`:

```toml
projected_history = true
allowed_paths = ["team/alpha/selected/"]
blocked_patterns = ["team/alpha/selected/private/**"]
path_rewrite_rules = [{ strip_prefix = "team/alpha/" }]
```

This selection mirrors SVN `team/alpha/selected/note.txt` at Git
`selected/note.txt`. Both allow and block rules are evaluated in the original
SVN coordinates. Leave out the rewrite array to keep SVN-relative names in
Git. An explicitly enabled scope needs a fresh data directory and empty Git
destination branch. Run `reposync personal import --full`; scoped snapshots
are refused rather than importing the whole repository. The full import pins
the SVN origin and binds the policy only after verifying the published Git tip.

The Personal worker then applies the same projection during SVN polling, and
maps eligible merged pull-request changes back to their original SVN paths.
Direct pushes are not replayed as Personal pull requests. Git commits that
diverge from the projection are held. Git LFS is not supported with this opt-in;
writing executable-mode, special-node, or non-regular changes via a projected
PR is refused. Existing scoped-only configurations without `projected_history`
stay inbound-only; old configurations with no scope remain unchanged.

`reposync personal status` reports the active policy and its verified baseline.
`reposync personal doctor` reports files outside the scope from the local
checkout and current remote tip. The remote observation uses a short-lived
private Git directory and never changes the live checkout or remote. A rule
change, partial import, or destination change does not authorize new writes.
Preserve the old mirror, choose a new/empty destination and fresh data directory,
and re-import. Do not erase a preserved mirror to clear a held write.

A projected PR is applied from its actual accepted Git commit, using the
first-parent change from the last handled Git checkpoint. Merge commits and
linear/squash commits with that parent are supported. A multi-commit rebase
whose merged tip has a different first parent, or other ambiguous intervening
Git history, is held instead of silently skipping commits.

Projected history contains commits only when selected Git content or mode
changes. Revisions that leave the projected tree unchanged (including a
leading empty tree) advance the verified SVN frontier without copying their
authors or log messages into Git. A copied SVN branch starts at the creation
snapshot of that exact branch incarnation; import does not follow history
into the predecessor branch.

If a poll finds a selected SVN change while Git also has unhandled work,
RepoSync holds before updating the bridge checkout or publishing in either
direction. It does not pick a side, reset a checkout, or attempt an automatic
cross-side merge; reconcile the competing history before resuming the mirror.
