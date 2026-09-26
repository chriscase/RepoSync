# Candidate #54/#63 copy migration contract

**Status:** candidate v13/v14 are implemented only behind `reliability-fixture` and the explicit `CopySession` test entry point. Normal schema registration still ends at v12. Installer, daemon and scheduler activation is outside this review. The previous reviewed proposal is preserved byte-for-byte in [review-6-migration-proposal.md](review/review-6-migration-proposal.md); its illustrative SQL is superseded by the executable candidate SQL and writer. Original GOAL and issue acceptance criteria remain unchanged. No issue is closed by this prototype.

## One implementation and one future activation decision

`crates/core/src/db/candidate_migration.rs` owns the ordered candidate registry `[13,14]`, v13 rebuild, exact physical-schema validation, conversion, version updates and reports. `crates/core/src/db/candidate/v14.sql` owns v14 DDL and structural/trigger constraints. `candidate_authority.rs` owns the single resolved-transition writer. Tests invoke these implementations directly. They do not contain an alternate migration algorithm. Normal `schema::MIGRATIONS` is unchanged and never calls the candidate registry.

Activation requires a separate reviewed change covering deployed executable/schema inventory, all writers and installer/daemon ownership, compatible typed readers, backups and remote lineage. If #54 lands with another migration number, coordinate/reuse its rebuild rather than introduce competing DDL. No down migration is implemented.

## Candidate v13

Starting version must be exactly supported v12/13/14 and its complete `sqlite_schema` table/index/trigger SQL must match the reference constructed by the same original registry and candidate implementation. Unknown/forged/future/partial shapes fail before conversion. v13 changes only `commit_map.git_sha` nullability. Every retained row, ID and value survives. All four mapping indexes are restored. Save the original `sqlite_sequence` entry before rebuild; after explicit-ID copy/drop/rename, restore its exact value or exact absence in the same transaction. Never replace historical sequence with `MAX(id)`. All other table sequence entries remain identical.

## Candidate v14 and K01 authority

Repository plus explicitly named generation owns lineage, source identity, baseline identity, projection JSON/version/hash, direction-specific frontiers, typed outcomes and preserved evidence links. No active generation selector exists in this slice; no `MAX(generation)` is used. Activation remains outside this pass.

Initial frontiers require the separately proved lineage baseline. SVN→Git handles the baseline SVN revision and records its baseline Git SHA. Git→SVN handles the imported Git baseline with **NULL emitted SVN revision**: an SVN import does not invent an outbound effect. Initial authority is `baseline`, with NULL evidence allowed only at this exact insert. An existing frontier cannot be replaced or deleted to reset authority.

Every later transition is `outcome`, with non-NULL evidence. Composite FKs bind repository, generation, direction, source key, outcome ID and policy/projection to the frontier. Triggers require a resolved outcome (`applied_verified`, `filtered_no_target`, `empty_no_target`, `semantic_no_delta`), exact previous source key and exact emitted targets. The transactional writer explicitly names the generation, rejects stale predecessors and unresolved statuses, and inserts outcome plus frontier update atomically. Pending, unknown, reconciliation, wrong owner/generation/direction/source/policy, missing and arbitrary NULL evidence are rejected. Cited outcomes and lineages cannot be edited/replaced. These constraints validate ownership of supplied evidence; they do not implement #64 external-effect qualification or recovery.

## K03 nullable-constraint audit

Copy ancestry is exactly `(path IS NULL AND rev IS NULL)` OR `(path IS NOT NULL AND rev IS NOT NULL AND rev > 0)`. Empty path with a positive revision remains valid SVN-root representation. Every introduced directional conditional explicitly requires its mandatory revision/SHA non-NULL, positive revisions and valid SHA shape; applied outcomes explicitly require an opposite-direction target. No-target outcomes require both targets NULL. Baseline/outcome authority explicitly pairs NULL/non-NULL evidence with its type. Optional target fields may be NULL deliberately; FK ownership fields are NOT NULL. None relies on a NULL CHECK result to require evidence.

## Qualification and read-safe conversion

Copy admission seals a quiesced v12 original and an independent temporary copy, rejects overlap/symlinks/special files/source journal sidecars, and compares all non-DB file hashes/modes. Only the copied DB is opened for writes. Source SQLite reads use immutable read-only connections; the source manifest is checked before and after every success/failure. A crashed copy's own journals may be recovered by SQLite; source journals are never dropped or recovered.

The only automatic proof path in this prototype admits the pinned original **complete two-revision trunk import**, unrestricted regular-file projection, matching repository and scoped Git cursor, complete scoped incoming applied records and uniquely corroborating retained mapping rows for each actual import commit. The pinned original importer may skip empty r1; qualification verifies an empty/property-free SVN r1 and a genuinely empty bootstrap, then requires retained records/mappings for every actual linear imported Git object. A surviving r1 import object without its row is pruning and is rejected. It verifies disposable enrolled local SVN UUID/root/path/revision/creation ancestry and absence of properties/copies, bridge and bare Git ref identities, and complete SVN-export versus Git-tree file hashes/types. SVN XML attribute order is canonicalized without dropping values. The importer process exits before any copy is sealed. Inventory's `qualified_fixture_shape` label grants no authority. More complex ancestry/projections/history require separate proof and remain unqualified.

Only such proved pairs receive generation 1 and two initial frontiers. Legacy scoped row IDs are linked without reinterpreting their historical outcomes. Ownerless mappings remain unchanged and unclaimed. Other repositories keep all legacy bytes and `not_qualified`, `needs_reconciliation` or `external_effect_unknown` disposition, with no canonical frontiers. Explicit read-safe disposition cannot grant qualification. Migration never changes enabled state, parents, secret ownership, configuration, checkpoints, receipts, policy, Git refs, or SVN content.

## Transactions, restart and idempotence

For each version, check physical shape and foreign-key enforcement before `BEGIN IMMEDIATE`; repeat validation inside the checked transaction. Execute conversion, compare every legacy SQL value and exact sequence, validate indexes/schema/FKs/integrity and canonical plan, update `user_version` inside that transaction, then commit. Any failure returns an error and rolls back that version. A committed v13 followed by failed v14 remains **valid v13**, not a claimed 12→14 rollback. Reopen/retry validates shape and every sealed legacy value before proceeding. Repeated v14 validates exact canonical contents and proof/dispositions and performs no write. A changed plan cannot silently overwrite prior authority.

Qualification covers injected errors and abrupt child-process exits at pre-transaction, create/copy/replace, validation and pre/post-version boundaries, plus v14 partial repository conversion; actual SQLite query-only/read-only, FK-off and NOT NULL failures; forged physical schemas/future versions; and v13-complete/v14-failed restart. Logical preservation is always checked; handled-error v13 failures additionally preserve exact DB bytes.

## #54 reader audit and activation requirements

The fixture-only `lookup_mapping(repo,generation,rev)` returns distinct `Mapped`, `ProvedNoTarget`, `LegacyUnresolvedNull`, `LegacyOwnerless` and `Missing` results. Proved no-target requires an explicitly linked mapping plus a matching generation-owned, policy-owned resolved source outcome. NULL/ownerless records never infer filtering. Ambiguous scoped rows are an error. The `MAPPING_NULL` case verifies the distinctions and wrong-generation refusal.

| Existing operational reader/writer | Observed v12 behavior; requirement before activation |
| --- | --- |
| `get_git_sha_for_svn_rev` | Reads non-NULL String, and can error on NULL. Replace global optional lookup with the typed scoped result; NULL is not "missing" or proof. |
| `CommitMapEntry` / `list_commit_map` | String field and row conversion reject NULL. Candidate activation needs nullable display data plus typed status; preserve every ID. |
| Web `sync_history::CommitMapEntryView`, scoped/raw SQL and unscoped list | Same String contract. Coordinate nullable UI/API representation and explicit outcome status. |
| `get_last_git_hash` and unscoped `SyncEngine` fallback | Latest mapping reads String. Skip NULL for an emitted hash, but never infer handled authority from global maximum/latest row. Replace operational frontier reads with explicit owned direction/generation. |
| `is_svn_rev_synced` / `is_git_sha_synced` | Existence/SHA predicates are legacy compatibility queries, not typed resolved authority. Do not authorize replay decisions from nullable row existence. |
| `get_last_svn_rev` fallback | Global MAX is not canonical handled source. Keep outside activation until a scoped qualified adapter is reviewed. |
| Existing mapping inserts / conflict and sync-record readers | Mapping inserts require SHA; separate sync-record/conflict SHA fields already optional. Future typed no-target writes must atomically own outcomes/frontiers and explicitly link nullable rows. |

These operational readers are intentionally unchanged in this copy-only pass. No candidate database is admitted to a running scheduler. Reader activation tests, fresh operational null cases, installer/daemon activation and deployed compatibility are remaining production gates, not evidence claimed by this prototype.

## Exact legacy classification-to-row proposal

| Inventoried shape | Proposed v14 disposition and conversion |
| --- | --- |
| Pinned old single import: equal scoped Git copies, scoped applied SVN→Git row, matching SVN cursor, verified SVN UUID/path/source ancestry, remote Git ref ancestry and policy | Assign generation 1 **only after those remote proofs**; copy observed handled/emitted frontiers and link original row IDs. No tip change or replay. The copy-only proof path now verifies the enrolled pinned two-revision local fixture; inventory alone still does not satisfy proof. |
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


## Retention and #64 recovery contract

Keep applied legacy rows, imported baselines, no-target receipt bytes, lineage, resolved outcomes/frontiers and evidence links until a separately reviewed retention policy proves that removing them cannot erase pending-work boundaries or historical policy. Only non-applied diagnostics may expire under the current policy. Storage growth is accepted meanwhile.

Before external writes, a consistent pre-upgrade snapshot plus old executable may be a rollback option. After SVN/Git publication, that snapshot is stale: stop writers, retain evidence, inspect actual pinned target identity/ref ancestry/tree/revision and reconcile/roll forward from durable intent. Reimport, checkpoint reset, blind retry and database rollback cannot substitute for external-effect recovery. General #64 intent/publication/recovery service is design only here. A future down migration must refuse any lossy typed/NULL/effect state.

**Next gate:** independent review of the copy-only implementation and downloadable evidence. No merge, deployment or startup activation is authorized by a passing fixture run.
