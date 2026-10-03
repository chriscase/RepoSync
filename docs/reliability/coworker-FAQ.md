# RepoSync: feedback response, improvement plan, and FAQ

## September 2026 implementation update: full-import Stop

The new per-repository full-import flow assigns a persistent operation ID. Its Stop button requests cancellation of that exact job. “Cancellation requested” means the worker is still stopping; “Cancelled” means it has stopped. Already published Git commits stay published. The page shows the last locally created and remote-confirmed positions separately.

If a Git push may have succeeded but its result cannot be verified, RepoSync records **reconciliation required** and holds that repository. A cancelled partial import is also held. Restart does not restart the import or let the ordinary scheduler treat it as complete. Another, independent repository can keep working. An operator must compare the local and remote histories with the recorded operation before choosing a later recovery action; resetting checkpoints or reimporting is not the cancellation procedure.

This is the first bounded #64 implementation, not automatic recovery for every interrupted external write. Setup-wizard import now occupies the same per-repository busy slot and `import_operation_v1` journal as ordinary imports. The existing #73 conflict test remains ignored until its fixture and behavior are corrected.

## October 2026 status: current, implemented, and planned

This section describes the tree that adds the first #71 slice. It does not close #71 or #61. The version installed in the active environment is still not established, and this slice is not #41 release evidence. Links below are source and local tests, not a live sandbox run of the new workflow.

| Behavior | Current product behavior | In this slice | Planned, not done here |
| --- | --- | --- | --- |
| Ordinary sync | `POST /api/repos/{id}/sync` records a scheduler request. The response is not a finished cycle. | Documented. The manual client does not replace the scheduler. | No change to sync semantics. |
| Late pairing | `POST /api/repos/{parent}/branches` can preview an SVN-derived branch. `dry_run: false` returns `publish_not_implemented`. | Documented, with one valid and one invalid SVN-origin example. | Publishing the pair, creating the remote branch, and checkpoints stay later work. |
| Pair refresh | `POST /api/repos/{pair}/refresh` previews and pins `plan_digest`. `execute: true` returns `refresh_execute_not_implemented`. Re-anchor returns `reanchor_not_implemented`. | The manual client calls that preview and reports the refusal. It does not invent execute. | #69 execution and re-anchor. |
| GitHub request path | The UI already uses bearer sessions. | `scripts/git_operation_request.py` and a `workflow_dispatch` workflow call that same API for preview, import/SVN-commit status, and import cancel. | Push-triggered refresh, webhooks, and live secrets. |
| Branch protection | Repository rules were not changed by the reliability work. | A recommendation that would block non-fast-forward updates and deletion of paired refs, without blocking fast-forward sync. | Applying a ruleset requires a separate admin decision. |
| Local hooks | Not installed by RepoSync in this tree. | Limitations are documented. No clone is modified. | A guarded installer that preserves an existing hook chain. |
| Reconciliation | Imports and SVN commits can remain in `reconciliation_required`. | Status prints that lifecycle and says not to force-push, delete the ref, edit SQLite, or run an independent `svn merge`. | Automatic recovery is not claimed. |
| SVN merge versus lineage | A normal SVN merge adds a revision. Deleting and recreating a branch, or replacing repository history, is a different identity event. | Restated below as current guidance, not as a new merge implementation. | Content and mergeinfo coverage still needs explicit tests. |

Local evidence for the client is `python3 scripts/test_git_operation_request.py` (loopback HTTP only). The workflow file is not authorized to receive a production token. Exact host support for GitHub-hosted versus self-hosted enterprise runners is the reachability note in [Git-centric workflows](git-workflows.md): the runner must reach the RepoSync API over HTTPS, and this slice did not run that workflow with live secrets.

**Prepared for:** Chris and the RepoSync team  
**Date:** September 23, 2026  
**Tracking:** [Reliability epic #61](https://github.com/chriscase/RepoSync/issues/61)  
**Status:** Issues filed; implementation and runtime qualification are not yet complete.

## What we are changing

Your feedback points to a shared reliability problem, not just several missing buttons: RepoSync needs a trustworthy record of which histories belong together, which changes are actually synchronized, and which long-running operations are still active or only partly finished.

The plan is to address that foundation and then improve the workflows built on it. The goals are to preserve existing installations, stop unsafe replay, make imports cancellable, support late branch pairing correctly, offer a fast SVN snapshot starting point, and provide a coordinated way to update long-lived pairs.

The code review was pinned to `87379741779a6259f7eeb52a68cc6f061174e5ef` on `main`. The version currently installed in the active environment has **not** been established, and the reported incidents have **not** yet been reproduced by this review in a running sandbox. Source findings below therefore describe the reviewed code, not a claim that we inspected the active installation.

## The SVN-origin rule remains in place

Every managed history must have a verified SVN origin. Working primarily in Git does not change that rule.

A supported target workflow is: SVN baseline → Git mirror → developer creates a Git branch from that mirror → developer makes changes → the branch is paired with an SVN build branch later. The developer used Git to create the reference, but its source history came from SVN. An unrelated Git repository or orphan history being used to create the original SVN history is not an allowed workflow.

The new pairing logic must demonstrate that relationship using actual ancestry and recorded SVN identity, not just a branch name, a matching file tree, or an editable commit message. This requirement is in [#63](https://github.com/chriscase/RepoSync/issues/63) and [#67](https://github.com/chriscase/RepoSync/issues/67).

## Responses to the observations

### 1. “Deleting a repository only disables it. Is there a trash bin?”

**Current code:** The root-repository delete handler explicitly sets `enabled=false` and returns “repository disabled.” In that inspected flow, there is no separate trash expiration or eventual cleanup operation. It is not appropriate to promise that the entry will disappear later. Branch-pair deletion is a different handler and can delete remote branches, so those two actions must not be treated as interchangeable. [Source: repository API][repo-api]

**Plan:** Separate three operations in the interface: disable synchronization; remove the entry and its owned local working data from RepoSync; and separately authorize remote branch deletion. Removing an entry will leave upstream Git/SVN repositories alone by default. Existing clients calling the old root DELETE must not become destructive after an upgrade. Previously disabled entries must not be purged automatically.

Removal must first establish that workers have stopped, handle child-pair dependencies, and preserve enough audit/recovery information to retry a partial cleanup. See [#65](https://github.com/chriscase/RepoSync/issues/65).

### 2. “Why can’t I cancel a full-history import?”

**Current code:** Per-repository import start/status/cancel and the setup-wizard import path both use a durable `import_operation_v1` row plus the process-wide busy slot. Cancellation means stop safely, not undo published history. Disabling a repository is not the same as stopping an already-running task. Deleting the Git destination is not a reliable cancellation mechanism. [Sources: repository API][repo-api], [shared importer][import], [setup API][setup]

**Plan:** Give each import a persistent operation ID, a real Cancel action, and accurate queued/running/cancelling/cancelled/failed/completed states. Cancellation requests will survive a restart and target the intended operation, not a later import. Long-running subprocesses need bounded stop behavior and cleanup.

Cancellation means **stop safely**, not “erase commits already published.” The result must show what completed, what remains, and whether it is safe to resume. An uncertain remote write must be reconciled rather than silently repeated. See [#64](https://github.com/chriscase/RepoSync/issues/64).

### 3. “Commits made before pairing do not reach SVN.”

**Current code:** The `skip_import` pairing path can record the existing Git tip as the synchronization checkpoint. That can acknowledge older commits without transferring or reconciling them. The same handler can create the SVN branch from the parent's current SVN revision, which need not represent the Git branch's starting point. These are concrete reasons to investigate the behavior you reported. [Source: pairing implementation][pairing]

**Plan:** Find a verified common SVN-derived baseline, enumerate work added on each side, and preview the intended reconciliation. A new SVN branch should start at the corresponding proven SVN revision, or an explicitly selected reconciled baseline—not an unrelated current HEAD. Existing SVN targets must be compared, not merely accepted because their names already exist.

Your earlier Git work must arrive exactly once. SVN-only work must also be retained. Conflicts must stop for a decision. The pair must not become active until the baseline and pending work are verified. See [#67](https://github.com/chriscase/RepoSync/issues/67).

### 4. “A Git rebase creates duplicate SVN commits.”

**Current code:** The reviewed team path resets its local checkout to the fetched remote branch and then gathers commits until it encounters the stored checkpoint SHA. That path does not first establish the required ancestry relationship. This is consistent with the reported duplication, although the exact incident still needs a reproducible fixture. [Sources: Git client][git-client], [team change discovery][engine]

Git rebase creates replacement commits; a synchronization checkpoint tied only to an old commit identity is not enough to decide which changes remain to be applied. [Git rebase documentation][git-rebase]

**Plan:** Detect non-fast-forward or unproven history before destructive local reset or remote writes. Pause the affected pair with useful old/new-tip evidence rather than replaying arbitrary history. Polling must provide this protection even without webhooks. Missing history objects or a deleted remote must not trigger an automatic reset/reimport. See [#66](https://github.com/chriscase/RepoSync/issues/66).

We are not promising that every arbitrary Git rewrite can be automatically converted into a safe SVN operation. Containment comes first; approved reconciliation follows.

### 5. “Deleting the pair currently being viewed produces a 404.”

**Plan:** Add a browser regression test, then navigate to the surviving parent or repository list after actual removal. Cancelled, failed, busy, and partially completed operations must remain understandable. If removal becomes asynchronous, accepting the request must not be displayed as completed deletion. The initial source lead is the detail page's deletion mutation and query invalidation; the full browser reproduction is still pending. [Source: detail page][detail]

This is a separately reviewable small fix, tracked in [#70](https://github.com/chriscase/RepoSync/issues/70).

### 6. “We need a coordinated rebase for reused branches.”

**Plan:** Provide **Update pair from parent** as the history-preserving default. It should account for pending changes on both sides, show conflicts, pin the expected starting points, and perform one coordinated, recoverable operation. It must preserve correct mappings and avoid turning inherited changes into duplicates.

A true replacement/re-anchor is a distinct operation. Its design must retain the old generation's evidence, protect unsynchronized work, and explain changes to names, references, and downstream clones. Destructive in-place replacement will not be disguised as ordinary refresh. The safe default can be delivered while a replacement mode remains explicitly unimplemented and separately gated. See [#69](https://github.com/chriscase/RepoSync/issues/69).

### 7. “Can we create a release mirror without importing all history?”

**Current code:** Personal mode already contains a snapshot importer. This is not a capability that needs to be invented from scratch, but the reviewed team onboarding does not expose the same explicit verified workflow. [Source: personal initial import][snapshot]

**Plan:** Offer full history or a snapshot at SVN HEAD/a selected revision. Resolve HEAD once to a fixed revision, materialize and verify its actual tree, and record the SVN-origin baseline before normal sync starts. Subsequent SVN changes and Git work based on that snapshot must synchronize normally. The interface will clearly say that earlier SVN history was intentionally omitted. Simply advancing checkpoints on an empty or unrelated Git repository is not an acceptable shortcut. See [#68](https://github.com/chriscase/RepoSync/issues/68).

## Answers to the Git/SVN workflow questions

### Could GitHub Actions keep us in Git rather than visiting RepoSync?

Yes—as a client of RepoSync's API, not as a second sync engine. The manual client and `workflow_dispatch` workflow now request a pair-refresh preview, and they read status or cancel durable import and SVN-commit operations that already exist. Refresh execution is still `refresh_execute_not_implemented`. The workflow does not run on push. See the October 2026 status section and [Git-centric workflows](git-workflows.md).

GitHub supports event-driven and manually dispatched workflows. A workflow responding to a push is not a pre-receive barrier: it cannot prevent the push that triggered it. [GitHub workflow events][actions]

The workflow must not independently run SVN merges, edit RepoSync's database, or force-push a branch. That would create a second coordinator with different locks and recovery behavior. Requests use the same authenticated API as the UI, with the token kept in an environment secret and with pinned inputs. Automatic refresh stays disabled until manual execution exists and is qualified. See [#71](https://github.com/chriscase/RepoSync/issues/71).

### What happens when somebody “rebases” from the SVN side?

An ordinary SVN update-from-parent workflow is a **merge followed by a new commit**, not Git-style replacement of earlier committed revisions. SVN keeps revisioned repository snapshots; a sync merge adds a later revision and may update `svn:mergeinfo`. A jump in revision numbers alone is not evidence of history being out of order. [SVN merging][svn-merge], [SVN revisions][svn-revisions]

The content and merge-property cases need explicit RepoSync tests; we are not claiming that today's implementation handles every variation correctly. Git/SVN graphs do not have to look identical, but their mapped content and provenance must agree.

A normal SVN merge **appends** revisions. The earlier revisions stay in the repository, and the merge commit (or the merge revision) is a new point in history. Revision numbers increasing is expected.

Deleting and recreating the SVN branch path, copying a different source into its place, or administratively replacing repository history are a separate lineage event. They are not an ordinary batch of new revisions, and they are not what "update pair from parent" means. Re-anchor remains `reanchor_not_implemented`. The current guidance is to stop and reconcile, not to treat the replacement as the next fast-forward.

### Can hooks prevent rebase on paired branches?

A local **`pre-rebase` hook** can reject the local rebase command. A **`pre-push` hook** can check publication. These are useful guardrails, but users can remove or bypass local hooks; they are not a complete enforcement boundary. Server-side update/pre-receive hooks can reject updates only where the hosting service supports and authorizes them. [Git hooks][hooks]

For GitHub-managed paired branches, the recommendation is to disallow unauthorized force pushes and deletion while still allowing ordinary fast-forward sync. That recommendation is written in [Git-centric workflows](git-workflows.md). Actual protection depends on the host, permissions, and bypass settings. No live branch settings or bypasses have been changed. [GitHub protected branches][protection]

RepoSync's own ancestry validation remains necessary even with hooks and branch protection. Until the new behavior is qualified, avoid rewriting already-published paired history. An already-desynchronized pair should be investigated with its old/new tips and mappings preserved—not “repaired” by blind checkpoint edits or full reset.

## How upgrades and testing will work

The first deliverable is an isolated reproduction environment using real disposable SVN/Git repositories and the real RepoSync paths. It will reject production targets, use separate temporary state and credentials, and suppress or isolate notifications. Existing test infrastructure will be extended, including its zero-test guard. [Current E2E workflow][e2e]

Upgrade tests will start from old installations created with pinned old code and synthetic history. They must preserve configuration, repository IDs, credentials/inheritance, disabled state, history mappings, local data, and verified checkpoints; then prove ordinary new changes still flow in both directions. The exact actively installed version must be added to that qualification before release.

Recovery testing includes interruption after a remote commit succeeds but before RepoSync saves its checkpoint. A retry must identify the already-applied change instead of duplicating it. Restoring an old database after new remote writes is not automatically safe; the release procedure must distinguish a pre-write restore from a post-write reconciliation/roll-forward.

Local tests, local provider/API tests, disposable real enterprise tests, and active-environment acceptance will be reported separately. Passing one tier does not imply the next passed. No production acceptance or deployment is authorized by the issue-creation work.

## Tracking and next steps

| Work | GitHub issue |
|---|---|
| Overall plan and review gates | [#61](https://github.com/chriscase/RepoSync/issues/61) |
| Sandboxed reproductions and regression matrix | [#62](https://github.com/chriscase/RepoSync/issues/62) |
| Lineage, checkpoints, and safe migrations | [#63](https://github.com/chriscase/RepoSync/issues/63) |
| Durable jobs and cancellation | [#64](https://github.com/chriscase/RepoSync/issues/64) |
| Disable/remove semantics | [#65](https://github.com/chriscase/RepoSync/issues/65) |
| Rewrite detection and containment | [#66](https://github.com/chriscase/RepoSync/issues/66) |
| Late pairing | [#67](https://github.com/chriscase/RepoSync/issues/67) |
| Snapshot initialization | [#68](https://github.com/chriscase/RepoSync/issues/68) |
| Coordinated pair refresh | [#69](https://github.com/chriscase/RepoSync/issues/69) |
| Deletion-page navigation | [#70](https://github.com/chriscase/RepoSync/issues/70) |
| Git workflow integration and safeguards | [#71](https://github.com/chriscase/RepoSync/issues/71) |
| Enterprise qualification and rollout | [#41, updated](https://github.com/chriscase/RepoSync/issues/41) |

The implementation agent starts with reproductions and the state/migration design on a feature branch. Chris passes the PR and exact commit to the reviewing chat. Code, tests, and unresolved risks are checked before the next wave. The coworker is not being asked to use the active installation as the development test harness.

[repo-api]: https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/crates/web/src/api/repos.rs
[pairing]: https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/crates/web/src/api/repos.rs#L1100-L1400
[import]: https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/crates/core/src/import.rs
[setup]: https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/crates/web/src/api/setup.rs
[git-client]: https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/crates/core/src/git/client.rs
[engine]: https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/crates/core/src/sync_engine.rs#L1457-L1540
[snapshot]: https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/crates/personal/src/initial_import.rs
[detail]: https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/web-ui/src/pages/RepoDetail.tsx
[e2e]: https://github.com/chriscase/RepoSync/blob/87379741779a6259f7eeb52a68cc6f061174e5ef/.github/workflows/e2e.yml
[git-rebase]: https://git-scm.com/docs/git-rebase
[hooks]: https://git-scm.com/docs/githooks
[actions]: https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows
[protection]: https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches
[svn-merge]: https://svnbook.red-bean.com/en/1.8/svn.branchmerge.basicmerging.html
[svn-revisions]: https://svnbook.red-bean.com/en/1.8/svn.basic.in-action.html
