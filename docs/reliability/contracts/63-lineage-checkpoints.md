# #63 contract: canonical lineage, directional checkpoints, non-destructive migration

**Status:** operational activation. This contract's schema is durable
`repositories.scope_uuid` (v13) plus typed canonical lineage/checkpoint tables
(v14). Ordinary exclusive-owner startup is now SQLite `user_version=15`, which
adds #54's nullable `commit_map.git_sha` on top of v14. Candidate CopySession tables
`pair_lineages` / `pair_frontiers` / `pair_outcomes` remain fixture-only and
must stay absent from ordinary `STARTUP`. `#54` nullable `commit_map.git_sha`
is not part of this contract. It is operational v15, which reuses the candidate
v13 rebuild shape; see [54-nullable-commit-map.md](54-nullable-commit-map.md).

**Depends on:** #62 catalog (`docs/reliability/acceptance-matrix.json`).
**Coordinates with:** #54 nullable `commit_map.git_sha`; do not introduce a
second migration directory. Current migrations are embedded in
`crates/core/src/db/schema.rs::MIGRATIONS`.
**Does not absorb:** monorepo path projection (#52/#60).

## Supported operational schema

| `user_version` | Ordinary meaning | Who may write |
| --- | --- | --- |
| 12 | Pre-scope legacy file. Lone file-backed install with `last_git_sha_<id>` stays at 12 through `Database::initialize` (R01). | Exclusive `{data_dir}/reposync.lock` owner. Old executable may reopen only this snapshot. |
| 13 | Durable `repositories.scope_uuid` plus scoped echo/receipt/checkpoint KV. Intermediate committed state if v14 has not yet applied. | Same exclusive owner. A v12 executable is not a safe concurrent writer. |
| 14 | v13 plus `canonical_lineage`, `canonical_checkpoints`, `canonical_mapping_outcomes`. Zero `pair_%` tables. Intermediate committed state if v15 has not yet applied. | Same exclusive owner. |
| 15 | Current. v14 plus nullable `commit_map.git_sha` ([#54 contract](54-nullable-commit-map.md)). | Current executable with exclusive owner. |
| >15 | Unsupported. | Refused before any migration write (`UnsupportedSchema`). `user_version` is left unchanged. |

Candidate CopySession v13/v14 (`candidate_migration.rs`) is a sealed-copy
qualification registry. It is **not** the operational schema and must not be
registered in `MIGRATIONS`.

### Mixed-version writer policy

One owner per data directory (`reposync.lock` plus the durable writer fence).
SQL and `PRAGMA user_version` for each migration run in **one** SQLite
transaction (`run_migrations` / `ensure_v13_migration`). An old executable
must not write after new records exist. A newer `user_version` is refused
before writes. Pair-refresh recovery opens only an already-current WAL
database (`CURRENT_SCHEMA_VERSION`).

## Logical identity (one pair generation)

A generation is immutable and keyed by `(scope_uuid, generation)`. The human
`repositories.id` is a label only.

Required identity:

- SVN UUID, canonical root URL, branch-relative path, path incarnation
  (copy-from path+revision or explicit creation), verified source
  UUID/path/revision, and the projected baseline tree
- Git provider/repository identity, full ref name, verified baseline object ID
  and its relation to an SVN import mapping for the same lineage/projection
- Projection/policy version and hash; empty policy remains today’s full-import
  default

A Git feature ref is valid only when its ancestor chain reaches a verified SVN
import for that same lineage and projection. Unrelated Git-first roots are
refused. Commit messages, trailers, timestamps, short SHAs, patch-ids, and
equal final trees are hints, not lineage.

Live SVN UUID and path incarnation are compared in `fetch_svn_changes` **before**
the `head_rev <= last_rev` early return. Independently established
initialization (no retained mappings/cursors, an import snapshot pin whose UUID
matches live, or a producer-recorded UUID from a completed history import,
late-pair copy in the same SVN repository, or a fixture that observed the live
repo it created) pins `canonical_lineage` and records the Git baseline/ref
from the repository row. A live `svn.info` UUID is not independently established
over a predecessor full-import journal or published Git prefix that lacks a
historical UUID key or snapshot pin: resume holds (`svn_identity_unproved`)
before recording replacement authority, skipping old revisions, or publishing.
Legacy adoption with retained mappings and no proved UUID holds rather than
pinning a same-URL replacement. Retained `last_svn_rev` / `last_git_sha` alone
is never proof. A later UUID or incarnation mismatch holds only that pair
(`svn_identity_changed` / `reconciliation_required`) and does not silently
sync a recreated repo at a low or equal revision under stale lineage.

## Directional checkpoints

One authoritative logical checkpoint per
`(scope_uuid, generation, direction)`:

| Direction | Meaning |
| --- | --- |
| SVN→Git | last SVN revision *examined and resolved* for this pair |
| Git→SVN | last Git frontier *examined and resolved* for this pair |

An SVN revision produced by Git→SVN is that operation’s **effect**, not an
incoming SVN cursor advance. Advancing a checkpoint requires the same SQLite
transaction as the resolved mapping/outcome. Observed remote HEAD is never
adopted merely because it exists.

Ordinary managed sync writes `applied_and_verified` in the directional journal
`finalize_tx` (SVN→Git in `git_push_operations`, Git→SVN in
`svn_commit_operations`) together with the applied `sync_records` mapping and
operation completion. `classify_mapping_outcome` /
`load_directional_checkpoint` are the canonical readers: a missing row is
`pending`, not a license to adopt a replacement identity. A generation read
or persist fault refuses the whole journal transaction (no watermark
advancement, no completed operation). Legacy `repositories.last_svn_rev` and
import baseline remain the SVN fetch cursor until an exclusive-owner reader
cutover.

Typed mapping outcomes, not overloaded NULL/missing:

- `applied_and_verified`
- `intentionally_filtered_no_target`
- `pending`
- `locally_published_not_remote`
- `unknown_reconciliation_required`

A no-target row has a reason, policy snapshot, and NULL target as appropriate.
NULL is not “missing row.” Empty-string SHA is refused. A missing row classifies
as `pending`. Preserve source/target parents/trees or stable fingerprints
sufficient to verify an external effect. The canonical outcome table keeps the
explicit outcome and NULL targets. Operational `commit_map.git_sha` may be NULL
only as a legacy projection of an already recorded
`intentionally_filtered_no_target` outcome
([#54 contract](54-nullable-commit-map.md)). A NULL `commit_map` row does not
mint `applied_and_verified` or `intentionally_filtered_no_target`.

## Operational v13 scope identity (shipped slice)

### Verified repository UUID vs human repository id

| Concept | Role | Continuity |
| --- | --- | --- |
| **`scope_uuid`** | Durable repository identity for echo receipts, generations, inbound Git checkpoints, and history blocks | Survives only while the repository row exists; a new row gets a new UUID |
| **`repositories.id`** | Human-chosen label for config, UI, and `sync_records.repo_id` | May be reused after delete + re-register; **must never** be treated as proof that scoped KV/receipt state from an earlier row still applies |

Admission and checkpoint logic bind evidence to **`scope_uuid`**. A receipt or
cursor key that matches only the human `id` (or omits `scope_uuid` on a managed
row) is unverified. Matching `repo_id` in JSON without the same `scope_uuid` as
the live row does not prove repository continuity.

- Each managed repository row carries an immutable `scope_uuid` assigned at
  registration (v13 backfill for existing rows). The human-chosen repository
  `id` is a label only: matching `id` after delete + re-register does **not**
  prove continuity of repository identity. Authority always follows
  `scope_uuid` (and generation-scoped receipts bound to that UUID).
- Git→SVN **inbound** handled checkpoints live in UUID-scoped KV
  (`last_git_sha_<scope_uuid>`). A repo-id mirror (`last_git_sha_<id>`) may
  exist for operators and isolation tests; when both are present they must
  agree or sync blocks with `ambiguous_checkpoint`.
- `repositories.last_git_sha` remains the SVN-emitted Git tip (split cursor);
  it is not interchangeable with the inbound checkpoint KV.
- Echo generation, no-target receipts, and durable history blocks key off
  `scope_uuid`, not the human `id`, so delete + re-register cannot reuse stale
  scoped state.
- Managed repositories with a `scope_uuid` accept no-target receipts only when
  the receipt JSON carries the same `scope_uuid`. Legacy repo-id receipts
  without `scope_uuid` are never authoritative for those rows, even after other
  repository rows are removed. Field-less receipts are rejected at generation
  `>= 2`.
- Legacy repo-id KV/receipts without `scope_uuid` may be read only for
  pre-v13 single-repository databases during migration; v13 migration binds
  legacy rows to the original UUID or leaves them quarantined.

### Exact ownership for legacy scoped KV (v13 migration and delete)

Human `repositories.id` and durable `scope_uuid` must never be conflated when
matching KV key names. SQL `LIKE` with a repo-id prefix is unsafe: unescaped `_`
acts as a single-character wildcard, and prefix matching cannot prove that a
longer id (for example `pair2` or `pair_child`) belongs to a shorter id
(`pair`).

**Rule:** migrate, quarantine, or delete a legacy no-target receipt key only when
**exact ownership** is proven:

1. Parse the key as `handled_{git|svn}_no_target_<scope_token>_<suffix>` where
   `<suffix>` is validated (40-character Git SHA, or `g<generation>_` plus SHA /
   numeric SVN revision) so `<scope_token>` is unambiguous.
2. Treat the key as owned by a human repository only when `<scope_token>` equals
   that repository’s `id` exactly (not a prefix or LIKE match).
3. When the JSON payload includes `repo_id`, it must equal that same repository
   `id`; otherwise leave the key unchanged (fail closed).
4. On hard delete, remove UUID-scoped receipt keys only when `<scope_token>`
   equals that row’s `scope_uuid` exactly, plus the fixed legacy keys
   (`last_git_sha_<id>`, and so on). Never delete through a bare
   `handled_*_no_target_<prefix>%` pattern.

Namespace-wide scans may use `GLOB` on the fixed literal prefix
(`handled_git_no_target_*`); per-repo actions always filter with the exact-parse
rules above.

## Current v12/v13 compatibility (must keep working)

Operational authority today is the bounded table in [design.md](../design.md):

- Equal repository-column and scoped KV Git cursors, plus a scoped applied
  mapping or a valid no-target receipt
- Split column/KV only with applied emitted mapping, handled proof, full SHAs,
  and handled→emitted ancestry
- Absent KV never selects the first surviving `sync_records` row or another
  repository’s global maximum
- `run_maintenance(90)` may prune only old non-applied diagnostic rows

Healthy legacy pairs stay enabled. Disabled pairs stay disabled. IDs, config,
credentials/inheritance, mappings, valid checkpoints, files, and parent/child
links survive. No automatic reimport, reset, remote deletion, or blanket
disable. Parent credential rotation never overwrites an explicit child override
or revocation; a single fallible scoped resolver distinguishes
absent / inherited / global / explicit / revoked / read-failure.

## Candidate storage (fixture-only; not activated)

`crates/core/src/db/candidate_migration.rs` implements candidate v13 (nullable
`commit_map.git_sha` rebuild) and candidate v14 (`pair_lineages`,
`pair_frontiers`, `pair_outcomes`, evidence links) only behind
`reliability-fixture` and `CopySession`. [migration-proposal.md](../migration-proposal.md)
is that physical contract. Ordinary startup must keep passing with zero
candidate `pair_%` tables.

Operational #54 landed as schema v15 and reuses that rebuild. Do not register
a competing `crates/core/src/db/migrations/` tree or a second nullable rebuild.

## Backup and honest rollback

1. Identify the exact running executable/schema. Quiesce writers. Take a
   consistent SQLite backup including WAL, configuration, key material, and
   local refs/workdirs. Never start a copied installation with scheduling or
   outbound access.
2. Inventory on the copy. Do not resolve a missing per-pair cursor from
   another pair’s global maximum.
3. Reject a future `user_version` before writing. Run ordered, transactional,
   restart-safe changes with a single-daemon owner. Wrap SQL and
   `PRAGMA user_version` in **one** SQLite transaction.
4. Classify proven / ambiguous / external-effect-unresolved. Only proven pairs
   receive a generation. Others stay read-safe.
5. Validate foreign keys and integrity after each version. Repeated or
   interrupted startup converges without duplicate rows.
6. Startup refuses unsupported newer schemas.

Rollback is honest: old executable plus a pre-upgrade snapshot is restore
**only before** new external effects. After SVN/Git writes, inspect remotes
and roll forward. A destructive down migration that drops typed/NULL outcomes
is unacceptable. A committed v13 followed by a failed v14 remains valid v13,
not a claimed 12→14 rollback.

## Required tests before claiming #63

Fresh DB; pinned old-import fixture; multi-repo overlapping revisions;
disabled/error/pending-import; conflicting cursor sources; interrupted
migration; write failure; unsupported schema; no-change first sync; one new
change each direction on a copy; restore-before-writes; refuse blind
restore-after-writes; SVN UUID / recreated path-incarnation hold; typed
outcomes that do not overload NULL/empty/missing; RS-11 http(s) origin
credential isolation. Assert real remote trees/logs as well as DB rows.

Deployed-version qualification stays **NOT ESTABLISHED** until Chris supplies
a safe inventory. No production DB or secrets in the tree.
