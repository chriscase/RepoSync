# Changed-child review v1: object inspection only

Refs #69 AC2 and AC5. This is a separate read-only inspection policy named
`issue69_changed_child_review_v1`, schema 1. It does not change the existing
[execution-v2 contract](69-pair-refresh-execution-v2.md), enable changed-child
execution, or complete either acceptance criterion. Re-anchor/AC7 remains NOT
IMPLEMENTED. No production caller is added by this slice.

## Inputs and authority

`inspect_changed_child_review` accepts an already opened Git repository and
`ChangedChildReviewInputs`. The caller supplies explicit baseline, child and
parent commit OIDs and their exact tree OIDs. The inspector reads existing
objects only; it never selects a merge-base or substitutes an equal-looking
tree for the caller's baseline. Both tips must descend linearly from that
baseline within the existing 1000-commit parent bound, applied separately to
each side. The parent must advance. The child may equal the baseline.

The existing complete-graph guard refuses shallow repositories, grafts, replace
refs and linked worktrees. All three input trees and every intervening tree are
scanned under the existing regular-file/content restrictions. An unrelated,
nonlinear, missing or oversized history refuses, including when its final tree
looks equivalent. The inspector does not establish that the supplied baseline
is the actual registered copy/receipt baseline; it always reports that missing
authentication separately.

The context binds both existing registration incarnations, configuration
digests, echo counters, column/scoped Git and SVN checkpoint copies, Git endpoint
and branches, transport policy, SVN UUID/root/paths/revisions/observed HEAD,
baseline/lineage/property evidence references, optional actor/session/policy and
credential-context digests, and the explicit projection. Conflicting checkpoint
copies or malformed identities refuse. Evidence references are metadata, never
opened as paths or URLs. Raw credentials must not be supplied.

The sole projection is `full_tree_no_lfs_v1`: empty allow/block sets and zero
LFS threshold. This matches the current supported admission boundary rather than
extending general path projection. No generation value is invented.

All supplied context is untrusted metadata at this boundary. Even when every
reference is present, output explicitly says that baseline provenance is not
authenticated, actor/policy and credentials are not revalidated, SVN
correspondence is not verified, live sources are not revalidated, and pending
work has not been independently observed. Absent references have additional
missing-evidence markers. No caller flag can turn these into verified proofs.
Unsynchronized or unmapped work remains an execution refusal under the existing
live policy; object-side changes here are not a synchronization attestation.

## Three-way manifest

Enumerate the complete union of paths in deterministic order. Each role has an
explicit `absent`, `file { blob, mode }`, or `directory { tree }` identity. File
modes are Git `100644`/`100755` (serialized as numeric `u32` values); an empty
blob is not absence. Both baseline-to-child and baseline-to-parent path lists
are retained, including directory identity changes and identical changes made
on both sides. These lists count paths, not commits or newly unsynchronized work.

For regular paths, comparison uses the whole `(blob, mode)` tuple:

| Relation | Selected inspection result |
| --- | --- |
| Child equals parent | Common value |
| Child equals baseline | Parent value |
| Parent equals baseline | Child value |
| Otherwise | Explicit conflict; no result tree yet |

Absence participates in these comparisons. The inspector does not silently
combine one side's executable bit with the other side's bytes. Blobs are opaque,
including binaries: no decoding, line merge, conflict marker, driver, attribute
evaluation or rename inference occurs. Identical additions retain both delta
records while selecting one identical value. Distinct-path changes can combine.

Directory rows retain all role OIDs; their selected result is derived from the
resolved leaf map, not an input subtree copied over the other side. A
file/directory collision, or deletion of a baseline directory while the other
side changes its subtree, is an explicit topology refusal. This first policy
does not resolve those cases by side choice. Non-UTF8/noncanonical paths, empty
subdirectories, symlinks, submodules, LFS pointers and active attribute rules
refuse under the supported content boundary. The root may be empty.

Every conflict/refusal remains visible in the manifest. A partial/conflicted
manifest has no candidate tree OID. A fully selected leaf map has its exact
canonical Git tree OID computed in memory, using Git mode/name/NUL/raw-OID
serialization and Git directory ordering. `Oid::hash_object` hashes those bytes;
it does not write an ODB object. This identity is not a claim that the tree
exists in the source repository or corresponds to an SVN result.

## Inspection and resolution identities

The manifest digest is lowercase SHA256 of serde JSON encoding of
`("changed_child_manifest_v1", manifest_with_empty_manifest_digest)`. All
manifest fields participate: schema/policy, complete inputs/context, ordered
path triples and dispositions, both delta lists, unverified markers, optional
candidate tree and `execution_eligible=false`. Struct field order and sorted
path enumeration define the encoding; there are no caller-controlled JSON maps.
These are inspection identities, not keyed authenticated approval digests.

`resolve_changed_child_review` accepts current inputs and a strict
`ChangedChildResolution`. It first recomputes the current manifest. Schema,
policy, complete inputs and original manifest digest must match before it
interprets any choice. Every conflict needs exactly one exact path/triple and a
`base`, `child` or `parent` choice. Choosing an explicitly absent side means
absence; there is no implicit deletion fallback. Unknown fields/variants,
duplicate document fields, duplicate/missing choices, forged triples and
unknown/non-conflict paths refuse. Deserialize the typed document directly;
do not first collapse duplicate fields through a generic JSON map.

The result retains original triples and the original manifest digest, plus
choices sorted by path, resolved dispositions, complete inputs, candidate tree,
unverified markers and `execution_eligible=false`. Its SHA256 identity encodes
`("changed_child_resolution_v1", result_with_empty_resolution_digest)`.
Choice order cannot change identity; different choices remain different even
if a future policy could produce equal content. New content uploads, arbitrary
text/binary merges, and directory/property resolutions are outside this policy.

Stale refusal is relative to the supplied current immutable inputs/context. This
function does not observe remote refs, a live DB or current credentials. Later
production integration must freshly obtain trusted observations before accepting
resolution, approval, enrollment and each publication. There is no invented TTL.

## SVN and later execution dependencies

This slice invokes no SVN client and cannot prove SVN property or merge status.
Missing property evidence is never synthesized as an empty property set. Current
supported semantics remain regular bytes, executable properties, fixed
`svn:mime-type=application/octet-stream`, complete directory correspondence and
the exact approved root mergeinfo. Unsupported/inherited properties, SVN-only
empty directories, text/property/tree conflicts and competing mergeinfo remain
explicit execution refusals. A supplied property evidence hash is not proof
that these conditions were checked.

Before changed-child execution can be admitted, separately scoped work must:

- Prove the registered original-copy or completed-receipt baseline while current
  child mappings have independently advanced, without selecting a convenient
  merge-base or replaying inherited commits as new work.
- Bind authenticated review/resolution to trusted fresh source/configuration,
  actor and credential context through existing preview/digest machinery.
- Prepare an unreferenced history-preserving two-parent Git commit and a normal
  pegged SVN range merge into the actual child, applying only admitted choices.
  Independently prove both results under the selected projection, including
  bytes, modes, directories, properties and separate conflict/status evidence.
- Derive root mergeinfo only from the exact approved `(baseline,parent]` range
  and prior ranges proven by the completed receipt. Do not accept arbitrary
  caller mergeinfo.
- Version current journal/result validation, which currently equates the result
  with the parent tree, and retain durable inputs/resolution/intents/receipts
  across the existing publication and recovery boundaries. Never promise atomic
  rollback across Git, SVN and SQLite.

Existing preparation, retained-object validation, journal, actor, service,
publication, native lifecycle and HTTP code is unchanged by this slice. A
read-only resolved result cannot be passed as an execution capability.

## Source regression gate

The appended `changed_child_review` test module uses task-local Git fixtures
and no commands, network, SVN clients or live services. It covers distinct and
identical changes; exact mode/absence/binary conflicts and choices; canonical
tree identity independently constructed with libgit2 TreeBuilder; stale context
and source identities; strict wire schema and choice refusal; topology/content/
graph bounds; explicit unverified provenance; and unchanged refs, object files,
index and working-tree bytes. A combined result OID is checked against a
separate expected repository and must remain absent from the source ODB.

These new regressions are AUTHORED, UNRUN on the Mac. No formatter/compiler/
syntax/apply check or native fixture execution is implied by source inspection.
Independent source review precedes publisher-owned exact-tree qualification.
Existing 15 lifecycle tests and prior passing evidence are preserved, not rerun
or transferred as evidence for this new policy. Full AC2/AC5 and #69 remain open.
