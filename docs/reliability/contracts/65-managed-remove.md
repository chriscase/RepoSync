# #65 slice: legacy disable and additive managed removal

This is the smallest product slice of issue #65. It does **not** close #65 or #61.

## What shipped

| Operation | Route | Effect |
| --- | --- | --- |
| Legacy disable | `DELETE /api/repos/{id}` | Sets `enabled=false` only. Registration, mappings, secrets, local files, and remotes stay. Response `action` is `disable` and `message` remains `repository disabled`. No removal journal is written. |
| Managed removal | `POST /api/repos/{id}/remove` | Explicit and additive. Uses the v12 `kv_state` journal `managed_remove_v1:` (same storage family as #64 import and Git→SVN journals). Schema stays v12. |
| Removal status | `GET /api/repos/{id}/removal` | Read-only state. Does not delete anything. |

Managed removal:

1. Records a durable operation and disables the row before any local delete.
2. If a non-terminal import or Git→SVN commit is active, or either journal is `reconciliation_required`, it does **not** delete files or secrets. Imports are cancel-requested. The response is `cancelling` (HTTP 202) or `reconciliation_required` (HTTP 409), with `ok: false`.
3. If this process still holds the repository busy lock, or in-memory import progress is non-terminal, cleanup waits the same way.
4. Parent removal is refused while child registration rows exist. Children are not cascaded. A dependency preview is a later slice.
5. After writers are quiet, cleanup deletes only `{data_dir}/repos/{id}` when that path is a direct real child of the managed root, plus the exact keys `secret_svn_password_{id}` and `secret_git_token_{id}`.
6. On full success the repository row is removed from active listings, a tombstone is kept, and the response is `completed` with `ok: true`. Commit-map rows, audit rows, per-repo watermark keys, and remote Git/SVN history are kept.
7. Symlink roots, traversal, and escapes fail the cleanup. `ok` stays false, `state` is `failed`, and the same POST retries. Repeated calls and a completed tombstone do not recreate the registration. Restore is **not** supported.

`remote_git` and `remote_svn` in the removal response are `untouched`. This slice does not delete remote branches or SVN paths.

## Still later

- UI wording polish beyond this API distinction
- Parent/child dependency preview
- Optional authenticated remote Git-ref / SVN-path deletion
- Changing branch-pair `DELETE /api/repos/{id}/branch-pair`, which still defaults omitted `delete_git` / `delete_svn` to true
- Closing #65

## Tests

Named cases are in `docs/reliability/required-cases.json` under R02: `R02_LEGACY_DELETE`, `R02_MANAGED_REMOVE`, `R02_REMOVE_RETRY`, `R02_PATH_CONFINEMENT`, and `R02_JOURNAL_RETRY`, plus the existing `R02_R03_ROUTE` disable assertion.
