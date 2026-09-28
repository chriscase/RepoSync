# Candidate frontier identity boundary (N01)

This checklist covers only the **copy-only v14 candidate**. The original
installation and the operational v12 schema are unchanged. Candidate SQL is
compiled only for `reliability-fixture` and is not an operational migration.

| Candidate table | Declared conflict identities | Implicit identity | Supported write path and boundary |
| --- | --- | --- | --- |
| `repo_migration_state` | `repo_id` primary key | None (`WITHOUT ROWID`) | Copy migration records one disposition per repository; copy readers only read it. There is no operational disposition service in this slice. |
| `pair_lineages` | `(repo_id,generation)` primary key; `(repo_id,generation,projection_version,policy_sha256)` unique | None (`WITHOUT ROWID`) | Copy migration inserts a proved baseline. UPDATE and same-key INSERT replacement are guarded; children restrict deletion when foreign keys are enabled. Arbitrary raw DML is outside the supported writer contract. |
| `pair_outcomes` | `id` primary key; `(repo_id,generation,direction,source_key)` unique; referenced composite unique key | None (`WITHOUT ROWID`) | Checked `advance_frontier` inserts resolved outcomes. Direct resolved UPDATE/DELETE and prospective resolved-victim INSERT/UPDATE replacement are guarded even with recursive triggers off. Pending bookkeeping remains possible. |
| `pair_frontiers` | `(repo_id,generation,direction)` primary key | None (`WITHOUT ROWID`) | Copy migration inserts baselines; checked `advance_frontier` moves only a matching directional pair. Baseline, transition and direct-delete triggers apply. No rowid/oid/_rowid_ can select another pair as a replacement victim. |
| `legacy_evidence_links` | `(repo_id,generation,legacy_table,legacy_key)` primary key; `(legacy_table,legacy_key)` unique | None (`WITHOUT ROWID`) | Copy migration records original ownership. Readers require exact links and treat absent/conflicting evidence as unresolved. Raw link mutation is not a supported recovery operation. |

The N01 exact case exercises INSERT OR REPLACE baseline creation and a valid
forward UPDATE OR REPLACE attempt with all three former rowid aliases, two
directions, independent repositories or separate generations, and both
recursive-trigger settings. Each rejection preserves complete outcomes and
frontiers, the database bytes and reopened CopyReaders/DTO results for both
original pairs. Legitimate baseline creation and checked forward transitions
also pass. A manually constructed generation-two lineage in this structural
test is **not** proof of another remote import or Git ancestry.

The physical-shape check refuses an old rowid-backed prototype at v14 without
repair. It does not reset a checkpoint or reimport. These controls do not
authorize arbitrary DDL, unrestricted SQL clients, general #64 recovery, or
activation of candidate storage in normal startup.
