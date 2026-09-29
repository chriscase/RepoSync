# PR74 correction goal — finish real import cancellation, then merge closeout

Read posted PR74 review **#5347108648** together with this goal.

## Identity and objective

Repository `chriscase/RepoSync`, draft PR #74, branch `feature/import-cancellation`.

Continue from independently reviewed final head:
`0c39105e1aa7a3358dfb6f13ecae7dc863272410`

Merged starting base:
`6b4b3587f6f442ec40e9308b13f2b927bf84f19a`

Read the posted PR74 review and `RepoSync-PR74-review-1.md`. Preserve original `docs/reliability/GOAL.md`, `RS64-A-goal.md`, prior briefs and existing issue acceptance criteria. Save this goal separately. Inspect current worktree/remote status first; preserve local work and do not assume a clean checkout if it changed.

Finish the **same #64-A cancellation increment**, not another prototype or all of #64. Retain the durable operation journal, actual importer/API integration, restart holds and passed publication/finalizer controls. There is no new planning-only gate.

## 1. Close the remaining import command paths (74-01)

Reproduce the actual incremental-import stop gap before correction where practical. The current importer calls `apply_diff_to_path`, whose subprocess stdin write and output wait do not observe the import cancellation signal or timeout. Its LFS preflight/install path also includes unsupervised synchronous commands.

Use the shared bounded process mechanism for every CLI command executed by this per-repository import path. Include patch-stdin delivery as well as waiting and draining output; cancellation must not hang while writing the patch. Preserve actual SVN-diff/Git-apply behavior and ordinary non-import semantics. A small reusable optional cancellation/deadline extension is preferable to a second importer or test-only implementation.

Produce a concise command-path table (preparation, SVN info/log/diff/export, Git apply, LFS preflight/install/add/commit, push and confirmation). Identify bounded noncancellable verification that is deliberately allowed after an issued write. Do not claim stopping a child proves a previously issued publication did not occur.

Actual-path required controls:
- Stall incremental `git apply` after at least one actual imported revision, with a descendant. Cancel via the exact-operation API. Prove child/descendant cannot execute, status remains responsive, no later revision/push begins, and local/remote refs/checkpoints plus durable state remain truthful.
- Test a blocked patch-input delivery boundary or demonstrate the implementation's supervised temporary-input design removes that unbounded await.
- Stall one LFS preparation command, cancel, and prove bounded quiescence and no new publication. Retain an ordinary LFS-enabled and ordinary incremental positive.
- Required persistence/cleanup failure must not be reported as safe terminal cancellation. Leave an explicit hold when quiescence or state is uncertain.

Use deterministic barriers/fault injection confined to disposable fixtures. Do not just abort a task, flip a mock flag, kill an unrelated PID or add fixed sleeps and call it proof. No new production process-service framework is necessary.

## 2. Remove the unsafe reset interaction from this bounded path (74-02)

The review's independent Git-only probe demonstrates that legacy reset first publishes a bootstrap branch, then the new absent-ref lease rejects it. Legacy reset preparation also runs destructive commands outside the journal/supervisor.

For this increment, explicitly reject `reset=true` **before local deletion, clone/credential changes, remote writes, checkpoint/mapping changes, or creating an active held operation**. Do so after applicable authentication and validation, using a clear unsupported/review-required message. Document this intentional stricter compatibility boundary. Update any visible caller that would otherwise offer the unsupported action; do not silently turn a reset request into an ordinary import.

This is not permission to erase the old reset implementation indiscriminately across setup or other workflows, nor to implement general reset/recovery now. Normal `reset=false` import, cancellation and existing healthy synchronization must remain useful. If an unavoidable dependency prevents the safe refusal boundary, publish the exact blocker and nonblocked corrections rather than weakening lease protection or force-pushing as a fix.

Use an actual API fixture with an already published repository and local working data. Prove rejected reset preserves full local/remote refs/trees, all old mapping/checkpoint data, credentials/configuration and active/latest operation records, and does not install a hold that disables that healthy repository. Preserve the existing target-ref refusal and absent-ref first-push controls.

## 3. Execute the actual card-to-API workflow (74-03)

Add the original assignment's missing small UI execution test. Mount the actual `ImportProgressCard` or drive the existing repository page against the disposable real API/importer. Do not replace the component with a test-only state renderer.

The journey must cover:
- authenticated Start returns/publishes an operation ID;
- the actual Cancel action addresses that exact ID;
- while a deterministic worker barrier is held, the UI displays cancellation requested/stopping, not completed cancellation;
- quiescent cancellation displays retained local versus confirmed remote progress honestly;
- an uncertain-publication outcome stays reconciliation-required without an automatic retry;
- a refused/failed durable cancellation write displays an error rather than success;
- reload retrieves durable terminal/held status;
- existing unauthorized/expired backend denials and ordinary completed-import behavior remain.

A small supported component/browser harness is sufficient. Reuse available tooling; justify any narrow test dependency and preserve locked comparison semantics. No broad frontend framework migration, permission system or inspection/DTO expansion.

Add the executed UI test to CI/required qualification or provide an equally explicit enforced check with commands and artifacts tied to the exact final tree. UI lint/build alone is NOT RUN for this criterion.

## Scope and preservation

- Keep PR74 on its current branch; leave merged PR72 alone.
- Keep ordinary startup/storage v12 and candidate v13/v14/inspection routes unactivated.
- Do not reset production checkpoints, reimport as recovery, contact production, migrate active installs, release, tag, deploy or force-push.
- Automatic external-effect reconciliation, cross-host fencing, arbitrary historical migration and #73 conflict resolution remain deferred. Do not close #64 or #73.
- Retain all 98 required case IDs/oracles, including the deliberately converted missing-cancel regression and its non-destructive deletion assertions. Add only the finite gap coverage above.
- Preserve scope-safe cancellation, duplicate/stale-operation behavior, independent repository progress, startup holds and normal imported-prefix handling. Do not resolve rejection tests by refusing every import.

## Validation and evidence

Use focused tests while iterating, then final full qualification on the meaningful head: formatting, strict lint, workspace tests, UI lint/build plus the executed UI journey, broader E2E and isolated exact cases/scans. Compare to merged starting main with the same documented lock and dependency-edge method. Label historical anchors correctly; the file named immediate-comparison must not be confused with the actual merged-start comparison.

The currently qualified lock is `eca9dfdc221c31be47daad874924ce09dd621ed686ecdad78426458b482591c9`; the previous goal's lock differed by one explicitly documented direct libc dependency edge. Do not claim old lock bytes are unchanged. Record any additionally necessary dependency change separately.

Preserve source/fixture manifests and distinguish local component/Git probes from real importer/API/browser executions. Do not claim tests ran when tooling is absent. Keep #73's one ignored test as uncovered, not passed. No repeated unchanged full-suite loops or recursive documentation receipts.

## Return and merge decision

Publish a few ordinary reviewable commits separating command supervision, reset refusal/compatibility, and UI/qualification. Keep the PR draft until the independent runtime delta review.

Return `READY_FOR_REVIEW` with:
- exact base, runtime head, final test/documentation head, remote head, CI merge and matching tree;
- per-finding before/fix evidence level, command-path table and supported shutdown bounds;
- reset refusal's full preservation/no-new-hold proof;
- actual UI entry point, exact command, screenshot/trace where useful, and asserted API/state sequence;
- retained/new required IDs, failures/ignored/NOT RUN, merged-base comparison, all CI links;
- archive digest independently recomputed, key case hashes, expiration and remaining limits.

Stop for the independent review. **When these bounded corrections and CI pass, the next step is merge closeout for #74—not implementation of the rest of #64.** No additional feature campaign is authorized by this goal.
