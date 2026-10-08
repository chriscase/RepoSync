# #65 slice: legacy disable and additive managed removal

This is the smallest product slice of issue #65. It does **not** close #65 or #61.

## What shipped

| Operation | Route | Effect |
| --- | --- | --- |
| Legacy disable | `DELETE /api/repos/{id}` | Sets `enabled=false` only. Registration, mappings, secrets, local files, and remotes stay. Response `action` is `disable` and `message` remains `repository disabled`. No removal journal is written. |
| Explicit disable | `POST /api/repos/{id}/disable` | Same contract as legacy DELETE for new UI clients. Adds `preservation`, `remote_git`, `remote_svn`, and `managed_removal: false`. |
| Managed removal | `POST /api/repos/{id}/remove` | Explicit and additive. Uses the v12 `kv_state` journal `managed_remove_v1:` (same storage family as #64 import and Git→SVN journals). Schema stays v12. Optional query `explicit_remote_deletion_opts=true` with `delete_git` / `delete_svn` applies **only** to child (branch-pair) registrations and mirrors the branch-pair DELETE remote opt-in contract; omitted params stay non-destructive for new UI callers. |
| Removal status | `GET /api/repos/{id}/removal` | Read-only state for the latest managed-removal operation. Does not delete anything. Returns `operation_id`, `partial_cleanup` on failure/hold, and `recovery` tombstone metadata when present. HTTP status follows the operation: **`200`** when `state` is `completed`; **`202`** when `queued`, `cancelling`, or `running`; **`409`** when `failed` or `reconciliation_required`. Returns **`404`** when no managed-removal journal exists for the id. |
| Removal dependency preview | `GET /api/repos/{id}/removal/preview` | Read-only. Lists parent/child registrations, per-repo and global credentials (what would be deleted vs preserved, including credential-chain inheritance), managed local path `repos/{id}`, sibling paths left intact, and other registrations sharing the same non-empty Git remote. When a managed removal is already active, the response body still includes `dependency_preview`, and the **HTTP status** mirrors that operation (`202` while waiting, `409` on `reconciliation_required` or parent blocked) plus `active_removal`. When no removal is active, HTTP **`200`**. |
| Managed restore | `POST /api/repos/{id}/restore` | Restores a **completed** removal from its tombstone while retained mappings/recovery metadata still exist. Registration returns with **`enabled=false`** until the operator enables sync. **`409`** when removal is still in progress or recovery was purged. Idempotent **`200`** when the registration is already listed. |
| Branch-pair remote delete | `DELETE /api/repos/{id}/branch-pair` | Legacy callers omitting `delete_git` / `delete_svn` still default both to **true**. New UI uses managed `POST /remove` with explicit remote flags; this DELETE route remains for legacy callers. |

Managed removal:

1. Records a durable operation and disables the row before any local delete.
2. If a non-terminal import or Git→SVN commit is active, or either journal is `reconciliation_required`, it does **not** delete files or secrets. Imports are cancel-requested. The response is `cancelling` (HTTP 202) or `reconciliation_required` (HTTP 409), with `ok: false`.
3. If this process still holds the repository busy lock, or in-memory import progress is non-terminal, cleanup waits the same way.
4. Parent removal is refused while child registration rows exist (HTTP 409, `state: blocked`, full `dependency_preview`). Children are not cascaded.
5. Child removal deletes only that registration's per-repo secret keys (`secret_svn_password_{id}`, `secret_git_token_{id}`) and `repos/{id}`; parent, sibling, and global credentials and trees are preserved.
6. After writers are quiet, cleanup deletes only `{data_dir}/repos/{id}` when that path is a direct real child of the managed root, plus the exact per-repo secret keys above.
7. On full success the repository row is removed from active listings, a tombstone is kept, and the response is `completed` with `ok: true`. Commit-map rows, audit rows, per-repo watermark keys, and remote Git/SVN history are kept. **`restore_supported: true`** on new completions while recovery metadata remains.
8. Symlink roots, traversal, and escapes fail the cleanup. `ok` stays false, `state` is `failed`, and the same POST retries. Repeated calls and a completed tombstone do not recreate the registration until restore.

`POST /api/repos/{id}/remove` uses the same HTTP status mapping as `GET /api/repos/{id}/removal` for the operation body, except parent-with-children refusal returns **`409`** with `state: blocked` (not a journal state).

`remote_git` and `remote_svn` in the removal response are `untouched`. This slice does not delete remote branches or SVN paths unless explicitly opted in on child removal.

## Still later

- Optional authenticated remote Git-ref / SVN-path deletion beyond branch-pair delete
- Full trash/restore product polish (purge lifecycle, list removed entries)
- Closing #65

## Tests

Named cases are in `docs/reliability/required-cases.json` under R02: `R02_LEGACY_DELETE`, `R02_MANAGED_REMOVE`, `R02_REMOVE_RETRY`, `R02_PATH_CONFINEMENT`, and `R02_JOURNAL_RETRY`, plus RS-16 / dependency cases `R65_*`, and the existing `R02_R03_ROUTE` disable assertion.
