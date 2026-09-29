# PR74 bounded correction: preserve unconfirmed process cleanup

Repository: `chriscase/RepoSync`
PR: #74; branch: `feature/import-cancellation`
Reviewed head: `84465429c6089d90447f91d50428f2b76e521f1f`
Merged base: `6b4b3587f6f442ec40e9308b13f2b927bf84f19a`

## Outcome

Correct **74-04 only**: an unconfirmed process stop must never become permission to continue importing or a false safely-cancelled result. Keep the accepted reset refusal, mounted browser proof, supervised apply/LFS ordinary controls and prior durable-journal work. This is the remaining runtime review finding, not a new operations architecture assignment.

Read posted review **#5347974000** on PR74 and `RepoSync-PR74-review-2.md`. Preserve the original GOAL, 64A brief and earlier closeout brief. Save this continuation separately and record its hash. Verify actual remote/base/head/worktree state before edits; do not overwrite unrelated work.

## Problem to reproduce

The supervisor returns a distinct I/O error when group termination or full output/reap completion is unconfirmed. It must not be collapsed into generic command failure or cancellation:

- With cancel=false, incremental Git apply's error match currently falls back to full export even after unknown cleanup.
- SVN info/log/export errors with cancel=true become Cancelled without inspecting cleanup certainty; diff errors can fall back too.
- Preparation finalization infers safe stop from a cancel flag and absent local/intended journal positions without knowing whether clone/inspection actually quiesced.

The provided Linux process probe is a component illustration, not the required actual-engine regression. You may inject the explicit uncertainty result deterministically inside the existing sealed fixture boundary; keep real importer, operation storage and finalization code in the path. A safely contained helper that retains output is another option. Do not spawn unbounded or unmanaged processes, leave them alive, require actual signal permission failures, or wait out repeated full production timeouts.

## Implementation requirements

1. Preserve an explicit outcome distinction for success/nonzero finished command, confirmed cancellation/timeout, spawn/pre-execution failure, and unconfirmed cleanup. A small typed error/result or shared classification helper is appropriate; no message-substring protocol or new generic framework.
2. Propagate unknown cleanup through Git/SVN wrapping and every import caller of the supervisor. In particular audit clone, remote inspection, SVN info/log/diff/export, apply, LFS preparation, local CLI commit, push and bounded post-write reads. Do not expand into unrelated normal-sync refactors.
3. Unknown cleanup halts the current operation before a fallback export, copy, later revision, commit, push or automatic retry. The cancellation flag does not weaken this rule.
4. Preserve the durable active hold and exact operation identity. Record a failed/reconciliation-required outcome with truthful cleanup-unknown detail, or retain an existing fail-closed state if the write itself fails. Do not mark safe Cancelled or Completed without the required proof.
5. Keep local and confirmed positions distinct and retain outstanding publication intent. No checkpoint clearing, auto-reimport, remote repair or old-state reset.
6. Preparation drop handling must use an explicit safe/uncertain result, not infer safety merely from zero recorded progress. A no-work cancel may be classified safely only when it is established no command is still running.
7. Existing ordinary failed-patch fallback may remain only when the command is known to have exited and the fallback's existing preconditions hold. Preserve successful incremental import, ordinary cancellation, independent-repository behavior and LFS publication controls.
8. Do not silently broaden supported process-tree guarantees. General containment/fencing is outside scope; inability to prove cleanup must be represented honestly. Report tested platforms and process-group boundary.

## Finite required tests

Keep all 104 existing IDs and assertions. Add named exact coverage sufficient to prove:

- Actual importer/API, no user cancel, apply cleanup unknown after timeout: no fallback export or later local/remote writes, no checkpoint advance, durable hold.
- Actual importer/API, user cancellation plus SVN cleanup unknown: no safely-cancelled/completed result and no next revision/publication.
- Preparation clone/inspection cleanup unknown with cancel flag: no false quiesced-preparation finalization and no worker start.
- Kill failure and output/reap timeout, each with cancel=true and false, retain uncertainty through the shared classifier/wrappers. This can be a focused table-driven test, not eight new end-to-end fixture projects.
- Normal finished-command failure and confirmed interruption retain their intended behavior; healthy import and cancellation controls remain green.
- Restart/status/UI representation of the uncertainty remains held and does not claim safe cancellation. Extend the existing browser or API-status assertion narrowly; do not rebuild the already accepted browser journey.

Use execution counters/barriers to prove forbidden next work did not run. Merely observing a held pointer after the function returns is insufficient. Preserve full remote refs, local tracked/index state where relevant, mappings and checkpoints around the fault. Explicitly kill/reap fixture helpers in cleanup even on failure.

## Work and qualification

Use ordinary focused commits on this branch: error propagation, focused regressions, necessary contract clarification. Keep schema/startup at v12 and candidate migration/inspection activation unchanged. No UI redesign, recovery search, general #64 service or unrelated topology campaign.

Run targeted tests while iterating. On the meaningful final head, run format, strict required lint/build/tests, existing UI checks, broader E2E/browser and the expanded isolated suite. Preserve the documented pinned dependency graph and compare against the actual merged base; label historical anchors correctly. Do not hide #73 or weaken the old missing-cancel/deletion assertions. Do not rerun unchanged full suites just to generate new receipts or hash-only commits.

## Handoff and merge boundary

Return READY_FOR_REVIEW with:

- Exact base, prior head, functional/final head, CI merge/tree and worktree state.
- Small caller/result propagation table, corrected 74-04 locations and per-case proof.
- Demonstrated no-fallback/no-next-write counters, held outcome and restart/status behavior.
- Targeted/full test counts, skips, matched comparison and browser result at exact head.
- Downloadable scanned artifacts with API/recomputed hashes and expiry.
- Explicit unchanged v12 activation boundary and remaining scope limits.

Stop for independent review of this small runtime correction. Once accepted, green CI and normal merge-closeout checks are the finish line for PR74. Do not wait for all #64 or #73 work. Do not merge this unreviewed runtime change, release, tag, deploy, access production, force push, reset checkpoints, or reimport as recovery.
