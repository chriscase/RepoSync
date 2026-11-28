# Configuration Reference

RepoSync is configured via a TOML file, typically at `/etc/reposync/config.toml`.

## [daemon]

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `poll_interval_secs` | integer | `15` | Seconds between polling for new changes |
| `log_level` | string | `"info"` | Log level: trace, debug, info, warn, error |
| `data_dir` | string | `"/var/lib/reposync"` | Directory for SQLite database and state |

## [svn]

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `url` | string | yes | SVN repository URL (root, not trunk) |
| `username` | string | yes | SVN username for the sync service account |
| `password_env` | string | yes | Environment variable name containing SVN password |
| `layout` | string | no | `"standard"` (default) or `"custom"` |
| `trunk` | string | no | Custom trunk path (only with `layout = "custom"`) |
| `branches` | string | no | Custom branches path |
| `tags` | string | no | Custom tags path |

## [github]

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `api_url` | string | yes | GitHub API base URL. Use `https://api.github.com` for GitHub.com or `https://github.company.com/api/v3` for GHE |
| `repo` | string | yes | Repository in `owner/name` format |
| `token_env` | string | yes | Environment variable containing GitHub token |
| `webhook_secret_env` | string | no | Environment variable containing webhook secret |
| `default_branch` | string | no | Default branch name (default: `"main"`). It must be a branch name Git accepts, such as `main` or `release/2026.11`, not a full ref like `refs/heads/main`. The daemon refuses to start if it is not |
| `git_base_url` | string | no | Explicit Git clone base URL override. When omitted, derived automatically from `api_url` (`https://api.github.com` → `https://github.com`; `https://host/api/v3` → `https://host`). Set this only when your enterprise instance uses a non-standard clone endpoint. **Note:** Enterprise support is theoretical — pending live GHES/GHEC validation. |

## [identity]

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `mapping_file` | string | no | Path to the author mapping file, if used. Settings > Identity mappings and the setup wizard write this file (or `identity-mappings.toml` beside the configuration file when it is unset, which the mapper does not read). A running sync picks up a saved change on its next lookup. Saving from the server rewrites the file in its own format, so comments in it are not kept. If the file is deleted, running syncs keep the mappings they loaded until restart. |
| `email_domain` | string | no | Fallback domain for unmapped users |
| `ldap_url` | string | no | LDAP server URL for automatic lookup |
| `ldap_base_dn` | string | no | LDAP base DN for searches |
| `ldap_bind_dn` | string | no | LDAP bind DN for authentication |
| `ldap_bind_password_env` | string | no | Environment variable with LDAP bind password |

## [web]

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `listen` | string | yes | Address to listen on (e.g., `"0.0.0.0:8080"`) |
| `session_secret_env` | string | yes | Environment variable for session cookie secret |
| `auth_mode` | string | yes | `"simple"`, `"github_oauth"`, or `"both"` |
| `admin_password_env` | string | depends | Required when auth_mode includes "simple" |
| `github_oauth_client_id_env` | string | depends | Required for GitHub OAuth |
| `github_oauth_client_secret_env` | string | depends | Required for GitHub OAuth |
| `github_oauth_allowed_org` | string | no | Restrict access to members of this org |

## [notifications]

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `slack_webhook_url_env` | string | no | Slack incoming webhook URL env var |
| `email_smtp` | string | no | SMTP server address (host:port) |
| `email_from` | string | no | Sender email address |
| `email_recipients` | array | no | List of recipient email addresses |

## [sync]

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `mode` | string | `"direct"` | Reserved. `"direct"` and `"pr"` are both accepted, but sync always pushes directly today; PR-gated sync is not implemented. The pair page and setup wizard do not offer this choice. |
| `auto_merge` | boolean | `true` | Attempt 3-way merge for non-overlapping changes. This is the service-wide switch: when it is `false`, no pair auto-merges. When it is `true`, a pair whose own auto-merge setting is off still holds every conflict for review. The pair setting applies to scheduled sync and to publishing a late branch pair; the `sync now` command uses only this service setting. |
| `sync_branches` | array of strings | `[]` | Branch names to synchronize. An empty list selects all branches. |
| `sync_tags` | boolean | `true` | Sync tag creation |

### [sync.pr] (only when mode = "pr")

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `require_approval` | boolean | `true` | Require PR approval before SVN sync |
| `auto_merge_approved` | boolean | `true` | Auto-merge approved PRs |
| `reviewers` | array | `[]` | Default PR reviewers |

## Authors Mapping File

The `authors.toml` file maps SVN usernames to Git identities:

```toml
[authors]
svn_username = { name = "Full Name", email = "email@company.com", github = "github_username" }

[defaults]
email_domain = "company.com"
```

The `github` field is optional and used for GitHub OAuth identity matching.

## Environment Variables

All sensitive values are stored in environment variables (never in config files):

| Variable | Description |
|----------|-------------|
| `REPOSYNC_SVN_PASSWORD` | SVN service account password |
| `REPOSYNC_GITHUB_TOKEN` | GitHub PAT or App token |
| `REPOSYNC_WEBHOOK_SECRET` | GitHub webhook secret |
| `REPOSYNC_ADMIN_PASSWORD` | Web dashboard password |
| `REPOSYNC_SESSION_SECRET` | Session cookie signing secret |
| `REPOSYNC_SLACK_WEBHOOK` | Slack incoming webhook URL |
| `REPOSYNC_LDAP_PASSWORD` | LDAP bind password |
| `REPOSYNC_OAUTH_CLIENT_ID` | GitHub OAuth App client ID |
| `REPOSYNC_OAUTH_SECRET` | GitHub OAuth App secret |
