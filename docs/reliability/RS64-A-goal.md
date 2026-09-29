# RS64-A: Safe cancellation of a real per-repository import

## Objective and finish line

Implement the first mergeable increment of **chriscase/RepoSync #64**: an operator can stop an actual per-repository full-history import, see a truthful result, and restart RepoSync without losing the stop request or allowing a partial import to be mistaken for a completed repository.

This is an implementation assignment, not another inspection/DTO campaign or a planning-only handoff. Write the small state-transition contract, implement it, run the real workflow in the existing isolated harness, and return one reviewable PR. Do not implement all of #64 in this increment.

Successful journey:

`existing import start -> persisted operation -> actual importer -> cancel request -> worker quiescence or explicit uncertain outcome -> persisted status -> service restart -> no unintended replay`

Include the small existing-UI change needed to request cancellation and show progress/outcome. Keep the old import path useful when cancellation is never requested.

## Starting point and work organization

- Repository: `chriscase/RepoSync`.
- PR #72 is **merged**. Do not reopen it or continue accumulating changes on its feature branch.
- Verified merged baseline: `6b4b3587f6f442ec40e9308b13f2b927bf84f19a`.
- Baseline tree: `efafbf6e708ba3c6ea89ba1b80a7b7375857e1eb`.
- Start a new ordinary branch, preferably `feature/import-cancellation`, and a new PR against `main`.
- Inspect the selected worktree and current remote main first. Preserve local changes. If main has advanced, record the new base and relevant differences; do not overwrite unrelated work or force-push.
- The completed recovery search and accepted inspection/migration prototypes are not new tasks.
- Read #64, the applicable #62/#63 contract, #73, and the current import/scheduler/API code. Preserve the original `docs/reliability/GOAL.md` bytes and prior briefs; save this as a separate goal.
- Use a few ordinary reviewable commits separating durable lifecycle, importer/API integration, UI/tests, and the handoff. No requirement to preserve a particular number of commits.

The baseline is now green: the closeout reports workspace 376 passed / 0 failed / 1 ignored and 86 required reliability cases. The ignored conflict case is tracked by #73 and remains uncovered, not passing. Do not restore the old assumption that ordinary formatting/lint failures are acceptable.

## Scope: existing runtime, narrow operation lifecycle

Deliver against the **existing v12 per-repository import workflow**, not a second importer that exists only in tests. Reuse `run_full_import` and the actual callers. Test-only fault injection and isolation are appropriate; replacing the production algorithm with a fixture imitation is not.

The canonical v13/v14 migration and inspection prototypes merged in PR #72 remain unactivated. This increment must not activate them, create an invented generation 1, or require arbitrary legacy installations to qualify for canonical migration before they can request a safe stop.

For the bounded lifecycle record, prefer typed, versioned operation documents in a **new namespace within the existing v12 SQLite state storage**, accessed through one checked transactional repository. This is an explicit bridge to the #63/#64 design, not an alternate canonical checkpoint system. Use the same DB transaction for operation/active-run pointer changes and applicable local checkpoint updates. Avoid a second database or an unversioned JSON sidecar with independent commit semantics.

Document the storage decision and eventual migration relationship. If a small additive schema change is demonstrably necessary instead, present that exact necessity and plan as a concrete blocker for review; do not silently register new migrations or start another broad schema design. Continue delivering nonblocked code/tests rather than merely returning questions.

## Source leads to verify, not preclaimed runtime reproductions

At the merged baseline:

- `crates/core/src/import.rs` has an in-memory, nonserialized `cancel_requested` flag.
- Its loop cancellation branch takes a progress read guard and then awaits a write guard; it also calls the logging helper while a progress write guard is held. Trace the lifetimes and reproduce the lock behavior. Do not carry nested/reentrant progress locking into the fix.
- Cancellation returns `Ok(count)`. The per-repository finalizer in `crates/web/src/api/repos.rs` shares its successful-import path, reads the global `svn_rev` watermark with a `total_revs` fallback, and can update per-repo checkpoint copies. That is not proof of a completed/published import.
- Existing import progress persistence includes shared/singleton paths. New cancellation authority must not be taken from another import's global progress.
- The setup wizard has separate cancellation behavior. Adapt shared internals without breaking that workflow or exposing a second inconsistent cancellation implementation.

These are source-review leads. Produce actual real-path assertions for the corrected behavior; do not describe them as independent end-to-end reproductions already performed by the reviewer.

## Required behavior

### 1. Durable identity and checked transitions

Create an unpredictable operation ID before asynchronous execution or expensive preparation begins. Persist the initiating identity, repository ID, operation type, request identity, configured target/workdir/policy fingerprint, creation/update times, phase, cancellation request, and the exact verified progress needed to explain partial work. Store no tokens/passwords in operation payloads or public logs.

Use explicit states, for example:

- queued / running;
- cancel_requested / cancelling;
- completed / cancelled / failed;
- reconciliation_required when an external effect or interrupted local state is uncertain.

Define allowed transitions and terminal-race behavior. Implement atomic compare-and-update of the active operation and state, not several unchecked `set_state` calls. Duplicate requests are idempotent. A delayed cancel for operation A must never stop later operation B. A persistence failure must not produce an acknowledgement claiming the cancellation was durably accepted.

Keep operation identity separate from synchronization lineage. An unqualified canonical generation remains unknown/absent; operation bookkeeping does not establish SVN provenance.

### 2. Actual API and UI

Return the operation ID additively from the existing import-start response. Add an authenticated cancel action addressing the repository **and exact operation ID**, and a status response that survives restart. Preserve existing response fields used by the UI; introduce explicit lifecycle fields rather than silently changing old meanings.

The Cancel button must use the operation ID obtained from start/status. Show cancellation requested/in progress separately from stopped. Explain that already-published commits are not undone. Keep failure and reconciliation-required results actionable instead of claiming success.

Do not accept caller-supplied principal, filesystem path, credentials, target replacement, or checkpoint values. Reuse the application's applicable authenticated import permissions; cancellation must be limited to the authorized initiating context or a current authorized administrator. Preserve and explicitly test supported legacy single-admin mode without importing the fixture-only grant framework into production. Named expired/revoked/disabled identities must not gain access via a privilege fallback. No new permissions administration subsystem is part of this goal.

### 3. Cooperative cancellation and subprocess lifecycle

Persist the stop request before signalling the worker. Check it before starting a new expensive phase, between revisions, and before each new publication/verification phase. Do not keep a progress/DB lock across subprocess waits or re-enter a held lock through logging.

Supervise the long-running commands used by this import path so stalled connection/history/export/fetch/push work has declared timeout and shutdown behavior. Terminate and reap read-only child processes safely; test process-tree cleanup on the supported test platform. Merely aborting a Tokio handle, cancelling a Future, or dropping a subprocess handle is not evidence the work stopped.

Local atomic work may finish at a safe boundary. A publication already sent to Git may already have succeeded. Stopping/killing its local process does not prove it had no remote effect. Persist intent before publication, distinguish intended and observed refs, and use `reconciliation_required` if the outcome cannot be established safely. Do not start an extra push just to flush local work after cancellation.

No new write may be initiated after terminal cancellation/quiescence is confirmed. A cancellation racing a genuinely verified completion must return the truthful winning result without rewriting completed history.

### 4. Truthful finalization, checkpoints, and partial work

Use an explicit importer result (or equivalently unambiguous checked outcome), distinguishing complete import, safely cancelled partial import, failure, and uncertain publication. Update every affected caller; do not leave `Ok(count)` interpreted as full completion after a stop.

Keep UI counters distinct from real SVN revisions. Do not use `total_revs`, the latest local Git tip, or a global watermark as proof that work was processed or published. Preserve separate local-created and remote-confirmed progress where they differ. Check persistence errors rather than ignoring them on the success path.

Previously verified prefix work can remain. Unpublished local commits must remain identifiable and must not be silently published by ordinary sync. No checkpoint may advance over a failed or unverified revision. A cancelled job must not execute the successful completion handler or clear required recovery evidence.

Do not delete or reset remote repositories, clear mappings, drop the working directory, or reimport to make cancellation appear clean. Retry/resume must not be automatic. A safe manual recovery explanation is sufficient in this increment when resumption cannot yet be proved.

### 5. Restart and writer coordination

Integrate the durable active/blocked operation state with the actual scheduler and manual start paths. A partial or uncertain import remains held across restart even after its in-memory busy guard disappears. New start/delete/reset/pairing requests that would race the held work must be rejected or coordinated safely; no destructive cleanup is needed to deliver this gate.

Serialize workers for the same managed resource in the supported single-daemon topology, including duplicate start requests. Unrelated repositories must remain independently usable. Detect or refuse overlapping registrations/shared workdirs that the new operation path cannot safely coordinate. Reuse existing resource guards where correct; do not invent a distributed-lock platform.

On restart, unfinished operations must become visible and safe: no implicit full restart, no guessed completion, no automatic replay of uncertain effects. Restoring status must not run old completion logic. A missing legacy operation record is not proof of interruption; healthy preexisting repositories must keep working unchanged.

Document single-writer/mixed-version limits. An older executable that cannot read the operation record is not a safe downgrade while work is active or unresolved. General cross-host fencing and recovery are separate goals, not capabilities to claim from this slice.

## Explicitly deferred

Do not implement automatic recovery of every lost Git/SVN response, generic scheduling/queue infrastructure, all operation types, arbitrary-history canonical migration, branch refresh/rebase, repo removal, full-range browser integers, #73 conflict resolution, or more inspection DTO variants.

For a possibly completed external write, recording intent and **blocking truthfully** is acceptable here. Automatically reconciling and resuming it is later #64 work. Do not close #64 just because this first increment passes.

## Required real-path tests

Use the existing network-isolated SVN/Git fixtures and real importer/API. Add deterministic barriers/fault injection rather than timing-only sleeps. At minimum cover these grouped scenarios with individually identifiable assertions:

1. **Ordinary import:** no cancellation, verified complete history/trees/publication, correct per-repo finalization, and later ordinary synchronization still works.
2. **Early and mid-import stop:** accepted cancel while queued/connecting and during actual export or replay; responsive status/cancel endpoints; no progress-lock deadlock; correct last verified prefix; no extra push after stop.
3. **Durability/idempotence:** duplicate cancel, stale operation ID, cancel after completion, restarted service retaining cancelled/blocked status, concurrent duplicate starts.
4. **Subprocess stop:** genuinely stalled read-only child and descendant cleanup under the declared timeout; do not substitute a mock flag flip for process exit proof.
5. **Publication boundary:** controlled cancellation/crash before a batch or final push, and an already-issued push whose reply is lost. Unknown outcome remains blocked with intent retained, never marked harmless cancellation or automatically retried.
6. **Scheduler and two repositories:** after cancellation/restart the affected partial import is not scheduled; an independent repository can still import/sync; equal SVN numbers cannot exchange cursors or operation state. Exercise a conflicting same-resource request.
7. **UI/auth:** actual existing page or component integration drives the real start/status/cancel API using the operation ID; unauthorized/expired requests cause no state change; request acceptance is not displayed as completion. Preserve setup cancellation behavior.
8. **Existing-install compatibility:** a quiesced pinned pre-change v12 fixture opens with configuration, credentials, enabled states, IDs, refs, and checkpoint values preserved; ordinary startup still stays v12 with no activated v13/v14 tables/routes. No-operation repositories remain healthy; restart of new unfinished jobs remains held.
9. **Persistence and finalizer failure:** failed durable-cancel write is not acknowledged; failed checkpoint/finalization write cannot claim completion or release an unsafe job; retries do not fabricate duplicate records or remote effects.

Assertions must inspect real Git refs/logs/trees, SVN revisions, local unpublished work, operation records, and relevant directional checkpoints—not only a status label or exit code. A blocked uncertain result is a distinct expected outcome, not a successfully resumed job.

The existing 86 cases remain part of regression qualification. One baseline diagnostic currently expects the missing per-repository cancellation route: if the implemented endpoint changes that observation, explicitly strengthen/convert that diagnostic into a candidate regression and archive its old baseline meaning. Retain its deletion/non-destructive assertions. Do not leave a stale 404 expectation merely to preserve a green count, and do not weaken any unrelated oracle.

## Validation and review discipline

Keep ordinary formatting, strict lint, workspace tests, broader E2E, and isolated reliability CI green. Compare against the merged starting baseline using the existing locked method; label historical anchors accurately. Preserve the pinned lock unless a narrowly justified dependency change is explicitly documented and qualified. Retain all existing canaries, required-case enforcement, scans, and nonactivation assertions.

Use focused tests while iterating, then final full qualification at the meaningful head. Missing tooling is NOT RUN, not PASS. No repeated unchanged-tree full-suite loops, recursive receipt-only commits, broad recovery searches, or new backlog epics.

Add a short issue #64 progress comment identifying this bounded slice and the new PR. Do not change its acceptance criteria or close it. Keep #73 visible as uncovered conflict behavior. Update the coworker-facing note in a few paragraphs to explain cancel versus undo, partial publication, restart holding, and what still requires reconciliation; do not expand another report framework.

Return a compact handoff containing new PR/branch, exact base/functional/final/CI heads and tree, durable-state/API contract, which calls/paths changed, new or deliberately converted test identities, real-path results, migration/nonactivation proof, test counts and omissions, artifact/hash links, and remaining limits. Do not paste all historical review reports back into the PR.

## Stop and merge boundary

Stop at **READY_FOR_REVIEW** for this new runtime change. Do not merge the new PR without the independent delta review. Once the finite cancellation slice passes that review and CI, it should move directly to merge closeout; completing all of #64 is not a prerequisite to merging this increment.

No production endpoints or credentials, active-installation upgrade, live acceptance, release tag, force-push, deployment, or unreviewed canonical migration activation is authorized. Use only disposable test environments.

## Source references for this assignment

- Merged foundation: `https://github.com/chriscase/RepoSync/pull/72`.
- Parent work: `https://github.com/chriscase/RepoSync/issues/64`.
- Known uncovered conflict behavior: `https://github.com/chriscase/RepoSync/issues/73`.
- Baseline `crates/core/src/import.rs`, especially `ImportProgress` and `run_full_import`.
- Baseline `crates/web/src/api/repos.rs`, especially start/status/finalization; setup callers and actual scheduler guards must also be traced.

All baseline source references above are pinned to `6b4b3587f6f442ec40e9308b13f2b927bf84f19a`; new engineering requirements in this document are the next assignment, not claims that these capabilities already exist.
