# #89 — Projected Git→SVN changeset and conflict apply gate

Parent: #61. Coordinate with #52/#60; this contract covers RS-C02 ordering,
leakage, and team-mode conflict apply gates only.

## Persisted conflict apply gate (team mode)

Before any SVN or Git **apply** (including `sync_svn_to_git`, `sync_git_to_svn`,
and `resume_authorized_svn_to_git_push`), `SyncEngine::run_sync_cycle` reads the
`conflicts` table for the active repository from SQLite. In-memory detection
alone is not sufficient.

| Status | Blocks apply |
| --- | --- |
| `detected` | yes |
| `queued` | yes |
| `resolving` | yes |
| `active` | yes |
| `deferred` | yes |
| `resolved` | no |
| `dismissed` | no |

When any blocking row exists, the cycle returns `UnresolvableConflict` and must
not mutate SVN, the Git remote, or publish mappings. A row can remain after the
live Git remote no longer shows the divergent commit (for example force-push
back); the persisted row still blocks until resolved or dismissed.

## Conflict upsert integrity

`record_detected_conflict` matches existing rows for the same `repo_id` and
`file_path` with any non-terminal status above. Retries refresh content fields
but do not demote `queued`, `resolving`, `active`, or `deferred` to
`detected`. The SELECT and INSERT/UPDATE run in one `BEGIN IMMEDIATE`
transaction.

## Pre-commit hook path rules

Rendered allowed prefixes use single-quoted Bash literals. Blocked patterns use
the same component-aware matching as `path_matches_blocked` (directory prefix
`secret` blocks `secret/leak.txt`).
