# FAQ: current, held and planned

*Back to the [guide index](README.md).*

Labels are defined in the [index](README.md#availability-labels). Everything here
was checked against source at commit
[`ade6f9c`](https://github.com/chriscase/RepoSync/commit/ade6f9cb383f76d65c6610ef21eec2199f6c23b5);
nothing here was observed on a running installation.

## Current versus planned, at a glance

| Capability | Label | Where |
| --- | --- | --- |
| Scheduled sync of fast-forward Git pushes to SVN and SVN revisions to Git | **In tree** | [Ordinary sync](ordinary-sync.md) |
| History gate: inspect before reset or write, hold on rewritten or unproven Git history | **In tree** (contract lists which cases are proven by local fixtures and which are not run) | [Status and reconciliation](status-and-reconciliation.md#why-a-pair-gets-held) |
| `POST /api/repos/{id}/sync` | **In tree**: records an audit row; we found nothing that starts a cycle from it | [Ordinary sync](ordinary-sync.md#can-i-make-it-sync-now) |
| `/webhook/github` and `/webhook/svn` receivers | **In tree**: signature or secret checked only if one is configured; no delivery-ID dedup | [Ordinary sync](ordinary-sync.md#webhooks-that-already-exist) |
| Hardened webhook authenticity, scope authorization, dedup, stale-event handling | **Held** | [Held and unavailable](held-and-unavailable.md) |
| Status observation (`GET /api/status?repo_id=`) | **In tree, read-only** | [Status and reconciliation](status-and-reconciliation.md#reading-the-status) |
| Re-inspect a held import, SVN commit or Git push (reconcile) | **In tree**, admin only | [Status and reconciliation](status-and-reconciliation.md#looking-at-one-specific-operation) |
| Late-pair preview (v1 and reviewed v2) | **In tree, read-only** | [Pairing](svn-origin-and-pairing.md) |
| Late-pair publish, status, resume, cancel (reviewed v2); v1 publish | **In tree**; no live qualification claimed | [Pairing](svn-origin-and-pairing.md#step-2-publish-only-on-a-reviewed-result) |
| "Start from now" (`skip_import`) | Refused unless a compatibility flag is also set; checkpoints are not advanced | [Pairing](svn-origin-and-pairing.md#what-not-to-use) |
| Pair refresh preview (v1) | **In tree, read-only** | [Refresh](refresh.md) |
| Pair refresh execute through v1 | **Not implemented** (`refresh_execute_not_implemented`) | [Refresh](refresh.md) |
| Pair refresh v2 review (inspection) | **In tree, draft, read-only** | [Refresh](refresh.md#step-2-draft-v2-review-prepare-approve) |
| Pair refresh v2 prepare, approve, execute, receipt, recovery mode | **In tree, draft**; qualification gates open | [Refresh](refresh.md#step-2-draft-v2-review-prepare-approve) |
| Re-anchor / recreate a pair | **Not implemented** (`reanchor_not_implemented`) | [Refresh](refresh.md), [Rewrites](history-rewrites-and-svn-merges.md) |
| Automatic or push-triggered refresh | **Held / planned**; no safe auto-refresh is described or claimed | [Refresh](refresh.md) |
| Opt-in manual request workflow and client | **Held** in this documentation lane | [Held and unavailable](held-and-unavailable.md) |
| Hook installer / remover | **Held**; none exists | [Branch policy and hooks](branch-policy-and-hooks.md) |
| Branch rules for paired refs | **Recommendation only**; nothing applied | [Branch policy and hooks](branch-policy-and-hooks.md#recommendation-for-paired-refs) |
| Downloadable Git `pre-commit` path-rule script | **In tree** | [Branch policy and hooks](branch-policy-and-hooks.md) |
| A passing live qualification of any of the above | **Not claimed** | [Evidence](#what-evidence-exists-and-where-it-stops) |

## Questions

### Can I rebase my branch?

Unpublished commits: yes. Commits already on a paired branch: no. The full table
is in [History rewrites and SVN merges](history-rewrites-and-svn-merges.md#which-case-am-i-in).

### Can a GitHub Action stop someone force-pushing?

No. A workflow triggered by `push` runs after the update was accepted. It is not a
pre-receive barrier. See [Branch policy](branch-policy-and-hooks.md#push-triggered-actions).

### Can a local Git hook stop it?

Only on the machine that installed it, and only until someone passes `--no-verify`,
changes `core.hooksPath`, uses a clone without the hook, or uses a tool that does
not run it. It is a seatbelt, not a lock. See [Local hooks](branch-policy-and-hooks.md#local-hooks-useful-nudges-not-enforcement).

### What can actually prevent it?

A server-side rule on the host (a protected branch or ruleset blocking non-fast-forward
updates and deletion on the named paired refs) or a host-side `pre-receive` hook where
the host supports one. Both need an authorized administrator. This guide applied none.

### Is RepoSync going to rewrite SVN history to match my rebase?

No, and nothing in this guide set promises it. A rewrite of published paired
history is held for investigation. See [Rewrites](history-rewrites-and-svn-merges.md#what-reposync-does-not-promise).

### A webhook said `forced: true`. Does RepoSync reset because of it?

No. In the handler the flag is echoed back as `forced_hint` with
`inspection: polling_safety_gate` and is never used to reset anything. The same
inspection that polling does decides what happens.

### I called `POST /api/repos/{id}/sync` and got "Sync triggered". Did a cycle run?

Not necessarily. At the pinned commit the handler records an audit entry and returns;
we found no code that starts a cycle from that entry. Your push is picked up by the next
scheduled poll. Use [status](status-and-reconciliation.md#reading-the-status) to see what happened.

### Does an SVN merge rewrite history?

No. A normal merge-and-commit appends a new revision and may update `svn:mergeinfo`.
Deleting and recreating a branch, copying something else into its place, or replacing
the repository's history is a separate lineage event. See [Rewrites](history-rewrites-and-svn-merges.md#normal-svn-merge-versus-delete-recreate-or-replace).

### Can I make a coordinated "update from parent" happen automatically?

No. Not today, and this guide does not describe a safe way. Ask an administrator for a
read-only preview or review. See [Requesting a refresh](refresh.md).

### Which release supports this?

**None is claimed.** What we can establish:

- The repository has **no published GitHub release** (the release list was empty when this was written, 2026-11-06).
- The only tags are ten evidence-style tags such as `rs69-lifecycle-…-v1`, `rs112-verify-…-v1` and `rs64-setup-idle-…-v1`. None is a `v*` tag, which is what the release workflow triggers on. This guide has not examined what those tags contain and does not treat them as a release or a qualification.
- The workspace crate version is `0.1.0`.
- So there is no release number to put in a support table, and we are not inventing one.

To find out what a particular running server is: `GET /api/status/health` needs no
sign-in and returns `ok`, `version` and `git_commit`. Compare `git_commit` to the commit
you expect. The project's own acceptance matrix states that the **deployed
executable and schema are not established**, so that comparison is something an
administrator has to do for their installation.

### What evidence exists, and where it stops

What exists in the repository: per-issue contracts and closeout documents under
[`docs/reliability/`](../reliability/contracts/README.md), the
[acceptance matrix](../reliability/acceptance-matrix.md) and its required-case catalog, and
Rust fixture and integration tests that use disposable local repositories. The
history-containment contract says which of its cases are proven by such fixtures and
marks others **NOT RUN**.

Where it stops:

- Fixture and loopback tests are not an enterprise or live qualification. The matrix says
  "local/offline PASS is not enterprise/live PASS" and that no live acceptance is authorized by it.
- The v2 refresh contract says its full qualification, exact-head CI and independent review
  are still open, and that positive Linux restart recovery still needs to be executed.
- Nothing in this guide set ran a build, a test suite, a server, a workflow or a live service.
  The only things run were `git` commands in a throwaway repository, read-only lookups of the repository and
  issue, and static checks of these pages (links, anchors, source line ranges, example syntax).
- Hosted-runner, self-hosted-runner and GitHub Enterprise behavior for any workflow is untested here.
- No sandbox run of a request workflow with live secrets exists to link.

### Where older documents disagree with the source

These documents were written for the first #71 slice and are not edited here. Where
they differ from the pinned source, follow the source.

| Older statement | At the pinned commit |
| --- | --- |
| [`coworker-FAQ.md`](../reliability/coworker-FAQ.md) and [`git-workflows.md`](../reliability/git-workflows.md): a late-pair request with `dry_run: false` "returns `publish_not_implemented`" and "this slice does not create the SVN branch, the child row, or a checkpoint". | No code in `crates/` emits `publish_not_implemented`. The handler contains a publish path (v1) and a reviewed v2 publish/status/resume/cancel path. Whether either works in a live installation is not shown. |
| `git-workflows.md`: "Status and cancel require the initiating user or an admin." | True for import status and cancel. **SVN-commit status and Git-push status require an admin**, and every reconcile route listed in the status page requires an admin (or, for v2 refresh, the original admin actor). |
| Both: `POST /api/repos/{id}/sync` "records a scheduler request". | It writes an audit row; we found no reader of it. |
| `git-workflows.md`: "Issue #69 execution is not implemented." | True of the v1 route. A separate **v2** prepare/approve/execute/receipt API exists in the tree as an unqualified draft. |
| `git-workflows.md`: the sections on reconciliation list one import reconcile route. | Separate reconcile routes also exist for SVN commits and Git pushes. |

The first #71 slice also left a manual client script and a `workflow_dispatch`
workflow in the tree; see [Held and unavailable](held-and-unavailable.md#the-first-slice-artifacts-already-in-the-tree).
