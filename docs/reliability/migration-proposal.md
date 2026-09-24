# Candidate #54-aligned #63 migration proposal (review only)

**Status:** proposed SQL and data contract. No DDL in this file has been executed by PR #72. The existing schema is version 12. The deployed executable/schema, remote SVN UUID and copy ancestry, and remote Git identity remain **NOT ESTABLISHED**. The sealed local inventory is a prerequisite, not an automatic qualification decision. [GOAL.md](GOAL.md) and issues [#54](https://github.com/chriscase/RepoSync/issues/54), [#63](https://github.com/chriscase/RepoSync/issues/63), and [#64](https://github.com/chriscase/RepoSync/issues/64) retain their acceptance criteria.

## One migration sequence, conditional on review

The repository actually stores ordered SQL in `crates/core/src/db/schema.rs::MIGRATIONS`. Today's `run_migrations` executes each SQL batch and updates `PRAGMA user_version` separately. It neither rejects a newer version nor makes a table rebuild and version update atomic. The implementation proposal is one coordinated sequence: **candidate v13** performs #54's nullable `commit_map.git_sha` rebuild; **candidate v14** adds #63 generation ownership and directional evidence. If #54 lands first with a different number, rebase these numbers and reuse that migration; never apply a second competing rebuild. None of these changes are in runtime code yet.

Before applying any migration, the future runner must verify a known starting schema/column/index shape, refuse `user_version > supported`, quiesce and fence every writer of the data directory, make an access-controlled consistent SQLite backup including WAL plus configuration, encryption-key ownership and local refs/workdirs, and inventory remote identities through a separately reviewed read-only process. Use `BEGIN IMMEDIATE` for each ordered migration, execute the SQL and its data conversion, validate row counts/FKs/integrity, set `PRAGMA user_version` **inside the same transaction**, then commit. An error rolls back that version's SQL and version number. Restart repeats only unapplied versions; it must reject a partially modified shape even if a version marker was forged. No old/new daemon may write the directory together.

### Candidate v13: #54 nullable mapping, preserving every old row

The following is a **candidate**, not an applied script. It preserves the existing v12 columns and primary IDs; only `git_sha` becomes nullable. It does not encode a filtered outcome retroactively.

```sql
-- Execute inside the future runner's BEGIN IMMEDIATE transaction.
CREATE TABLE commit_map_v13 (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  svn_rev INTEGER NOT NULL,
  git_sha TEXT,
  direction TEXT NOT NULL CHECK (direction IN ('svn_to_git','git_to_svn')),
  synced_at TEXT NOT NULL,
  svn_author TEXT NOT NULL DEFAULT '',
  git_author TEXT NOT NULL DEFAULT '',
  repo_id TEXT
);
INSERT INTO commit_map_v13
  (id,svn_rev,git_sha,direction,synced_at,svn_author,git_author,repo_id)
SELECT id,svn_rev,git_sha,direction,synced_at,svn_author,git_author,repo_id
FROM commit_map ORDER BY id;
-- Assert identical COUNT(*), id/column digest, and sqlite_sequence continuity.
DROP TABLE commit_map;
ALTER TABLE commit_map_v13 RENAME TO commit_map;
CREATE INDEX idx_commit_map_svn_rev ON commit_map(svn_rev);
CREATE INDEX idx_commit_map_git_sha ON commit_map(git_sha);
CREATE INDEX idx_commit_map_repo_svn ON commit_map(repo_id,svn_rev);
CREATE INDEX idx_commit_map_repo_git ON commit_map(repo_id,git_sha);
-- Future runner: PRAGMA user_version = 13; validate; COMMIT.
```

A *new* NULL `git_sha` means an SVN revision was intentionally handled without a Git commit **only when** a repository/generation-owned `filtered_no_target` outcome also records the exact active path policy and source SVN identity. A NULL in an old or ownerless row is unresolved; a missing row is not a filtered decision. #54 readers must use a typed result: `get_git_sha_for_svn_rev` returns `None` for NULL, `is_svn_rev_synced` counts the row, while `get_last_git_hash` must skip NULL outcomes rather than treating the last row as a Git frontier. Audit status API, conflict detector, mapping writers and legacy global fallback for this distinction. Test fresh/old databases, a NULL row surviving restart, and all retained legacy IDs and indexes. A down rebuild is allowed only if no NULL/new outcome or externally published effect would be lost; otherwise refuse and retain v13.

### Candidate v14: permanent ownership and typed outcomes

Only a **proved** pair receives a generation row. Equal revision numbers, names, message trailers, local byte trees or an inventory label alone cannot assign generation 1. Rows awaiting proof remain in the unchanged legacy tables and receive a repository-level read-safe disposition.

```sql
CREATE TABLE repo_migration_state (
  repo_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE RESTRICT,
  disposition TEXT NOT NULL CHECK (disposition IN
    ('qualified','needs_reconciliation','external_effect_unknown','not_qualified')),
  reason_code TEXT NOT NULL,
  evidence_manifest_sha256 TEXT NOT NULL,
  reviewed_at TEXT NOT NULL
);
CREATE TABLE pair_lineages (
  repo_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE RESTRICT,
  generation INTEGER NOT NULL CHECK (generation > 0),
  svn_uuid TEXT NOT NULL CHECK (length(svn_uuid) > 0),
  svn_root_url TEXT NOT NULL CHECK (length(svn_root_url) > 0),
  svn_branch_path TEXT NOT NULL,
  source_svn_uuid TEXT NOT NULL CHECK (length(source_svn_uuid) > 0),
  source_svn_path TEXT NOT NULL CHECK (length(source_svn_path) > 0),
  source_svn_rev INTEGER NOT NULL CHECK (source_svn_rev > 0),
  copy_from_path TEXT,
  copy_from_rev INTEGER,
  baseline_svn_rev INTEGER NOT NULL CHECK (baseline_svn_rev > 0),
  baseline_svn_tree_sha256 TEXT NOT NULL CHECK (length(baseline_svn_tree_sha256) = 64),
  git_provider TEXT NOT NULL CHECK (length(git_provider) > 0),
  git_repo_identity TEXT NOT NULL CHECK (length(git_repo_identity) > 0),
  git_ref TEXT NOT NULL CHECK (length(git_ref) > 0),
  baseline_git_sha TEXT NOT NULL CHECK (length(baseline_git_sha) IN (40,64)),
  projection_version INTEGER NOT NULL CHECK (projection_version > 0),
  projection_json TEXT NOT NULL CHECK (json_valid(projection_json)),
  created_at TEXT NOT NULL,
  PRIMARY KEY (repo_id,generation),
  CHECK ((copy_from_path IS NULL AND copy_from_rev IS NULL) OR
         (copy_from_path IS NOT NULL AND copy_from_rev > 0))
);
CREATE TABLE pair_frontiers (
  repo_id TEXT NOT NULL,
  generation INTEGER NOT NULL,
  direction TEXT NOT NULL CHECK (direction IN ('svn_to_git','git_to_svn')),
  handled_svn_rev INTEGER,
  handled_git_sha TEXT,
  emitted_git_sha TEXT,
  emitted_svn_rev INTEGER,
  evidence_outcome_id TEXT,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (repo_id,generation,direction),
  FOREIGN KEY (repo_id,generation) REFERENCES pair_lineages(repo_id,generation) ON DELETE RESTRICT,
  CHECK ((direction='svn_to_git' AND handled_svn_rev IS NOT NULL AND handled_svn_rev > 0
          AND handled_git_sha IS NULL AND emitted_svn_rev IS NULL) OR
         (direction='git_to_svn' AND handled_git_sha IS NOT NULL
          AND length(handled_git_sha) IN (40,64) AND handled_svn_rev IS NULL
          AND emitted_git_sha IS NULL)),
  CHECK (emitted_svn_rev IS NULL OR emitted_svn_rev > 0),
  CHECK (emitted_git_sha IS NULL OR length(emitted_git_sha) IN (40,64))
);
CREATE TABLE pair_outcomes (
  id TEXT PRIMARY KEY,
  repo_id TEXT NOT NULL,
  generation INTEGER NOT NULL,
  direction TEXT NOT NULL CHECK (direction IN ('svn_to_git','git_to_svn')),
  source_svn_rev INTEGER,
  source_git_sha TEXT,
  outcome TEXT NOT NULL CHECK (outcome IN
    ('applied_verified','filtered_no_target','empty_no_target',
     'semantic_no_delta','pending','locally_published_not_remote',
     'effect_unknown','reconciliation_required')),
  target_git_sha TEXT,
  target_svn_rev INTEGER,
  projection_version INTEGER NOT NULL CHECK (projection_version > 0),
  evidence_json TEXT NOT NULL CHECK (json_valid(evidence_json)),
  recorded_at TEXT NOT NULL,
  FOREIGN KEY (repo_id,generation) REFERENCES pair_lineages(repo_id,generation) ON DELETE RESTRICT,
  CHECK ((direction='svn_to_git' AND source_svn_rev IS NOT NULL AND source_svn_rev > 0
          AND source_git_sha IS NULL AND target_svn_rev IS NULL) OR
         (direction='git_to_svn' AND source_git_sha IS NOT NULL
          AND length(source_git_sha) IN (40,64) AND source_svn_rev IS NULL
          AND target_git_sha IS NULL)),
  CHECK (outcome NOT IN ('filtered_no_target','empty_no_target','semantic_no_delta')
         OR (target_git_sha IS NULL AND target_svn_rev IS NULL)),
  CHECK (outcome != 'applied_verified' OR
         (direction='svn_to_git' AND target_git_sha IS NOT NULL AND
          length(target_git_sha) IN (40,64)) OR
         (direction='git_to_svn' AND target_svn_rev IS NOT NULL AND target_svn_rev > 0))
);
CREATE UNIQUE INDEX uq_pair_svn_source ON pair_outcomes(repo_id,generation,source_svn_rev)
  WHERE direction='svn_to_git';
CREATE UNIQUE INDEX uq_pair_git_source ON pair_outcomes(repo_id,generation,source_git_sha)
  WHERE direction='git_to_svn';
CREATE TABLE legacy_evidence_links (
  repo_id TEXT NOT NULL,
  generation INTEGER NOT NULL,
  legacy_table TEXT NOT NULL CHECK (legacy_table IN ('commit_map','sync_records','kv_state','watermarks','import_progress')),
  legacy_key TEXT NOT NULL,
  interpretation TEXT NOT NULL,
  PRIMARY KEY (repo_id,generation,legacy_table,legacy_key),
  FOREIGN KEY (repo_id,generation) REFERENCES pair_lineages(repo_id,generation) ON DELETE RESTRICT
);
-- Future runner: load reviewed, complete migration-plan decisions with bound
-- parameters; assert one repo_migration_state row per repositories row.
-- Insert pair_lineages/frontiers/outcomes/links only for independently proved
-- pairs; validate all FK/index/row-preservation assertions.
-- Future runner: PRAGMA user_version = 14; COMMIT.
```

`pair_frontiers` distinguishes handled source from emitted target. A Git→SVN emission revision is **not** an incoming SVN cursor advance. The outcome and frontier advance must share one SQLite transaction. An operation journal and post-external-write recovery remain a later #64 migration; these tables do not claim cross-system atomicity. Keep `sync_records` applied rows and any legacy `commit_map` rows permanently until their new generation/outcome evidence is independently proved and a separate retention policy is reviewed. Only non-applied diagnostics may expire, so storage may grow in the interim.

## Exact legacy classification-to-row proposal

| Inventoried shape | Proposed v14 disposition and conversion |
| --- | --- |
| Pinned old single import: equal scoped Git copies, scoped applied SVN→Git row, matching SVN cursor, verified SVN UUID/path/source ancestry, remote Git ref ancestry and policy | Assign generation 1 **only after those remote proofs**; copy observed handled/emitted frontiers and link original row IDs. No tip change or replay. The current local fixture has only `qualified_fixture_shape`; it does not itself satisfy remote proof. |
| Two independent imports both at SVN r2 | Separate `(repo_id,1)` lineages only after each own UUID/path/ref/source proof. Never use global r2 or last imported Git SHA as the other's authority. Preserve each old mapping and credential owner. |
| Disabled third repository | Preserve `enabled=0`, configuration, parent and secrets. `not_qualified` is not reactivation; no scheduler poll. A future generation needs its own proof. |
| Column SVN=2, scoped SVN=999, global SVN=888, import watermark=777 | Record all four and import progress independently. Reader column precedence explains which current code uses, but conversion is `needs_reconciliation` until the mismatched scoped/import claims and pending work are checked. Never choose 999 or another maximum. |
| Repository column Git / scoped KV split after SVN publication | Keep the KV handled frontier and emitted column separate only with repository-owned applied mapping, baseline receipt and full ancestry. Conflicts are read-safe. |
| Historical filtered decisions behind current cursors; current v1 `filtered` or `empty_commit` receipt | Preserve every legacy receipt byte and policy. Convert only the exact proved source under its original projection; historical filtered choices require history/policy enumeration rather than inheriting the current cursor's policy check. |
| v1 `no_svn_delta` or old v2 content-only receipt | Preserve raw receipt, classify `needs_reconciliation`; neither proves mode/type/SVN-property equivalence. Do not relabel as v3 or advance a frontier from it. |
| New v3 `regular_file_bytes_no_properties_v1` receipt | Candidate semantic evidence only for regular Git mode 100644 and a pinned SVN regular file with matching bytes and no file properties; still require pair UUID/path/ref lineage and remote-effect checks before canonical conversion. |
| Already-pruned baseline, missing receipt/mapping, unsupported linked worktree/ref, unresolved credential parent, endpoint/UUID replacement or copy-origin mismatch | Preserve old state and mark `needs_reconciliation`; do not infer a baseline from a surviving row, same bytes or a title. |
| Effect-unknown marker or an operation that may have published without a local checkpoint | `external_effect_unknown`; no canonical frontier advance or blind retry. Later #64 reconciliation inspects the pinned remote target. |

The new inventory reports actual repository columns, scoped SVN/Git KV, global SVN/Git references, import `watermarks`, singleton `import_progress`, scoped and global mapping fallback references, schema columns/indexes/FKs, and credential ownership references. It keeps full sanitized reports for the pinned fixture and explicitly synthetic overlays. It cannot establish remote UUID/copy/ref identity or the installed binary. Any unknown legacy schema, historical copy shape, or inheritance path is read-safe, never auto-normalized.

## Preservation and interruption assertions required before implementation review

For each pinned old fixture and each synthetic overlay, record a private pre/post manifest of DB, WAL, config, secret/key ownership, refs/workdirs, content hashes, modes and owner. A sanitized review artifact records row counts and ID-keyed digests for `repositories`, `commit_map`, `sync_records`, `kv_state`, `watermarks`, `import_progress`, audit/conflict tables and new tables; all old row IDs/bytes remain reachable. Assert enabled flags, parent chains, credential-source references, endpoints/settings, policy JSON, old cursors, Git refs and SVN full trees unchanged. Assert `PRAGMA integrity_check=ok`, `foreign_key_check` empty, unique indexes effective, `user_version` correct, no outbound call, and equal result on restart. Fault after each v13 rebuild substep and each v14 insert/version boundary: either the pre-version state survives intact or the complete version is visible. Read-only/disk-write failures must leave no reported success.

Before any new external effect, restoring the old executable with the consistent pre-upgrade snapshot can be tested. After SVN accepts a commit or Git accepts a push, an old DB snapshot is stale: preserve the new data, stop writers, inspect actual remote UUID/path/revision or ref parent/tree and reconcile/roll forward using durable intent. This later #64 path is **design only**; no rollback, reimport or checkpoint reset is a substitute. A down migration must refuse if typed/NULL outcomes or external effects cannot be represented losslessly by the old schema.

**Next approval boundary:** review this proposal, qualify additional historical/deployed shapes and the actual installer/daemon ownership path, then separately authorize and test an implementation. PR #72 does not execute schema SQL.
