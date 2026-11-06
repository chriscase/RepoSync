# Issue 69: authenticated changed-child receipt candidate closure v1

This M2c extension is inspection-only. It follows M2a's authenticated stored-history
capture and M2b's registered-source preview. It does not change either existing API,
any clean-child preparation/execution reader or writer, or any production caller.
`execution_eligible()` remains literal `false` at all three new review surfaces.
Re-anchor / AC7 remains **NOT_IMPLEMENTED**.

## Exact scope and ownership

Base epic commit `5eba5b7c41bc06eb62053da3991b375208164bce`, tree
`995528a615d12d2ee3c00afa5d0598ce34ea3ce0`. Source author
`01a1434e-b4d8-7583-969f-75ad62ef10ce`; sole composition/application/publication
owner `01a145a6-69e9-7427-ba8c-26b2c39200fb`. The coordinator binds three EOF
extensions and this new contract. Entire original source prefixes are immutable:

| Path under `crates/core/src/` | Prefix bytes | Before Git blob |
| --- | ---: | --- |
| `db/pair_refresh_operations.rs` | 70335 | `737e5fb87ac6223824044bfd08dc9eac1ea0fd7b` |
| `pair_refresh/live_preparation.rs` | 27673 | `ffd03a8903ae85f50afd68ab183b43d0be55ccbf` |
| `pair_refresh/authenticated_enrollment_tests.rs` | 181426 | `d1ca4babdfa58be8af9da7f65c665a2cc3440887` |

No schema, storage key, policy version, dependency, feature, workflow, process/SCM
implementation, hold/mutation rule, or earlier assertion changes. Other source
paths and modes, staged/user work, main and PR216 are outside this delta.

## Stored-candidate producer and supported subset

`Database::capture_changed_child_baseline_candidate_review(expected)` accepts only
an opaque current `LiveExecutionConfiguration`, not a historical operation ID or a
caller-authenticated document. In one read transaction it:

1. Recaptures current configuration and independently resolved current credentials;
   exact pin mismatch refuses.
2. Calls the existing complete `journal()` reader. Every document, canonical
   encoding, supported journal version, authority MAC, native DB/key binding,
   request/hold ownership, receipt consistency, historical baseline chain and
   signing-key history must validate. Corrupt/unknown/orphan authority anywhere is
   refused before filtering, including otherwise unrelated/ineligible entries.
3. Inspects all returned operations. An eligible candidate is a version-3 completed
   operation with no reconciliation detail, exact child AND parent repository IDs,
   scope UUIDs, registration configuration digests and echo generations; exact
   registered Git remote/child branch/transport policy and SVN child/parent URLs.
   Parent branch and SVN configuration are also bound by the registration digests.
   Stored supported-policy/receipt validation and current source policy remain the
   existing implementations. Legacy v2, cancelled/pre-intent and valid old
   incarnations/configurations do not become completed authenticated candidates.
4. Requires **exactly one** eligible receipt. Zero refuses. Two or more refuse,
   including a legitimate chain of completed refreshes, different timestamps or
   UUID order, equal tree contents, or one newer/reachable candidate. There is no
   recency, revision, merge-base, equal-tree, ancestry or maximum-ID tie-break.
5. Reuses existing durable refresh hold, both registrations' competing-journal and
   ordinary-writer guards, personal/global journal guards and conflict checks.
   Capture never resolves/reconciles/repairs or clears them. A refused zero/multiple
   capture need not pass later guards: it already returns no proof.
6. Reads the original selected v3 document and authority in that same transaction,
   keeps exact document bytes and SHA256 identity of original authority, and
   privately constructs `ChangedChildBaselineCandidateReview` owning an M2a review.
   The selected review uses the unchanged M2a snapshot digest formula.

Current credentials are resolved independently, never compared as historical
secrets. Valid credential rotation can yield a new current context/digest while
preserving the original signed receipt/document/authority. It does not revive old
credentials or prove access to a remote with the rotated secret.

`candidate_set_digest` is unkeyed SHA256 of deterministic JSON:
`("changed_child_baseline_candidate_set_v1", current_pin, candidate_count,
selected_operation_id, exact_document, authority_sha256)`.
Success has count exactly one; no ordering ambiguity exists. This binds the entire
eligible set in the supported singleton subset. It is NOT a hash of every
unrelated/ineligible document: those must authenticate on each capture, but a valid
unrelated receipt change need not alter this digest. A newly eligible second
receipt refuses; no potentially matching receipt can be hidden by a caller UUID.
These digests identify captured evidence; they are neither signatures nor approval.

## Selector-free registered-source consumer

`capture_registered_changed_child_baseline_candidate_context(db, child_id)` takes
no historical UUID, candidate list, Git object repository, source snapshot,
lineage assertion or expected digest. Its private composition seam allows only the
existing test CA/client and one synchronous after-sources test callback.

The factory captures configuration and the opaque singleton closure, delegates
actual registered Git/SVN observation and P1/R/C/P2 checks to the unchanged M2b
seam using the producer-selected ID, and requires the returned preview's current
pin and authenticated selected-history snapshot to match the initial capture.
After M2b's repeated remote observations/final stored recapture, M2c independently
recaptures the current configuration AND complete eligible candidate closure.
Current-pin or candidate-set-digest mismatch refuses; late zero/ambiguity or damaged
authority also refuses. A callback cannot provide a proof or bypass any validator.

The new opaque, non-Clone/non-(De)serializable
`LiveChangedChildBaselineCandidateContext` owns both the singleton review and the
actual M2b preview resources. Safe read-only metadata accessors do not expose the
private repository, credentials or working copies. Its context identity is unkeyed
SHA256 of deterministic JSON:
`("changed_child_baseline_candidate_context_v1", candidate_set_digest,
preview_context_digest, final_current_pin)`.
The preview digest already binds M2b's exact original receipt identity, observed
source pins, role-specific objects/trees and lineage. P1 and R remain distinct;
current C/P2 are never silently substituted as a shared baseline.

The newly verified fact is only `single_authenticated_receipt_candidate`, alongside
registered-preview and final-set recapture facts. Exactly one stored candidate is
NOT proof of its semantic applicability or current mapping/content correspondence.
The result delegates M2b's unverified gates unchanged:

- Unique applicable historical baseline (not merely one supported stored receipt).
- Current mapping provenance.
- Full SVN visibility/content/properties/mergeinfo correspondence.
- Current actor/session authorization and authenticated resolution freshness.
- Exclusive changed-child execution admission.
- Atomic remote freshness after return.

No M1 merged result, authenticated resolution, approval DTO, job, checkout result,
prepared merge/result commit or execution capability is constructed. No generation,
TTL, admission default or re-anchor policy is introduced.

## Effects and race limits

The DB reader performs transaction-local reads only. It must not establish/rotate a
signing key, adopt legacy state, create a journal, alter schema, update a checkpoint
or repair an invalid row. The live wrapper inherits M2b's configured read traffic
and private observation repositories/auth/client scratch, plus DB/native-identity
reads. It makes no shared ref/index/worktree/FETCH_HEAD update, publication,
checkpoint/journal mutation, service launch or production caller activation.

Separate read transactions/remote observations are not a distributed transaction.
A valid change entirely outside the observation interval or an ABA change can be
unseen; a remote can change after return. The API does not claim a lease over
remotes, complete history uniqueness or future freshness. Existing M2b blocking
observation/cancellation semantics are unchanged; no new cleanup/lifetime or
finite-wall-clock guarantee is minted.

## Five authored regressions and execution gate

Module prefix:
`pair_refresh::live_preparation::authenticated_enrollment_tests::changed_child_baseline_candidate_closure::`

1. `single_authenticated_candidate_is_stable_read_only_and_never_execution`:
   genuine production-completed receipt, real independent C/P2 advances, stable
   repeated identities, preserved P1/R roles and execution refusals; two DB
   connection snapshots, shared target bytes/refs and parent worktree unchanged;
   read traffic only; normal private-scratch cleanup. Stored valid current token
   rotation preserves original receipt/authority and changes the capture identity.
2. `missing_or_ineligible_history_cannot_supply_a_candidate`: no history does not
   create a key; actual owned Prepared and CancelledBeforePublication receipts
   cannot satisfy Completed; fresh echo/incarnation/branch mismatches refuse;
   original completed history remains valid after fixture restoration.
3. `multiple_genuine_receipts_refuse_without_recency_or_equal_tree_selection`:
   two actual production completions, with a real parent advance-and-content-return
   and distinct Git/SVN receipt identities but equal Git trees. Both original
   documents/authorities/requests remain unchanged. DB and selector-free live
   capture refuse ambiguity before new source requests; no newest/equal-tree pick.
4. `candidate_or_registration_drift_invalidates_final_recapture`: control initial
   singleton visibility by withholding exact rows of an already genuine second
   completed receipt, then restore those exact original bytes/timestamps only at
   the post-source boundary. Journal validation remains real. The old selected
   receipt remains valid but final candidate closure refuses ambiguity. Also inject
   a late current echo change; injected DB evidence/shared sources are preserved.
   Withholding/restoration is test setup/fault injection, never a production API.
5. `corrupt_or_held_authority_refuses_without_repair`: absent/corrupt authority,
   corrupt document, unknown journal version, incompatible import hold, missing
   signing key and actual same-repository ordinary-writer window refuse. Evidence
   and storage through both connections remain intact; existing independent lease
   heartbeat is excluded only for the held-window subcase.

Tests reuse the existing disposable production fixture (TLS loopback HTTP Git
server, local SVN repository, actor/workspace/busy/fence and completion paths).
Positive signatures/receipts are produced by production completion, not forged test
MACs. No new fixture path or OS adapter is installed. Test setup deliberately makes
real disposable Git/SVN commits and controlled DB fixture mutations; capture's
read-only assertions do not claim setup itself is read-only.

**All five are AUTHORED / NOT_RUN.** No compiler, formatter, lint, inventory or
runtime validation has run on this Mac. Stdlib byte/hash/diff/full-tree/prefix and
source signature/call-site inspection are data checks, not typechecking. Independent
exact-delta source review is next. The sole publisher must bind the exact successor,
compiled nonignored five-case inventory, normal CI profile and fresh finite
resource/monitor/deadline/cancellation admission before qualified hosted execution.
The terminal M2b campaign and its cancellation permission do not authorize M2c.
Separate full AcceptanceLab/issue62 activation remains held. Existing M2b and prior
PASS receipts are preserved and do not transfer to these five new cases. Whole
issue69, changed-child execution and AC7 are not closed by this milestone.
