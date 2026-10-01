# #63 contract: canonical lineage, directional checkpoints, non-destructive migration

**Status:** design/contract for review. No DDL, reader activation, or installer
change ships in this PR. Normal startup remains SQLite `user_version=12`.

**Depends on:** #62 catalog (`docs/reliability/acceptance-matrix.json`).
**Coordinates with:** #54 nullable `commit_map.git_sha`; do not introduce a
second migration directory. Current migrations are embedded in
`crates/core/src/db/schema.rs::MIGRATIONS`.
**Does not absorb:** monorepo path projection (#52/#60).

## Logical identity (one pair generation)

A generation is immutable and keyed by `(repository_id, generation)`.

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

## Directional checkpoints

One authoritative logical checkpoint per
`(repository_id, generation, direction)`:

| Direction | Meaning |
| --- | --- |
| SVN→Git | last SVN revision *examined and resolved* for this pair |
| Git→SVN | last Git frontier *examined and resolved* for this pair |

An SVN revision produced by Git→SVN is that operation’s **effect**, not an
incoming SVN cursor advance. Advancing a checkpoint requires the same SQLite
transaction as the resolved mapping/outcome. Observed remote HEAD is never
adopted merely because it exists.

Typed mapping outcomes, not overloaded NULL/missing:

- `applied_and_verified`
- `intentionally_filtered_no_target`
- `pending`
- `locally_published_not_remote`
- `unknown_reconciliation_required`

A no-target row has a reason, policy snapshot, and NULL target as appropriate.
NULL is not “missing row.” Preserve source/target parents/trees or stable
fingerprints sufficient to verify an external effect.

## Current v12 compatibility (must keep working)

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
disable.

## Candidate storage (already written, not activated)

`crates/core/src/db/candidate_migration.rs` implements v13 (nullable
`commit_map.git_sha` rebuild) and v14 (`pair_lineages`, `pair_frontiers`,
`pair_outcomes`, evidence links) only behind `reliability-fixture` and
`CopySession`. [migration-proposal.md](../migration-proposal.md) is the
physical contract. `STARTUP_V12` must keep passing: zero candidate tables in
an ordinary database.

If #54 lands another version number, reuse that rebuild. Do not register a
competing `crates/core/src/db/migrations/` tree.

## Migration sequence (when activation is separately authorized)

1. Identify the exact running executable/schema. Quiesce writers. Take a
   consistent SQLite backup including WAL, configuration, key material, and
   local refs/workdirs. Never start a copied installation with scheduling or
   outbound access.
2. Inventory on the copy. Do not resolve a missing per-pair cursor from
   another pair’s global maximum.
3. Reject a future `user_version` before writing. Run ordered, transactional,
   restart-safe changes with a single-daemon owner. Wrap SQL and
   `PRAGMA user_version` in **one** SQLite transaction (today they are
   separate in `run_migrations`).
4. Classify proven / ambiguous / external-effect-unresolved. Only proven pairs
   receive a generation. Others stay read-safe.
5. Validate foreign keys and integrity after each version. Repeated or
   interrupted startup converges without duplicate rows.
6. Startup refuses unsupported newer schemas. Document mixed-version writer
   policy: one owner per data directory. An old executable is not a safe
   concurrent writer after new records exist.

Rollback is honest: old executable plus a pre-upgrade snapshot is restore
**only before** new external effects. After SVN/Git writes, inspect remotes
and roll forward. A destructive down migration that drops typed/NULL outcomes
is unacceptable.

## Smallest next implementation slice (after this review)

Do **not** activate v13/v14.

1. Make `run_migrations` apply one migration SQL batch and its
   `user_version` bump in a single transaction, with a failing-midway test
   that leaves the previous version intact.
2. Add an exclusive data-directory owner check at ordinary startup (reuse the
   daemon lockfile; do not invent a distributed lock).
3. Keep operational readers on v12. Add tests that a future `user_version`
   is refused and that a healthy v12 fixture opens unchanged.

Activation of candidate tables, operational typed readers, and installer
wiring are a later reviewed slice.

## Required tests before claiming #63

Fresh DB; pinned old-import fixture; multi-repo overlapping revisions;
disabled/error/pending-import; conflicting cursor sources; interrupted
migration; write failure; unsupported schema; no-change first sync; one new
change each direction on a copy; restore-before-writes; refuse blind
restore-after-writes. Assert real remote trees/logs as well as DB rows.

Deployed-version qualification stays **NOT ESTABLISHED** until Chris supplies
a safe inventory. No production DB or secrets in the tree.
