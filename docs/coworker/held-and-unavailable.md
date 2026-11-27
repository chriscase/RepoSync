# Held and unavailable

*Back to the [guide index](README.md).*

The source-pinned coworker guides from #247 and #256 document ordinary sync,
SVN-origin pairing, refresh, status and reconciliation. Issue #71 remains open.
The following pieces have since been delivered on Reliability:

- [#258](https://github.com/chriscase/RepoSync/pull/258): bounded local refresh
  and separately approved local re-anchor. See [Refresh](refresh.md).
- [#86](https://github.com/chriscase/RepoSync/pull/86) and
  [#292](https://github.com/chriscase/RepoSync/pull/292): opt-in manual client
  and workflow source; disposable tests cover preview, named status and safe
  reporting of uncertain submissions. See the [operator guide](../reliability/git-workflows.md#manual-client-and-workflow).
  The v1 preview is not execution approval. The client can read an existing v2
  execution or no-op receipt; it does not turn a preview into an approved refresh.
- [#293](https://github.com/chriscase/RepoSync/pull/293): explicit opt-in
  [local paired-branch guards](../reliability/git-workflows.md#local-hooks).
  They protect the selected clone's normal Git commands, including linked
  worktrees; they are not server-side enforcement.

## The held remainder

| #71 criterion | Still open | Safe next step |
| --- | --- | --- |
| Approved manual refresh integration | Submitting and tracking an explicitly approved refresh via the same RepoSync service as the UI. The v1 preview, CLI idempotency key and mock submission fixtures do not prove this works with the real writer. | Use read-only [preview or review](refresh.md); use named status for a known request. Do not resubmit an uncertain write. |
| Required webhook authenticity and live webhooks | Receivers now check scope, deduplicate delivery IDs and treat stale or out-of-order deliveries as inspection hints, but still accept payloads when no secret is configured. No live webhook was configured or qualified. Receivers are only poll hints and are not proof of lineage. | Configure webhook secrets. Rely on RepoSync's authoritative inspection from polling. Do not create or change a webhook. |
| Automatic or push-triggered refresh | Not enabled or qualified. A post-push Action cannot prevent the push that triggered it. | Do not script a rebase, force-push or independent SVN merge. |
| Live workflow and enterprise reachability | No live dispatch, credentials, environment or private runner configuration was qualified. Workflow source permissions, redaction and network requirements are documented separately. | Do not configure or activate the workflow from this guide. |
| Live branch rules | A recommendation exists, not a deployed ruleset or approved bypass. | An authorized administrator must decide and qualify any live change. |
| Release or installed support | No release or installed version is qualified by this guide. Network re-anchor was not delivered by #69. | See [the release and evidence limits](faq.md#which-release-supports-this). |

A passing fixture is evidence for its tested scope, not an acceptance waiver for
the remaining webhook, approved-operation, runner or live branch-rule work.
See the [evidence links](faq.md#what-evidence-exists-and-where-it-stops).

## The first-slice artifacts already in the tree

The `scripts/git_operation_request.py` client, its test file, and the opt-in
`workflow_dispatch` workflow are described in the
[operator guide](../reliability/git-workflows.md#manual-client-and-workflow).
The workflow environment and credentials are not created by these documents.
