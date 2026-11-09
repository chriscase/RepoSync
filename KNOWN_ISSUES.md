# RepoSync Known Issues

## Resolved (17 issues fixed)

1. ~~Data not scoped by repository~~ — Frontend passes repo_id to all API calls
2. ~~Import is global, not per-repo~~ — POST /api/repos/:id/import with ?reset=true
3. ~~Scheduler only syncs global repo~~ — Per-repo scheduler with SyncEngine per repo
4. ~~Setup Wizard overwrites TOML~~ — TOML generation removed, config in DB
5. ~~Watermarks in fragile kv_state~~ — Now in repositories table columns
6. ~~No credentials on repo detail~~ — SVN password + Git token fields with Test Connection
7. ~~Dashboard filter doesn't work~~ — Passes activeRepoId to all queries
8. ~~Server Monitor auth~~ — fetchJson includes Authorization header
9. ~~Audit log only errors~~ — Success entries now logged
12. ~~TOML file overwrite~~ — Generation removed
13. ~~No graceful shutdown~~ — SIGTERM + WAL checkpoint
15. ~~SVN commit parsing~~ — Case-insensitive with fallback patterns
16. ~~LFS not tracked~~ — FilePolicy::with_lfs() now used correctly (4 files tracked)
- ~~Import watermark not written~~ — Reads from watermarks table after import
- ~~Reset import URL bug~~ — Token no longer passed as git_base_url
- ~~Git test connection no auth~~ — Token sent in Authorization header
- ~~Audit log not filtered by repo~~ — SQL-level WHERE repo_id = ?

## Active Issues

### 10. Activity grouping/collapsing
- Consecutive same-action audit entries should be collapsed in UI
- Code exists but needs visual validation

### 11. Per-repo credential hot-reload validation
- reload_credentials() reads per-repo keys — needs broader testing

### 14. Session expiry UX
- 401 redirects to /login — could show toast instead

### 17. Status endpoint shows global data when repo filtered
- /api/status returns global sync engine state
- Dashboard status cards show wrong data for specific repos
- **Fix needed**: Read status from repositories table (last_svn_rev, sync_status, total_syncs, total_errors) instead of global sync engine

### 18. LFS counter not incremented in import progress
- lfs_unique_count shows 0 even when LFS files are detected and tracked
- The actual tracking works (4 files tracked) but the counter isn't updated
- Cosmetic issue — import log lines show "LFS: 4" per revision

### 19. Dashboard ImportProgressCard only shows global import
- Per-repo imports only visible on repo detail page
- Dashboard should show any active import across all repos

### 20. Echo loop — sandbox accumulated 3000+ duplicate SVN revisions
- Caused by watermark being 0 after import (Bug 1, now fixed)
- Echo detection (is_echo_commit) works but wasn't preventing reprocessing
  because scheduler didn't know where import ended
- Need to verify fix with clean test after watermark fix deployed

## Hardening Needed

### H1. Per-repo status on dashboard
- When repo filter selected, status cards should show that repo's data
- Read from repositories.last_svn_rev, sync_status, total_syncs, total_errors

### H2. Import shows progress on dashboard
- Any active per-repo import should show on main dashboard
- Currently only visible on individual repo detail page

### H3. Robust error display
- Long error messages truncated in audit log
- Should be expandable

### H4. Sync cycle should log to audit with repo_id
- Current audit entries don't have repo_id set
- Makes filtering by repo impossible for audit entries

### H5. Identity mapping per-repo
- Currently uses global identity config
- Each repo should be able to have its own email_domain and mapping file

## Documented States

### NULL `commit_map.git_sha` (#54)
- Since operational schema v15, `commit_map.git_sha` may be NULL. NULL means
  "this SVN revision produced no Git commit", and only when it is proven by a
  canonical `intentionally_filtered_no_target` outcome for the same repository
  scope and current generation. Unproven NULL rows are inert: they are not
  "synced" and they never advance a watermark or checkpoint.
- Nothing in the default configuration (empty `allowed_paths`) writes NULL
  rows. The SVN→Git path filter that would produce them is separate #52 work.
- The Repository Detail and Dashboard mapping tables label these rows instead of
  showing a SHA. Contract: [docs/reliability/contracts/54-nullable-commit-map.md](docs/reliability/contracts/54-nullable-commit-map.md).
