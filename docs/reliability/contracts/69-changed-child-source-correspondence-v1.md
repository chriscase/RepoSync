# Issue 69: changed-child source correspondence review v1

This is M2d, a read-only prerequisite for the default refresh. It adds current
mapping and complete source correspondence to M2c's singleton authenticated
receipt context. It does not enable execution, wire a production caller or turn
an unkeyed review identity into an approved result. Re-anchor / AC7 remains
**NOT_IMPLEMENTED**. Whole issue69 remains open.

## Source and ownership

Base epic `0d9f6ac212a8dcb9f14cd1389fde2d595769e123`, tree
`5b67353976ac9a4ae002e9b2ae8aa1918a763918`. Author
`01a1434e-b4d8-7583-969f-75ad62ef10ce`; sole composer/application/publisher
`01a145a6-69e9-7427-ba8c-26b2c39200fb`. Independent review is separate.
The coordinator binds only these EOF additions and this new contract:

| Path under `crates/core/src/` | Immutable prefix | Before blob |
| --- | ---: | --- |
| `db/pair_refresh_operations.rs` | 76221 bytes / 1896 lines | `358aa54bc2bf6fbb18cc2274ab631818bd5ee713` |
| `pair_refresh/live_preparation.rs` | 31280 bytes / 858 lines | `8aa959502671a86cc5e75ced25a0e59201bc14d9` |
| `pair_refresh/authenticated_enrollment_tests.rs` | 211349 bytes / 5710 lines | `570a6309ac0a6e707ce562bb9464453c9b971e7d` |

No old source bytes, assertions, modes, schema, keys, dependencies, features,
workflow, SCM/OS helper, credential policy, writer or execution behavior changes.
All other owners, user work, main and PR216 are preserved.

## Two independent evidence classes

The canonical issue accepts pair-scoped applied correspondence as stored mapping
provenance. That is distinct from M2c's MAC-authenticated historical refresh
receipt. A new mapping snapshot must never label legacy `sync_records` rows as
MAC-authenticated, prove who originally wrote them, or infer their age from a
timestamp. Generic raw SQL remains a low-level interface, not an approved write
capability. Fresh content/lineage checks are additional conditions.

`capture_changed_child_mapping_review(expected, candidate)` accepts only opaque
current configuration and an opaque M2c candidate. One SQLite read transaction:

1. Recaptures current configuration/credentials and requires exact equality to
   both expected configuration and the candidate's current pin.
2. Runs the complete existing journal validator before inspecting candidate
   eligibility. Canonical documents, authority MAC/native DB/key identity,
   request/hold ownership, receipt consistency and historical provenance stay
   mandatory. The same finite M2c eligibility predicate must yield the same one
   completed v3 operation; original document/authority and candidate digest must
   still match. No timestamp, latest ID or equal-tree tie-break.
3. Rechecks durable refresh, ordinary-writer, other-journal, personal/global and
   conflict guards. No hold/key/document/checkpoint is established or repaired.
4. Reads all original `sync_records` columns for each current role, ordered by
   record ID, where the row is scoped to that repository or unscoped and either
   its Git or SVN coordinate touches the registered current coordinate.
5. Requires a nonempty set of exact owned `(git_sha, svn_rev)` pairs, recognized
   `svn_to_git` or `git_to_svn` direction, `applied` status, nonempty ID and
   parseable original RFC3339 timestamp/synced-at fields. Every matching row must
   satisfy these conditions. Unscoped, partial, pending/failed, malformed or
   contradictory colliding rows refuse. A foreign repository cannot supply a
   missing owned row. Multiple exact rows may agree; every original row remains
   bound, with no recency selection.

There is no `commit_map`/column-only/global cursor fallback and no synthetic
import completion. Completed-import-only fallback remains unsupported here.
Current registration UUID/echo/configuration and the native authenticated receipt
are independently bound; rows themselves have no newly invented UUID column or
MAC. Recreated registration cannot reuse the old candidate.

`ChangedChildMappingReview` is privately constructed, non-deserializable and
non-Clone. It retains both original ordered row sets, current configuration,
candidate digest and a domain-separated snapshot digest over deterministic JSON:
`("changed_child_mapping_review_v1", current_pin, candidate_digest,
child_rows, parent_rows)`. Getters expose safe metadata/counts/digest only.
`execution_eligible()` is literal false.

## Four complete SVN role observations

`capture_registered_changed_child_source_correspondence(db, child_id)` accepts
no historical UUID, mappings, repository, properties, root mergeinfo or expected
digest. It captures the existing M2c context using registered observers, then
captures current mapping evidence. Existing M2c APIs/refusals are unchanged.

Using the actual private Git observation repository and its isolated registered
SVN clients, the new factory obtains four retained `VisibleSvnCheckout`s:

| Role | Git identity | SVN operative revision | Required mergeinfo |
| --- | --- | --- | --- |
| P1 | Authenticated prior parent anchor/tree | Signed historical parent revision | Absent |
| R | Exact signed integrated-child commit/tree | Signed SVN publication receipt revision | Exact approved receipt range |
| C | Observed registered current child commit/tree | Observed shared SVN HEAD | Same approved receipt range |
| P2 | Observed registered current parent commit/tree | Observed shared SVN HEAD | Absent |

P1 and R stay separate even when their tree OIDs agree. C is descended from R;
P2 is descended from P1 under unchanged M2b role checks. The result does not
substitute a merge-base or current mapping as the historical baseline.

For each role:

- Reuse `observe_subtree`: full-depth checkout without externals, read-only native
  WC31 integrity/presence/depth/location checks and repeated pegged identity.
  Sparse/excluded/incomplete/switched/external/queued/unsupported metadata refuses.
- Check UUID/root/URL/operative revision against registered source identity.
  For C/P2, require no visible node change after the role's handled checkpoint;
  P1/R use their own exact historical coordinate.
- Fetch recursive remote properties with inherited evidence and independently
  inspect local checkout properties. Reuse the attested empty-root exception only
  when complete WC31 metadata proves it. Compare all meaningful properties; never
  use an omitted XML root or readable listing as proof of an empty property set.
- Call existing `verify_refresh_svn_tree` against the exact role Git tree: full
  file bytes, executable property/mode, necessary directory paths, fixed binary
  MIME policy and exact supported root mergeinfo. Blobs remain opaque. Unknown or
  inherited properties, SVN-only empty directories, other file kinds or competing
  mergeinfo refuse. No current property value supplies its own expected value.
- Independently inspect readable property-free ancestors through the same
  isolated role credentials. Record the complete presence/ancestor/property facts.

Parent mergeinfo and additional child merge ranges remain unsupported by this
finite policy. Refusal retains them; there is no property resolution or range
synthesis. Signed receipt mergeinfo is the only expected child root range.

## Recapture, identity and lifetime

After all four roles are proved, the factory recaptures current configuration,
reobserves actual registered Git/SVN sources and requires unchanged pins. It then
recaptures configuration, the full candidate closure and mapping evidence after
those remote reads. Changed current pins, candidate identity or exact mapping
snapshot refuse. Damaged/missing/held evidence refuses rather than being repaired.

The opaque `LiveChangedChildSourceCorrespondence` owns the M2c context (including
actual private clients/Git observations), mapping capture and all four real
checkout lifetimes. Safe immutable role metadata can be serialized; callers
cannot deserialize/construct the context, mutate its resources through its API,
or supply a synthetic property/presence pin. Its identity is unkeyed SHA256 over:
`("changed_child_source_correspondence_v1", candidate_context_digest,
mapping_snapshot_digest, ordered_role_proofs, current_pin)`.
The role proof binds commit/tree, handled revision, complete presence/ancestor
pins, original supported property maps and expected root mergeinfo.
Private scratch names/timestamps/native inode numbers are not new authority.

This is correspondence at a bounded observation interval, not a distributed
transaction or future remote lease. Valid ABA/outside-window changes and drift
after return are not excluded. A copied serialized role pin/digest is not a
capability. Lower-layer M2b/M2c objects retain their original unverified lists;
only this additional outer factory establishes the new correspondence facts.

Still unverified: semantic applicability beyond the singleton subset; arbitrary
pending-work enumeration/local bridge work; authenticated resolution;
current actor/session authorization; exclusive changed-child execution; atomic
post-return remote freshness. Multiple genuine receipt chains still refuse.
The old M1 linear-history analyzer is not bypassed to accept the historical merge.
No result tree/merge, signed approval, job, publication or re-anchor is constructed.
All inherited and new execution-eligibility methods remain false.

## Regression inventory and effects

Eight ordinary nonignored Tokio tests are authored in
`changed_child_source_correspondence` under the existing authenticated-enrollment
module. They remain **UNRUN**:

1. `genuine_current_mappings_and_four_visible_roles_bind_one_review`
2. `missing_foreign_split_and_unscoped_correspondence_refuse`
3. `rewritten_equal_content_or_mismatched_roles_refuse`
4. `unhandled_git_or_svn_work_remains_explicit_and_preserved`
5. `bytes_modes_empty_directories_and_property_drift_refuse`
6. `excluded_sparse_unreadable_ancestor_or_history_refuses`
7. `mapping_candidate_sources_and_credentials_drift_refuse`
8. `review_capture_preserves_shared_storage_refs_and_worktrees`

Positive setup uses the existing disposable fixture and genuine production refresh
completion. Current C/P2 progress and applied rows then come from actual
per-repository `SyncEngine::run_sync_cycle()` calls, asserted to process one
SVN-origin change and no Git-to-SVN replay on each role. Those ordinary engines
use separate task-owned bridge clones and local transport to the same disposable
bare repository; actual correspondence observation still uses registered HTTPS
and the existing private fixture CA. This is not live-provider auth qualification.
Existing initial fixture registrations/checkpoints are seeded; this does not
claim full production initialization evidence. Positive rows are not inserted by
a test helper. Negative tampering/restoration is explicitly distinguished and
restores exact genuine rows/timestamps where applicable.

Snapshots compare complete logical DB/schema through two connections, shared
bare Git/refs and parent/child/ordinary bridge worktrees. A failed read preserves
injected evidence and pending source work. HTTP deltas require upload-pack and
exclude receive-pack. Normal-drop checks observe removal of owned snapshot/Git
scratch; they do not prove cancellation or deadline containment.

The visibility test additionally runs task-owned loopback `svnserve` through the
existing owned-process fixture. Its own new disposable repository configuration
initially grants anonymous fixture read/write; later finite authz denial faults
exercise unreadable/omitted source evidence. Host security, real service settings
and production credentials are untouched. Separate sparse/excluded WC31 metadata
faults affect only new disposable checkout databases and call the unchanged real
attestor. No OS-function substitute or shared fixture-source change is introduced.
A prerequisite may refuse before another gate; no unobserved per-layer causal
claim is made. Loopback readiness has a cooperative five-second wait; this is not
an end-to-end or kernel process-time bound. The admitted outer runner/VM remains
responsible for process/effects/resource/deadline/recovery containment.

All source regressions, formatter/typecheck/lint/build and runtime evidence are
NOT_RUN on the Mac. Source/hash/diff/token/signature checks are not Rust execution
or independent acceptance. Preserve exact source/prefix/whole-tree identities for
retained independent source/effects review. The sole publisher must separately
bind a fresh profile, compiled nonignored eight-case inventory, actual tool and
resource readiness, finite deadline and monitor/cancellation/recovery coverage
before any future hosted campaign. No M2c run grant or prior PASS transfers.
No source publication or CI is authorized by this record alone.
