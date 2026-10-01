# #64-C: explicit read-then-maybe-finalize for one Git→SVN commit

This increment starts at merged main `e2db4e047ce84d62da71fc61f70c8b90a1b9898f` and retains #64-A/#64-B import journals. It does not enable automatic recovery, a general team-sync journal, cross-host fencing, or candidate v13/v14 storage. Ordinary SQLite startup remains v12.

## Entry and exclusion

Before the team engine issues `svn commit` for one Git change, it persists a v12 `kv_state` document (`git_to_svn_commit_v1:`) with the operation ID, pair/repo identity, source Git SHA/parent/tree, target SVN UUID/path, pre-write revision/tree, projection, intended changed paths/tree, and durable identity trailers. The active pointer and document always change in one SQLite transaction.

If that commit is accepted but the reply or the local mapping/checkpoint write fails, the operation remains `reconciliation_required`. The worker must not report success or a rollback. A later scheduler tick skips a held repository unless resume of that exact intent has been authorized.

`POST /api/repos/{repo_id}/svn-commit/{operation_id}/reconcile` requires a live admin session. Named-user installations do not fall back to a legacy in-memory session. The route acquires the same process-wide per-repository busy slot used by import and the scheduler. It accepts only the exact active `reconciliation_required` Git→SVN commit. A completed operation returns its durable completed status without writing SVN.

## External proof

Recovery re-reads the exact SVN target (`info`, one-revision `log`, export/tree hash). A unique match requires UUID/path equality, immediate revision ancestry (`pre_write + 1`), intended changed paths (directory adds that are ancestors of intended files are allowed extras), the exported tree, and the durable `RepoSync-Operation` / `RepoSync-Git-SHA` trailers. A message trailer or final-tree equality alone is insufficient. Inspection errors never become evidence of absence.

If HEAD is still the pre-write revision and that tree is unchanged, the effect is absent. The endpoint authorizes resume of that one planned write and leaves the hold in place. Conflicting or unavailable evidence stays held. There is no blind retry, reimport, or checkpoint reset.

## Worker resume

Only after explicit authorize-resume may the team worker issue that one planned `svn commit`. It re-uses the same operation identity and refuses any other Git SHA while the hold exists. A unique match is finalized by the admin reconcile path without a second SVN write.

## Remaining #64 boundaries

Automatic/background reconciliation, cross-host fencing, partial-import resume, a general team-sync journal, refresh, pairing, and active v13/v14 remain outside this increment. Issue #64 stays open.
