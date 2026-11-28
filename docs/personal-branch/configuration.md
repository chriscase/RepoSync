# Personal Branch Mode: Configuration Reference

Personal commands read `~/.config/reposync/personal.toml` by default. To work with another file, write it with `init --output`, then pass `--personal-config` before the action each time you use it.

```bash
reposync personal init --output "$HOME/.config/reposync/work.toml"
reposync personal --personal-config "$HOME/.config/reposync/work.toml" status
```

## Full Configuration File

For a new setup, replace the example `data_dir` with an absolute path you own. Personal uses this directory for `personal.db` and the `git-repo` checkout. The TOML loader treats a leading `~/` literally, so its location depends on the directory from which you ran the command. For an existing installation, use the absolute path to the directory that already holds its database and checkout.

```toml
[personal]
data_dir = "/home/jdoe/.local/share/reposync"
poll_interval_secs = 30

[svn]
url = "https://svn.company.com/repos/project/trunk"
username = "jdoe"
password_env = "SVN_PASSWORD"

[github]
api_url = "https://api.github.com"
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
| `data_dir` | string | `"~/.local/share/reposync"` | Directory for the SQLite database, Git checkout and sync state. The fallback value is a literal relative path. Set an absolute path for a new setup. |
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
| `password_env` | string | yes | -- | Name of the environment variable that holds the SVN password. |

### [github]

Connection details for the GitHub repository used as the Git mirror.

| Key | Type | Required | Default | Description |
|-----|------|----------|---------|-------------|
| `api_url` | string | no | `"https://api.github.com"` | GitHub API base URL. Use `https://api.github.com` for GitHub.com. For GitHub Enterprise Server, use `https://github.yourcompany.com/api/v3`. |
| `token_env` | string | yes | -- | Name of the environment variable that holds the GitHub Personal Access Token. |
| `repo` | string | yes | -- | Target GitHub repository in `owner/name` format (e.g., `jdoe/project-mirror`). |

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
> LFS is **enabled** when `lfs_threshold` is greater than 0 or `lfs_patterns` is
> not empty. Both at their defaults disables LFS. There is no separate boolean.
>
> When LFS is enabled, import fails closed unless `git lfs version` succeeds.
> `reposync personal doctor` reports the tool version and whether the personal
> Git repository has local LFS filters (`git lfs install --local`).
>
> SVN→Git sync and full import copy a qualifying file, record that exact path
> in `.gitattributes` (for example `/models/large.bin`), then commit with the
> Git command-line clean filter. Glob characters are escaped. A path that
> contains whitespace or a double quote is written as a double-quoted Git
> attribute pattern; a backslash before a space does not keep the path together.
> The rule matches only the selected file. A size threshold does not become
> an extension glob, and a pattern such as `models/*.bin` does not track a
> `.bin` file outside `models/`. The recorded commit is a pointer
> (`version https://git-lfs.github.com/spec/v1`) and the bytes are stored
> under `.git/lfs/objects`. A file under the threshold that matches no pattern
> stays an ordinary blob. Each copy writes `lfs_tracked` and `lfs_skipped`
> audit rows.
>
> When a tracked file later shrinks below the threshold, or LFS is disabled,
> that exact rule is removed and the next content commit stores an ordinary
> blob. An SVN revision that does not change file bytes, such as a
> property-only revision, is recorded against the existing Git commit. It
> does not fail the import or the sync. A missing `git-lfs` tool, a clean
> filter failure, or a qualifying path that was committed as a regular blob
> still stops the run.
>
> Git→SVN replay reads the pointer and writes the real bytes to SVN when the
> local object exists. If the object is missing, replay stops before the SVN
> commit, leaves the pointer text out of the working copy, and writes an
> `lfs_resolution_failed` audit row naming the path. Restore the object with
> `git lfs fetch` or `git lfs pull` in the personal repository, then run sync
> again. Do not commit the pointer text into SVN.

---

## Environment Variables

Set `svn.password_env` and `github.token_env` to the names of the environment variables supplied to the Personal CLI. The loader reads the values from those variables when it opens the configuration.

| Config field | Name in the example above | Default name in the wizard |
|--------------|---------------------------|----------------------------|
| `svn.password_env` | `SVN_PASSWORD` | `REPOSYNC_SVN_PASSWORD` |
| `github.token_env` | `GITHUB_TOKEN` | `REPOSYNC_GITHUB_TOKEN` |

Supply the SVN password and GitHub token under your chosen names in the environment used for import and sync.

---

## Config File Location

The wizard creates the config file's parent directory when needed. For a custom location, select the file explicitly for import, start, status and stop, for example `reposync personal --personal-config "/path/to/personal.toml" import --full`.

If an older wizard wrote a literal `~/` directory beneath its working directory, pass the absolute path of that existing config to `--personal-config`. Keep the data directory pointed at the existing Personal storage.

---

## Example Configurations

### GitHub.com (minimal)

The simplest setup for mirroring an SVN trunk to a personal GitHub.com repo:

```toml
[personal]
data_dir = "/home/jdoe/.local/share/reposync"
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
data_dir = "/home/jdoe/.local/share/reposync"
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
data_dir = "/home/jdoe/.local/share/reposync"
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
data_dir = "/home/jdoe/.local/share/reposync"
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
| `svn.password_env` | *(required, no default)* |
| `github.api_url` | `"https://api.github.com"` |
| `github.token_env` | *(required, no default)* |
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
