# FAQ: current, held and planned

*Back to the [guide index](README.md).*

Labels are defined in the [index](README.md#availability-labels). Everything here
was checked against source at commit
[`ade6f9c`](https://github.com/chriscase/RepoSync/commit/ade6f9cb383f76d65c6610ef21eec2199f6c23b5);
the refresh, status and safeguard entries below also account for
[the later Reliability source](https://github.com/chriscase/RepoSync/blob/0f010a232a29489b31a1c73500040f766c739f81/docs/reliability/git-workflows.md).
Nothing here was observed on a running installation.

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
| Pair refresh execute through v1 | **Local-only, sandbox-evidenced**: service-backed update; a v1 preview is not approval | [Refresh](refresh.md#local-only-update-and-re-anchor) |
| Pair refresh v2 review (inspection) | **In tree, read-only**; review does not authorize execution | [Refresh](refresh.md#step-2-v2-review-prepare-approve) |
| Pair refresh v2 prepare, approve, execute, receipt, recovery mode | **In tree**, bounded; no live or release claim | [Refresh](refresh.md#step-2-v2-review-prepare-approve) |
| Re-anchor / recreate a pair | **Local-only**: separately approved; default creates a new path, while same-name replaces current contents and moves the local paired tip; not network re-anchor | [Refresh](refresh.md#local-only-update-and-re-anchor) |
| Automatic or push-triggered refresh | **Held / planned**; no safe auto-refresh is described or claimed | [Refresh](refresh.md) |
| Opt-in manual CLI/workflow | **In tree** for preview and named status; client sandbox-tested, Actions activation and approved refresh submission still held | [Operator guide](../reliability/git-workflows.md#manual-client-and-workflow) |
| Paired-branch hook installer / remover | **Local-only, sandbox-evidenced** for a supported Git; opt-in for one clone and its linked worktrees | [Preview, install and remove](../reliability/git-workflows.md#local-hooks) |
| Branch rules for paired refs | **Recommendation only**; no rule or bypass applied by this work | [Branch policy and hooks](branch-policy-and-hooks.md#recommendation-for-paired-refs) |
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

The checks run in worktrees of the opted-in clone. A developer can bypass
local hooks, use a different clone, or publish with another client. Only
the host can apply a rule to pushes from every client. [Local checks and limits](../reliability/git-workflows.md#local-hooks)

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

- The repository's GitHub release list was empty when checked for this update.
  Evidence tags and a workspace crate version are not released or supported versions.
- The fixture and hosted CI receipts below identify source commits, not installed versions.

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
- [#258](https://github.com/chriscase/RepoSync/pull/258) closed #69 for the bounded local
  update/re-anchor scope. Its [source CI](https://github.com/chriscase/RepoSync/actions/runs/42933526424)
  does not establish network re-anchor, installed-version support, or enterprise use.
- [#292](https://github.com/chriscase/RepoSync/pull/292) and its
  [receiving CI](https://github.com/chriscase/RepoSync/actions/runs/44507834160)
  exercised named operation status against the ordinary API and tested uncertain replies with
  disposable fixtures. The workflow source was inspected; no dispatch with live credentials was run.
- [#293](https://github.com/chriscase/RepoSync/pull/293) and its
  [receiving CI](https://github.com/chriscase/RepoSync/actions/runs/44519363189)
  exercised local paired-branch hooks on Git 2.56.1 and verified refusal on older Git.
  They did not apply or qualify a live rule.
- These are historical receipts at their linked commits. This guide update does
  not rerun them or claim a released, deployed or enterprise configuration.

### Where older documents disagree with the source

These are historical descriptions of earlier implementations. Some have already
been corrected in the current [operator guide](../reliability/git-workflows.md).

| Older statement | What to use now |
| --- | --- |
| [`coworker-FAQ.md`](../reliability/coworker-FAQ.md) and [`git-workflows.md`](../reliability/git-workflows.md): a late-pair request with `dry_run: false` "returns `publish_not_implemented`" and "this slice does not create the SVN branch, the child row, or a checkpoint". | No code in `crates/` emits `publish_not_implemented`. The handler contains a publish path (v1) and a reviewed v2 publish/status/resume/cancel path. Whether either works in a live installation is not shown. |
| `git-workflows.md`: "Status and cancel require the initiating user or an admin." | True for import status and cancel. **SVN-commit status and Git-push status require an admin**, and every reconcile route listed in the status page requires an admin (or, for v2 refresh, the original admin actor). |
| Both: `POST /api/repos/{id}/sync` "records a scheduler request". | It writes an audit row; we found no reader of it. |
| Earlier #71 docs: `refresh_execute_not_implemented` and `reanchor_not_implemented` were the only outcomes. | Current source has local service-backed operations. A v1 preview still cannot approve a writer, and network re-anchor is not supported. See [Refresh](refresh.md). |
| `git-workflows.md`: the sections on reconciliation list one import reconcile route. | Separate reconcile routes also exist for SVN commits and Git pushes. |

See [what is still held](held-and-unavailable.md#the-held-remainder) before
considering any workflow or host changes.
