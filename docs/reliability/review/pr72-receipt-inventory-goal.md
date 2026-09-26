# RepoSync PR #72 — receipt admission and read-safe legacy inventory

## /goal

Continue `chriscase/RepoSync`, `feature/reposync-reliability`, draft PR #72 from:
`afd3cd6f63078ab97609cde732880afc34394405`.

Read posted independent Review 4 (#5308189300) on PR72 and `RepoSync-PR72-review-4.md`. H01/H02's five demonstrated cases are accepted bounded progress. Do not redo them or mark the whole epic complete. Correct I01/I02 and deliver read-safe legacy inventory plus one consolidated migration contract. No generation schema migration or general operation service in this pass.

Original base: `87379741779a6259f7eeb52a68cc6f061174e5ef`.
Previous comparison anchor: `f74fce855a1f1d80dd631397f436ba33906272e6`.
Original GOAL SHA-256: `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`.
Locked graph SHA-256: `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05`.

Copy this brief into a new versioned review file and hash it. Preserve original GOAL and earlier brief bytes. The posted review is the fallback when an attachment is unavailable.

## Safety and scope

Use the existing isolated fixture runtime and scanner. Dependency preparation remains separate. No production endpoints, credentials, active installation access, mixed old/new writers, reset/reimport recovery, force push, merge, deployment or live acceptance. Do not repair the user's Docker storage by destructive pruning/reset; use CI and state where execution occurred.

Keep implementation sequential on the same branch. Separate receipt-admission changes, inventory/tests, and design/qualification documentation into reviewable commits. Do not introduce a parallel migration or modify issue acceptance criteria.

## Part A — receipt authority must not depend on cursor-copy disagreement

Reproduce I01 before fixing it. Current policy validation runs only inside column/KV mismatch; equal copies and scoped KV-only admission do not establish that a recorded no-target outcome applies under the current policy.

Required actual-engine controls:
- A genuinely filtered nonempty Git commit handled under policy X, with both Git cursor copies equal G. Change policy to Y before any SVN publication. Prove policy reconciliation/verification occurs before affected remote writes; G/G must not bypass the policy check.
- The same change after an SVN publication splits the copies, preserving today's rejection control.
- Scoped KV-only admission where supported; otherwise explicitly reject that unqualified shape without guessing.
- Unchanged policy with ordinary subsequent work still succeeds. Keep the genuine-empty, applied, imported-baseline, pending-both-directions, restart and retention controls.
- Malformed/stale receipts do not become verified merely because their SHA equals the repository column.

Do not automatically replay all history after a policy edit. Establish a reviewed policy-compatibility rule or block the affected operation with an actionable reason. Do not simply update the receipt to the new policy or copy the remote tip into checkpoint fields.

Suggested new IDs: `R10_POLICY_EQUAL_CURSOR`, `R10_POLICY_SPLIT_CURSOR`, `R10_POLICY_KV_ONLY`. Stable equivalent IDs are fine if the manifest and report agree.

## Part B — persist no-target proof only after it is proved

Current `no_svn_delta` paths can originate from swallowed content-read errors, failed staging, unversioned-only SVN status or `NothingToCommit`. A clean working-copy status is not a target-content oracle.

Add deterministic real-engine fault cases:
- A required non-delete Git blob cannot be read/materialized.
- SVN staging of a required newly copied file fails, leaving it unversioned.
- A later outgoing Git commit is queued in the same batch.

For these failures, stop at the offending commit; no verified no-target receipt, no cursor advance over it, no later outgoing publication, and no fabricated applied mapping. Preserve earlier proven effects, report the failure and prove safe retry once. Inject faults at explicit test boundaries while exercising real selection, materialization/status and persistence paths; do not replace the whole engine with a model.

A legitimately empty Git commit needs no invented SVN revision. A genuinely filtered outcome needs the exact active policy and correct cleanup/selection. A nonempty source delta already represented in SVN needs verification of its relevant projected target state at a pinned revision. Distinguish these from operational failure. Reject unsupported or uncertain cases rather than manufacturing proof.

Audit compatibility for existing version-1 `no_svn_delta` receipts written by the previous implementation. They are not automatically migration-ready simply because JSON parses. Keep them unchanged and classify/revalidate as appropriate. A small versioned current-schema receipt adjustment is permissible with a precise contract; no broad job service or DDL.

Suggested IDs: `R01_NO_TARGET_READ_FAILURE`, `R01_NO_TARGET_STAGE_FAILURE`, `R01_VERIFIED_NO_DELTA`, with retry and queued-successor assertions.

## Part C — read-safe inventory of legacy state

Provide one documented local command that inventories a **consistent fixture copy**, producing a sanitized report. It is an inspection command, not startup, sync, migration, or automatic repair.

It must:
- Open SQLite in a mode that cannot mutate input; do not invoke migrations/initialization/maintenance, materialize baseline receipts, acquire operational work or update access timestamps in the database.
- Refuse unsupported schema or inconsistent/incomplete backup input, rather than guessing or creating a new DB. Do not discard a WAL or treat `immutable` as a substitute for a consistent snapshot.
- Read configuration and the repository table and report IDs, enabled/disabled status, hierarchy, SVN/Git endpoint identity references, policy, each directional cursor source, relevant mapping/receipt existence and proposed classification.
- Redact credentials. Report their presence/ownership/inheritance references, not secret values. Any digest scheme for sensitive low-entropy values must not be exposed as a substitute for redaction.
- Distinguish `qualified_fixture_shape`, `needs_reconciliation`, `external_effect_unknown` and `not_qualified`; do not equate a report classification with production eligibility.
- Make zero Git/SVN network calls or writes. Use only supplied local fixture evidence; missing remote identity proof is UNKNOWN. Local Git inspection must not refresh/write the index.
- Preserve input file/DB/WAL/config/ref content hashes and applicable permissions. Repeated runs must produce the same substantive report.
- Never resolve multi-repository ownership from a global maximum, row order or equal revision number.

Reuse the pinned old-production driver. Extend it for a bounded additional topology: two independent imported repositories, one disabled. Overlapping SVN revision numbers should not cross-match mappings or credentials. Retained/pruned/missing-receipt variants may be derived with explicit synthetic fault overlays; label those separately from unchanged-old-code output.

Do not substitute fresh candidate-created tables for old-code qualification. Do not claim the current engine fixture proves the daemon/installer reads preserved configuration correctly; list that as a separate pending acceptance path.

## Part D — one current #54/#63/#64 contract

Consolidate the existing design rather than appending a competing set of rules:
- Remove or clearly archive the superseded first-retained-SVN-row fallback.
- Define permanent `(repository_id, generation)` ownership, source identity/copy lineage, baseline proof, handled/emitted frontiers and policy identity.
- Specify no-target verification and invalidation consistently for equal, split and missing cursor copies.
- Separate permanent authority from expiring diagnostics, including the temporary growth consequence of preserving all applied mappings.
- Map observed inventory shapes to justified automatic qualification, explicit reconciliation or unknown external effects.
- Reconcile #54's nullable outcome need with the actual embedded migration runner and any future table rebuild. Explain how existing/new data survive and why an old-DB restore after new external writes is not general rollback.
- Keep a concrete #64 post-publication/lost-reply-or-checkpoint example as design only.

No new migration tables, DDL application, canonical conversion or recovery service implementation. The next independent review authorizes that separately.

## Validation and economical handoff

Retain all 41 current required cases and their assertions. Do not relabel observed defects as acceptance. Add required cases so missing/ignored/failed executions cannot pass. Run original-base and previous-head comparisons with the same locked graph; retain existing failures and identify newly changed results. Keep scanner gates and downloadable sanitized artifacts.

Run targeted tests while iterating, then the full required suite/comparisons at the final functional head and confirm final documentation-head tree/check evidence. Reuse safe caches. Do not repeatedly run expensive unchanged qualification merely to produce another status report.

Return:
```
STATUS / PR / BRANCH:
STARTING / FUNCTIONAL / FINAL / CI HEADS AND TREES:
ORIGINAL GOAL / THIS BRIEF / LOCK HASHES:
I01 POLICY-EQUALITY / SPLIT / KV-ONLY:
I02 EMPTY / FILTERED / VERIFIED-NO-DELTA / FAULT / RETRY:
PINNED OLD FIXTURE PROVENANCE AND TOPOLOGY:
READ-SAFE INVENTORY COMMAND / CLASSIFICATIONS / INPUT IMMUTABILITY:
41 RETAINED CASES + NEW CASES / COMPARISON DELTAS:
SCANNER RESULTS / ARTIFACT URL / EXPIRY / RECOMPUTED HASHES:
DESIGN CHANGES / #54 COORDINATION / DDL: NONE:
RETAINED F/G/H LIMITS / ACTUAL DEPLOYED VERSION: NOT ESTABLISHED:
ONE PROPOSED NEXT SLICE:
```
Use compact deltas and durable artifact references rather than restating all prior manifests. Stop at the review gate. Keep PR72 draft.
