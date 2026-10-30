# Issue 69: trusted changed-child applicability inspection v1

This factory-only milestone connects M2d's opaque source correspondence to a
pinned, read-only four-role review. It does not enable default refresh,
authenticate choices, enroll a job, change recovery, or approve execution.
`execution_eligible` remains false at every new result layer. AC7 / re-anchor
remains **NOT_IMPLEMENTED**. Issue 69 remains open.

## Exact source boundary

Coordinator base: epic `60da017231ca250aaf3523de788c916650b16e94`, tree
`4108a5d30d89d5694912d4c8d7762f0fc4eacca1`. The base commit name comes from
the exact coordinator/publisher handoff; the local complete source inventory
independently reconstructs that tree. No remote read, branch/ref movement or
integration checkout was needed for source construction. Publisher must bind
its canonical commit/parent and current admission before a new campaign.

Retained author `01a1434e-b4d8-7583-969f-75ad62ef10ce`; independent reviewer
`01a17365-b565-7108-aff7-76c12eaef022`; sole composer/application/publisher
`01a145a6-69e9-7427-ba8c-26b2c39200fb`. The current direct coordinator
assignment supplies these additive spans; no newer conflicting exact-span
claim was delivered in this task. That is not a global reservation assertion.
Only an isolated source copy is edited; original worktrees/user bytes/refs
and all other reservations are preserved.

| Path in `crates/core/src/pair_refresh/` | Exact immutable prefix | SHA256 |
| --- | ---: | --- |
| `live_preparation.rs` | 41273 bytes / 1121 lines | `c79f3adc938dd54424d014124ad9094acfe4655fefba8212d90d6d63728c627c` |
| `preparation.rs` | 33945 bytes / 895 lines | `539368b95485065fc516cd109daf7e197bec65caeb9d8cb9bc09b3df6ed0939d` |
| `authenticated_enrollment_tests.rs` | 313874 bytes / 8114 lines | `c1c596c06bfafdf02d4d2b53e89331e95b6d68e78f9e7e86da893072b483e83e` |

This new contract is the fourth changed path. All old bytes, modes, strict
DB readers, source/credential policy, ordinary-writer convergence, lifecycle,
F07/F08 spans, assertions, workflows, dependencies and production callers stay
exact. No execution-v2 or M1 policy is silently broadened.

## Finite semantic applicability

Policy `issue69_retained_merge_linear_sides_review_v1` supports exactly:

1. A unique authenticated completed receipt chosen by the unchanged M2c
   journal/candidate closure; no caller-selected UUID or latest/equal-tree rule.
2. Its exact retained two-parent merge **R**, including immutable OID, parent
   order, operation marker, author/committer/timestamp, prepared tree, and prior
   clean-child ancestry/content contract. **P1** is the signed prior parent tip,
   never R, a current equivalent tree or a computed merge-base.
3. `tree(R) == tree(P1)` is independently checked against the signed receipt
   and actual objects. Thus the full content integrated at the last successful
   refresh is exactly P1's content, while both commit identities remain distinct.
4. Current **C** follows only single-parent commits back to R. Current **P2**
   follows only single-parent commits back to P1. Each side must advance its
   commit and content. Each walk uses the existing 1000-commit bound and scans
   every intermediate tree, including rules/pointers subsequently deleted.
   Extra merges, unrelated/rewritten sides, missing objects and oversize walks
   refuse explicitly. There is no general DAG applicability claim.
5. M2d has already proved each role's exact scoped mapping, complete SVN
   visibility/content/properties/modes/mergeinfo, incarnation and current
   handled checkpoints through registered observers. Registered tips and
   visible SVN work beyond those handled points refuse. These proofs do not
   enumerate arbitrary local bridge work or arbitrary pending objects.

For this subset, R's exact tree is a justified content comparison baseline:
C's delta is measured from R; P2's delta is measured from P1; those two base
trees are equal by authenticated history and actual object checks. This is
not substitution of an arbitrary equal-content commit and does not treat
P2 as descended from R. The same merge is still rejected by M1's unmodified
linear-history inspector/resolution path. The new inspector is a separate
policy, not an M1 fallback or an executable changed-child plan.

## Read-only path review

`inspect_changed_child_applicable_graph` is a crate-only object implementation
seam; it alone establishes no live provenance. It reuses M1's canonical metadata,
path/tree safety, opaque entry identities and in-memory result-tree hashing.
It validates the exact retained merge and the two role-specific walks first.

Compare the union of baseline/C/P2 paths using exact absence, blob OID and
100644/100755 mode identities. Equal side results select that identity. If
only one side changes a leaf, preserve that side's exact delta. Differing
overlapping changes become explicit conflicts retaining all three entries.
Directory/file collisions and delete/modified-directory cases remain explicit
refused rows. The candidate tree hash is absent for any conflict/refused row.
For a fully selected result it hashes canonical Git tree bytes in memory; no
result object, commit, index, ref or worktree is written. No automatic text
merge, rename heuristic or choice interpreter is added.

Binary bodies stay opaque; selected file bytes/modes/absence are not normalized.
Existing symlink/submodule/LFS/attribute, path, empty-directory, SVN property,
projection and mergeinfo restrictions remain. There is no executable SVN result
or property-resolution recipe. Unsupported observations remain actionable
refusals rather than being dropped or turned into an execution result.

## Factory, identity, recapture and lifetime

Public `capture_registered_changed_child_applicability_review(db, child_id)`
accepts no baseline, receipt, roles, source snapshot, mappings, expected content,
choices or actor assertion. It first captures the unchanged opaque M2d object.
Private construction retains that actual object, four complete checkout lifetimes,
Git observation repository, registered clients, candidate closure and mapping
capture. Neither live object implements Clone or Deserialize.

The separate serializable manifest binds:

- Its policy/schema and the exact M2d correspondence and role-proof digests.
- Candidate-set, original historical snapshot and current mapping digests.
- Complete current configuration pin, registered Git/SVN observation pins,
  both scope/generation/configuration/checkpoint identities and credential-context
  digest (never secret credential values).
- All P1/R/C/P2 commits and trees, original prepared-merge metadata, current
  context, baseline rule, complete sorted path triples/dispositions, both side
  deltas, candidate tree or conflicts/refusals, and every unverified status.

The inner inspection digest is SHA256 of deterministic JSON
`("changed_child_applicable_graph_v1", graph_with_empty_inspection_digest)`.
The outer digest is SHA256 of deterministic JSON
`("changed_child_applicability_manifest_v1", manifest_with_empty_manifest_digest)`.
Lineage and full M2d role proof metadata are separately domain-bound; the original
opaque source remains necessary for trusted construction. These are identity
hashes, not MACs, approvals, executable capabilities or serialized authority.
Current actor policy is deliberately absent rather than borrowed from an old job.

After immutable-object inspection, repeat actual registered Git/SVN reads and
final DB configuration/candidate/mapping capture. Any changed pin or stored
closure refuses before returning the review. Policy/generation/credentials or
history loss cannot be repaired by the adapter. A private test callback only
injects faults after inspection; it supplies no observations or authority.
`require_manifest_digest` compares a pin on an already captured live review;
callers must freshly recapture before checking a prior pin. It accepts no
serialized manifest or choices. Editing JSON cannot construct the live object.
A matching digest never grants execution or proves future freshness.

This is a bounded observation interval, not atomic remote locking. Valid ABA,
mutations outside the interval and drift after return remain unexcluded.
Arbitrary local work, authenticated resolution, current actor/session authority,
exclusive durable execution, crash recovery integration and default production
wiring are later gates. Existing lower-layer unverified lists are preserved;
the new outer result adds only the explicit finite applicability evidence.

## Authored regression inventory: eight cases, all UNRUN

Module `changed_child_applicability_review`, below the complete existing test
prefix. All use the existing serial native-writer admission and task-owned fixture.

| Test leaf | Literal intended coverage |
| --- | --- |
| `genuine_retained_merge_two_advances_bind_conflict_and_pin` | Real authenticated refresh completion plus real parent/child ordinary producers, exact four roles, same-path conflict with both deltas, non-executable pin, unchanged M1 refusal, storage/history preservation |
| `opaque_selection_keeps_both_binary_mode_absence_deltas` | Object-only selection control based on genuine historical merge: disjoint binary/mode/add/delete changes preserve both deltas; canonical in-memory result hash, directory/file refused row and LFS refusal, no ODB/ref writes |
| `rewritten_roles_foreign_sides_and_extra_merges_refuse` | Object-only equal-content wrong P1/R identity, foreign same-content C and added merge refusals without object/ref mutation |
| `ambiguous_genuine_receipts_are_not_selected` | Two real production completions remain valid but ambiguous; no selected baseline or history repair |
| `late_mapping_policy_generation_and_history_drift_refuse` | Actual post-inspection boundary reached for mapping metadata, projection policy, echo generation and historical authority faults; refusal preserves injected evidence |
| `pending_git_and_svn_work_is_refused_and_preserved` | Unhandled registered Git ref and separate real SVN source progress refuse without removing source work |
| `post_inspection_property_visibility_drift_refuses` | Real later SVN property/head mutation after inspection refuses and retains property; no unobserved per-layer authz claim |
| `pin_comparison_serialized_metadata_and_resources_stay_read_only` | Stable fresh recapture, mismatched/edited pin refusal, no JSON authority, upload-pack-only reads, complete shared DB/ref/WC/bridge preservation, normal-drop owned scratch lifetime |

Positive setup copies the already qualified M2d fixture's exact production refresh
and ordinary producer sequence; current applied mappings are not fabricated by
the new helper. The copied completion helper gains an explicit request label
only so the two genuine completion recipe can retain distinct requests. The
ambiguity-only fixture reuses M2c's genuine two-completion recipe, including its
explicit fixture parent mapping/checkpoint setup; it does not claim that recipe
as a new real ordinary-producer correspondence positive. Object-only controls
never masquerade as registered correspondence or authenticated capabilities.
Negative SQL/ref/property injections and restorations are explicitly labeled.

No compiler, formatter, build, test, native tool, fixture, provider, signal or
SCM/project command ran on the Mac. Data/hash/diff/identity/text checks are not
Rust typechecking or source acceptance. Formatting is not independently verified.
A retained independent review must inspect this fixed source packet first.

## Next executable gate and effects

Sole publisher must bind the canonical base/parent/successor tree, verify the
compiled nonignored eight-case inventory and current qualified cloud profile,
then obtain current actor/resource/operator/duplicate-run/cancellation/deadline
admission before publication/CI. The old completed M2d campaign grants no clock
or cancellation binding for this new slice. No unchanged test is rerun merely
for status, and no prior PASS is transferred to these new cases.

Tests use existing local Git/SVN/Python HTTP fixture tooling and disposable
repositories/native DB/bridge clones/loopback server/private fixture CA. Genuine
setup performs real fixture refresh/ordinary publication; adapter inspection
performs registered read requests, private scratch checkouts and readonly DB
capture. It does not write provider/production state. Extra recapture reads and
four fault-fixture setups must be accounted for in the new resource/profile.
Existing cooperative subprocess behavior is not an end-to-end finite deadline;
the admitted outer runner/VM owns containment and recovery. No installs,
credentials/access/security/mount changes or new external service are admitted.

Preserve all prior failing approaches, original reports and qualified M2d receipts.
M2D-N2 remains relevant: a prerequisite refusal may prevent later checks from
being reached. Aggregate PASS never proves an unobserved per-layer cause.
