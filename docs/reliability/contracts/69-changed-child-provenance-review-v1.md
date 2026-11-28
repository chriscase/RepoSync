# Changed-child provenance review v1: stored evidence only

Refs #69 AC2 and the later authenticated preview/resolution gates. This M2a
prerequisite complements [M1 object inspection](69-changed-child-review-v1.md).
It does not change either M1 or the
[existing execution contract](69-pair-refresh-execution-v2.md). #69 stays open;
AC7/re-anchor remains NOT IMPLEMENTED.

## Concrete interface

`Database::capture_changed_child_provenance_review(expected, historical_id)`
accepts an opaque, previously captured `LiveExecutionConfiguration` and one
explicit historical operation UUID. It reads and returns an opaque
`ChangedChildProvenanceReview`. The latter has private fields and no public
constructor, `Clone`, `Serialize` or `Deserialize` implementation. Getters expose
stored operation/context facts for review; there is no conversion into baseline,
resource, actor, journal-admission or publication authority.

The method's word **authenticated** applies to existing stored version-3 history,
not to a new request's current actor or current remote state. This is a DB read
seam, not a new HTTP/CLI handler. A current administrator session, authenticated
resolution and execution admission remain separate gates.

## One read transaction

The implementation uses the existing configuration capture on the same SQLite
connection/transaction as the complete `journal()` validation. It requires the
fresh current configuration pin to equal `expected.pin()`, including both
registration UUIDs, configuration digests, echo counters, column/scoped Git/SVN
checkpoint copies, transport/endpoints/branches and current credential context.
Missing/revoked/corrupt current credential chains fail through the existing
resolver. No key is created and no historical credential is adopted.

The complete existing journal reader validates all recognized history, including
document identities, canonical authenticated authority, MACs, native database
binding, key/schema consistency, receipt shapes/provenance, duplicate ownership,
orphan pointers and holds. A direct deserialization of a selected document is
insufficient. No key/schema/document/registration/checkpoint/mapping/hold repair
or mutation is performed by this capture method.

The named operation must exist exactly once as a version-3 completed operation,
with both observed receipts and no uncertainty diagnostic. Both historical
registration incarnations, configuration/echo identities and transport/endpoints
must still match the current registrations. Existing refresh holds, competing
journals, ordinary writer windows, managed removal, history blocks and relevant
conflicts continue to refuse through the existing read guards.

Current checkpoint divergence from historical receipt checkpoints is allowed
as **unproven current metadata**, rather than being collapsed or labeled corrupt.
Both current checkpoint copies must nevertheless agree internally. The current
credential context is independently resolved; valid rotation is not required to
equal the historical context. Missing/revoked credentials cannot be revived by
the historical signed document.

No timestamp, equal tree, commit-map message, inherited human-ID cursor or
merge-base chooses the historical record. The caller UUID is only an exact
document selector. Distinct valid historical operation IDs may be inspected
explicitly; none is thereby declared the unique applicable baseline. Ambiguous
document/receipt ownership and malformed global history refuse through the
unchanged journal validator, even if the supplied selector names another row.

## Separate anchors and bounded identity

The snapshot retains:

- The unchanged authenticated historical operation and its exact raw document.
- The historical parent anchor `P1` (its old Git tip and SVN checkpoint).
- The historical integrated-child anchor `R` (prepared Git commit and observed
  child SVN receipt revision).
- The distinct current child/parent registration/context pins, which may name
  later checkpoints `C` and `P2` without proving their synchronization.
- SHA256 of the original authority bytes, and an unkeyed snapshot identity over
  `("changed_child_provenance_review_v1", selected UUID, current pin, raw
  historical document, authority digest)` using the existing JSON digest helper.

These identities bind the captured storage facts. They are not an authenticated
execution approval and do not establish freshness after the read transaction.
The capture reads native DB file identity through existing journal verification;
it does not probe SCM processes, network services or live source trees.

This preserves the important graph distinction: `R = merge(oldChild,P1)` and
`C` may descend from `R`, while `P2` normally descends from `P1`, not `R`.
Neither historical anchor may silently replace the other as M1's single linear
baseline. This method neither invokes M1 nor relaxes its graph rules.

Every snapshot unconditionally reports `execution_eligible() == false`, plus
unverified baseline applicability, live graphs/freshness, current mappings,
SVN tree/property/mergeinfo correspondence, current actor authorization,
authenticated resolution freshness and exclusive changed-child admission.
An explicit operation UUID or a caller-supplied digest cannot clear those gates.

## Source regressions and effects

One appended isolated test module reuses the existing authenticated fixture and
the actual production preparation/approval/completion pipeline to obtain a valid
version-3 completed operation. It does not manufacture a valid MAC or bypass
production validation for the positive case. No new provider, native OS adapter,
HTTP/TLS server implementation or shared fixture file is added.

That existing setup has real disposable Git/SVN/HTTPS/TLS subprocess and local
service effects, already present in the original fixture; it is not a new
SQL-only setup. Publisher qualification must account for those existing effects.
The capture assertions themselves perform storage reads only. No fixture or
capture is executed on the Mac during source authoring.

The three authored regressions cover separate historical/current checkpoints
while the old strict baseline reader still refuses; exact historical raw bytes
and deterministic snapshot identity; stale expected context; missing/corrupt
documents, authority, MAC, key or key schema; missing receipt; inconsistent scoped
copies; wrong incarnation/configuration/echo; current revocation and quiet-history
guards; ordinary contention; alias/ambiguous document identity; copied storage;
and missing or held history without key establishment or repair.

Current-checkpoint advances in the storage test are deliberately unproven values,
not a fabricated claim of live Git/SVN synchronization. Full logical DB contents,
schema and fixture HTTP log are compared through both existing DB connections
around capture. The held-operation control uses the existing journal-state
comparison excluding the independent writer-lease heartbeat; that exclusion is
not represented as a whole-DB byte comparison. Existing signed documents and
original test/source prefixes remain exact.

All three new source tests are AUTHORED, UNRUN. Source names are not a compiled
inventory or a PASS receipt. Formatter/compiler/lint, test and native execution
are not performed by this author. Independent source review precedes publisher
exact-tree qualification. Completed M1/old fixture tests are not rerun merely for
status or relabeled as evidence for M2a.

## Remaining work

Role-specific live anchor/graph policy, independently verified current mappings,
fresh complete Git/SVN correspondence, authenticated resolution-to-execution,
versioned merged-result journal validation and durable side-effect/recovery
integration remain separately scoped work. Existing clean-child execution,
approval, receipt and recovery readers/writers are unchanged. This prerequisite
does not enable changed-child publication, normal-sync resumption or re-anchor.
