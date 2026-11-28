# Changed-child registered source and preview-context binding v1 (M2b)

Status: source milestone, not execution admission. This contract adds no caller
to the default refresh path. `execution_eligible()` is always false. AC7/re-anchor
remains NOT IMPLEMENTED. No previous M1/M2a or lifecycle result qualifies this
candidate automatically.

## Boundary and authority

`capture_registered_changed_child_preview_context(db, child_id, historical_id)`
is a separate factory appended to `live_preparation.rs`. It obtains an opaque
`LiveExecutionConfiguration` from the existing database reader and a
`ChangedChildProvenanceReview` from the existing authenticated journal validator.
The caller supplies selectors only. Neither UUID selection, equal content,
timestamps, a supplied tree/checkpoint DTO nor an M1 digest is authority.

The result, `LiveChangedChildPreviewContext`, has private fields, no public
constructor, and no Clone/Serialize/Deserialize implementation. Its safe getters
return metadata, not clients, a workspace, an approved result or an execution
pin. It retains its own observation resources and isolated credentials privately.
There is no conversion to `LivePreparedRefresh`, job enrollment, actor approval,
resolution acceptance, publication or recovery admission.

The private composition seam is not an alternate caller-facing producer. The
public factory uses the existing system-trust Git client and a no-op boundary
callback. Regressions use the existing disposable fixture CA and a callback that
injects a fault after real observations; they do not supply fake source snapshots.
Both paths use the same registered Git/SVN observers and final recapture.

## Four roles, not one invented merge base

The authenticated historical receipt supplies:

| Role | Git anchor | SVN anchor |
|---|---|---|
| P1 | historical parent checkpoint | historical parent SVN checkpoint |
| R | historical integrated child prepared/publication commit | observed historical child publication revision |
| C | currently registered and observed child tip | current child checkpoint, independently checked against current SVN history |
| P2 | currently registered and observed parent tip | current parent checkpoint, independently checked against current SVN history |

The existing signed-object validator reads R from the private observation
repository and checks its exact parents, tree, message, author, committer,
operation marker and time against the authenticated historical operation. A
missing R is an error; no replacement commit is created. The old validator and
its clean-child policy are unchanged.

The new graph checks require C == R or C descends from R, and P2 == P1 or P2
descends from P1. P1 and R keep their different identities even when their trees
are equal. The four Git trees must satisfy the existing supported-tree policy.
R's tree and P1's tree must match the historical publication tree. Equal bytes
cannot replace a role's commit identity or ancestry.

P2 need not descend from R. A normal independent parent advance from P1 has
`parent_contains_integrated_child() == false`; that is retained as an observed
graph fact, not an error or a fabricated R-to-P2 baseline. C can cross the old
integration merge R on its way back to P1. This factory never feeds an invented
single baseline into M1, calls its analyzer/resolver, relaxes its linear-history
policy or creates a changed-child merge/result.

These necessary graph facts do **not** prove that a caller-selected historical
receipt is the unique applicable/latest semantic baseline. Selection of an old
but genuine ancestor remains an explicit unverified gate. Neither checkpoint
recency nor topological reachability alone resolves competing historical meaning.

## Actual observations and SVN continuity

The existing registered Git observer downloads both exact registered branches
using their separately resolved credentials into a new private bare repository.
It checks both advertised tips, complete object ancestry and endpoint policy.
It does not update shared refs, HEAD, index, worktree or FETCH_HEAD. The new
factory reads objects from that repository and never prepares an unreferenced
result commit.

The existing SVN source observer uses both registered isolated clients. It
checks repository UUID, canonical root/role URLs, peg revisions, both complete
path histories, no unhandled branch changes after their checkpoints, and a
stable observed repository HEAD. It is not a directory-metadata substitute for
subtree history.

The new factory additionally requires observed SVN UUID/root/paths and child URL
to match authenticated history. Current child SVN revision cannot precede the
historical child receipt, and current parent SVN revision cannot precede P1.
It reuses `inspect_svn_refresh_lineage` at current child/parent pegs, using the
historical receipt revision as the child mapping anchor and P1 as the parent
baseline. The originating copy identity and parent incarnation must agree with
the signed historical proof; only the copy proof's inspected revision advances.
Replacements, unsupported copy/rename history, missing evidence or incompatible
identity/ranges refuse through the existing readers.

No current Git/SVN mapping is inferred from those registration columns. No
content, authorization-complete visibility, binary/executable-property or
mergeinfo correspondence is inferred from SVN location/history or equal Git
trees. This factory does not export/checkout/merge SVN trees or invent an empty
property set. A custom SVN property can therefore coexist with a source context
whose correspondence gate is explicitly unverified; it cannot become an
approved baseline or execution result. A later policy must obtain complete
visible tree/property/mapping proof or refuse. That work is not silently added
to an unreserved producer or reader here.

## Ordering, freshness and deterministic binding

1. Reject a nil historical selector. Capture current configuration and validated
   stored history using the existing M2a reader and quiet/hold checks.
2. Observe registered Git sources on a blocking worker and registered SVN
   sources asynchronously. Check historical objects, role ancestry and current
   SVN copy/parent continuity.
3. Repeat actual registered Git and SVN observation after those asynchronous
   reads. A remote ref move with unchanged DB rows must refuse; SVN identity,
   pegs or inspected HEAD drift must refuse. The observed pins must equal the
   earlier pins.
4. Recapture current configuration and authenticated M2a history after all
   asynchronous observations. Require the exact original current configuration
   pin and M2a snapshot digest. Credential revocation/context change, registration
   or echo generation drift, selected history/authority mutation and a new
   incompatible durable window/hold invalidate the result.
5. Hash a domain-tagged JSON tuple containing the M2a snapshot digest, final
   current configuration pin, registered Git and SVN source pins, fresh lineage
   proof and named P1/R/C/P2 commit/tree facts. All fields have fixed struct/tuple
   order. No scratch path, request timestamp or unrelated map iteration enters
   the identity. Scratch directories can differ while the same facts have the
   same digest. The M2a digest already binds exact original history/authority and
   historical/current credential contexts. Credentials themselves are not hashed
   into a newly exposed payload or serialized by this type.

The digest is an unkeyed identity of facts, not a MAC, actor signature, approval
or result pin. Final recapture is not a distributed transaction. Git and SVN
observations happen at separate times, and a remote can change after its last
observation or after return. Arbitrary ABA changes are not proved absent.
Revalidation at later preview approval/exclusive execution remains mandatory.
Existing blocking-observer cancellation and private scratch lifetimes are not
redesigned; this factory issues no lifetime/cleanup attestation for a cancelled
caller or detached blocking task.

## Verified and unverified gates

The returned context verifies only the named stored-authentication/current
capture, registered source observations, historical Git object identity,
role-specific Git ancestry, supported SVN checkpoint history and copy/parent
incarnation continuity, repeated source pins and final stored recapture.

Its unverified gates explicitly include:

- unique applicable historical baseline;
- current mapping provenance;
- authorization-complete SVN visibility/content/properties/mergeinfo
  correspondence;
- current actor/session authorization;
- fresh authenticated resolution;
- exclusive changed-child execution admission;
- atomic remote freshness after return.

No getter makes these gates true. A future caller must not interpret the type
name, source digest, nominal checkpoint or graph ancestry as an applicable
baseline/result certificate. Existing strict completed-baseline and clean-child
preparation continue to refuse independently changed child metadata/content.

## Read-only effects and qualification

Production effects are DB/native-storage identity reads plus configured Git/SVN
read traffic, private bare Git objects, existing temporary Git HTTP bridge
resources and isolated SVN authentication scratch. These are real effects;
read-only does not mean filesystem/network silence. There is no schema/key/
checkpoint/journal repair or write, SCM publication, shared index/worktree/ref
update, workspace adoption, result checkout or prepared commit. HTTP request logs
are expected to grow during live observations and must not be asserted equal.

The five authored default-lib regressions are in
`pair_refresh::live_preparation::authenticated_enrollment_tests::changed_child_preview_binding`:

1. `distinct_registered_roles_are_stable_read_only_and_never_execution`
2. `selectors_authentication_and_incarnation_refuse_before_live_reads`
3. `repeated_remote_observation_refuses_ref_and_svn_head_drift`
4. `final_recapture_refuses_stored_drift_and_new_admitted_window`
5. `equivalent_content_and_unknown_properties_cannot_mint_a_baseline`

All are authored **UNRUN**. They unconditionally use the existing authenticated
disposable fixture and real production completion to obtain a signed positive
receipt, then real isolated Git/SVN source advances. No synthetic signed positive
row, new provider, Python fixture or feature is introduced. Each uses existing
serialized writer-test admission and a two-worker Tokio runtime. Qualification
must verify nonignored inventory and actual named terminal passes for the exact
candidate, not just test registration or a skip.

The positive test compares full logical DB schema/table snapshots through both
DB connections, all shared bare-Git file bytes and the existing parent working
copy file bytes. It permits and checks read-only Git request-log growth, checks
no private refs/FETCH_HEAD, and checks normal-drop private Git scratch removal.
That is logical/file-byte preservation, not a claim about atime, physical SQLite
WAL bytes, adversarial transient mutation or crash/cancellation cleanup.

Fault controls deliberately mutate only disposable fixture refs, repository
revisions, credentials/echo/authority rows or a genuinely admitted child writer
window between initial reads and final revalidation. These are test injections,
not production factory effects. A real SVN drift revision is retained, not
rolled back. Original KV values/timestamps are restored only by fixture cleanup;
the capture must leave injected errors/holds intact. The admitted-window control
excludes the existing independent writer-lease heartbeat from its KV comparison,
as explicitly stated in its source; the positive controls compare all logical
tables without a live fence heartbeat.

No Mac compiler, formatter, tests, native/SCM command or project tool ran while
authoring. Independent source/concurrency review must precede the sole publisher's
exact source/profile/resource/cancellation admission and qualified hosted CI.
Existing default CI is the proposed qualification route; the separate issue62
acceptance-lab actor requests and unresolved campaign gates are not used or
activated by M2b. No ref, push, merge, main promotion or PR216 change occurs here.
