# #54 contract: nullable `commit_map.git_sha` (operational schema v15)

Refs #54 (parent #52). This document states the storage, writer and reader
contract for allowing `commit_map.git_sha` to be NULL. It was written before
the semantic code and is updated with it. It does not close #54 or #52, and it
does not implement the SVN→Git path filter, `allowed_paths` projection or any
other #52 feature.

Source base: `714748c07e49fa47a707c98a0aa83c3d2edced10`. Operational schema
before this change: `user_version = 14` (`schema.rs::MIGRATIONS`).

## What a NULL `git_sha` means

A `commit_map` row with `git_sha IS NULL` is a **legacy display projection** of
the statement "SVN revision N of this repository was considered and produced no
Git commit". It is never authority by itself.

Authority stays in the #63 canonical tables. A NULL row is meaningful
(**proven no-target**) only when all of these hold:

- `direction = 'svn_to_git'`;
- `repo_id` names a registered repository with a non-empty `scope_uuid`;
- `canonical_mapping_outcomes` already holds
  `outcome = 'intentionally_filtered_no_target'`, with both targets NULL, for
  that `scope_uuid`, that repository's **current** generation, direction
  `svn_to_git` and source key `svn:r<N>`.

Every other NULL row is **unverified no-target** and is inert: it changes no
value reader, grants no "already synced" answer, and advances nothing.

NULL, the empty string and a missing row are never aliases of one another.

## Storage

- Registry: one new entry, `v15`, in `crates/core/src/db/schema.rs::MIGRATIONS`.
  There is no `db/migrations/` directory and no second framework. The
  fixture-only candidate `CopySession` registry (its own "v13"/"v14") is not
  called, changed or activated.
- Physical shape: the v15 `commit_map` table body and its four indexes are
  byte-identical to the candidate v13 rebuild in
  `crates/core/src/db/candidate_migration.rs`, as the #63 contract requires
  ("If #54 lands another version number, reuse that rebuild"). A test fails if
  the two texts drift. No new `CHECK` is added, so the shape does not compete
  with the candidate one; the direction rule below is enforced by the writer.
- Transaction: the rebuild and the `user_version` bump run in the registry's
  single migration transaction. On any error nothing is kept.
- Preconditions, checked inside that transaction before any write:
  - exact operational v14 columns (`PRAGMA table_info`) with `git_sha NOT NULL`;
  - the four registry indexes are present with their registry columns;
  - no foreign key in another table refers to `commit_map`.

  Anything else is refused with `MigrationFailed { version: 15 }` and leaves the
  database at v14, byte-for-byte as far as SQLite is concerned.
- Views, and triggers on other tables, that query `commit_map` are kept. Modern
  `ALTER TABLE ... RENAME` re-validates them against the intermediate schema
  (where `commit_map` is momentarily absent) and would abort, so the rename runs
  with `PRAGMA legacy_alter_table=ON`, which rewrites nothing; the previous
  setting is restored on every path. Their stored SQL must be byte-identical
  afterwards, and each such view must still prepare.
- Preservation: every row keeps its `id` and every column value. The
  `sqlite_sequence` entry for `commit_map` keeps its exact historical value, or
  its exact absence; it is never replaced with `MAX(id)`. Other tables'
  sequence entries are unchanged. Additional indexes or triggers on
  `commit_map` are recreated from their stored SQL. Before the version bump the
  migration verifies the row digest, the sequence, the expected v15 schema text,
  the recreated objects, the untouched dependent views and triggers,
  `PRAGMA integrity_check(commit_map)` and `PRAGMA foreign_key_check(commit_map)`.
- Idempotence: the registry applies v15 only when `user_version` is 14. If the
  table already has the exact v15 shape, the body is a no-op.
- Future versions: an executable at v15 refuses `user_version > 15`, as before.
  A v14 executable refuses a v15 file with `UnsupportedSchema`; it does not
  silently downgrade.
- Ordinary startup still requires a quiet installation before any schema
  upgrade, and refuses while a pair-refresh hold exists. Pair-refresh recovery
  mode still requires the already-current schema, so a v14 database holding a
  refresh job must be recovered by the v14 executable before upgrading.

### Rollback

There is no down migration, because the registry has none and a downgrade that
removes or rewrites valid NULL rows would destroy data. The supported options
are:

1. **Pre-effect restore.** Before upgrading, take the consistent backup that
   the #63 contract describes. Restoring it is safe only if nothing was written
   after the upgrade, or if losing those writes has been accepted.
2. **Roll forward.** Otherwise keep v15 and fix forward.

No downgrade tool is provided.

## Writer contract

`Database::insert_commit_map_with_target(svn_rev, git_sha: Option<&str>,
direction, svn_author, git_author, repo_id)` is the single implementation. The
existing `insert_commit_map` and `insert_commit_map_with_repo` keep their `&str`
signatures and delegate with `Some(sha)`.

| Input | Behavior |
| --- | --- |
| `Some(sha)`, non-empty | Unchanged. For a scoped row it also records the canonical `applied_and_verified` outcome and checkpoint in the same transaction, as before. |
| `Some("")` | Refused. Previously stored as `''` with no canonical write; no caller in the tree used it. |
| `None`, `svn_to_git`, scoped, proven | Writes only the `commit_map` row. It writes no canonical outcome, checkpoint, sync record, watermark, echo receipt or repository column. |
| `None`, otherwise | Refused with no write: `git_to_svn`, unscoped (`repo_id` NULL or empty), a repository without `scope_uuid`, or no matching canonical `intentionally_filtered_no_target` outcome. |

Recording the canonical filtered outcome itself (with its reason and policy
snapshot) belongs to the producer that decides a revision is filtered. That
producer is #52 path-filter work and is not part of this change.

The raw writers in import, Git push, SVN commit, personal and pair-refresh code
write real SHAs only. Applied SVN→Git push finalization records the real SHA
even when an earlier no-target row exists for the same revision, so applied
evidence is never suppressed by a NULL row.

## Reader contract

**Value readers return exactly what they would return if the NULL rows were
absent.** **Existence predicates count a NULL row only when it is proven
no-target.**

| Reader | v15 behavior |
| --- | --- |
| `get_git_sha_for_svn_rev` | Earliest non-NULL SHA for the revision (the previous effective order); `None` when the revision has no row or only NULL rows. |
| `get_svn_rev_for_git_sha`, `is_git_sha_synced` | Unchanged: `git_sha = ?` never matches NULL. |
| `is_svn_rev_synced` | True for a non-NULL row, or for a proven no-target row. An unverified NULL row does not count. |
| `get_last_git_hash` (feeds the personal checkpoint P) | Newest non-NULL row; NULL rows are skipped. If no non-NULL row exists, the existing `sync_records` fallback applies, as for an empty table. |
| `get_last_svn_revision` (unscoped fallback watermark, CLI and web status) | `MAX(svn_rev)` over non-NULL rows only, so a NULL row never advances the fallback watermark. |
| `list_commit_map` / `CommitMapEntry` | Unchanged type (`git_sha: String`); lists rows that have a Git target. |
| `list_commit_map_rows` / `CommitMapRow` (new) | Every row, with `git_sha: Option<String>` and an explicit `target`. |
| `/api/commit-map` | `git_sha` may be JSON `null`; each entry has `target_state`: `git_commit`, `filtered_no_target`, `no_target_unverified` or `legacy_empty_value`. |
| Repository Detail and Dashboard mapping tables | Render the target state in words; they never read `.substring` of a null. |
| Personal `is_personal_svn_rev_synced` | Same rule as `is_svn_rev_synced`. Unscoped NULL rows never count, even with a personal sync record. |
| Managed-removal counts and tombstones | Count every row. A baseline proof requires an exact SHA match, so a NULL row never proves a baseline. |
| Pair-refresh contradiction checks | Unchanged: a NULL row at the same revision is treated as contradictory, so publication is refused. |
| `retained_legacy_baseline` | Counts every row (conservative). |
| Audit log writers, conflict detector, echo receipts | Do not read `commit_map`. |

Classifying a row as proven reads the repository's stored `scope_uuid` and its
generation key directly. It never mints a `scope_uuid` or revokes legacy reads,
which the general helpers can do, so a display or predicate never writes.

## Scope and compatibility

- With empty `allowed_paths` (the default), nothing writes a NULL row, so
  existing installations behave exactly as before. Existing data has no NULL
  rows.
- Personal and unscoped installations cannot write no-target rows, because they
  have no canonical scope. NULL rows that arrive another way (raw SQL, restored
  files) stay inert.
- The #52 SVN→Git filter must record the canonical filtered outcome first, then
  this legacy row. Its functional tests are the primary validation of the
  end-to-end behavior and are not claimed here.
