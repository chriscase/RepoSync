# #71 slice: manual operation request and workflow docs

The manual client and opt-in workflow implement the request and status portion
of [#71](https://github.com/chriscase/RepoSync/issues/71). The operator guide
includes commands for [checking status](../git-workflows.md#checking-status).

## What shipped

| Item | This slice |
| --- | --- |
| Git workflow documentation | Ordinary sync, late-pair preview, refresh preview versus execute, status, reconciliation-required, SVN-origin examples |
| Coworker FAQ | Current versus implemented versus planned, including SVN append versus a lineage event |
| Manual client | `scripts/git_operation_request.py` calls the authenticated API |
| Manual workflow | `.github/workflows/git-operation-request.yml` is `workflow_dispatch` only |
| Branch policy | Recommendation text only. No ruleset was applied |
| Refresh execute | The client checks the v1 preview and its eligibility before submission. Local execution approved through RepoSync has a separate [execution contract](69-pair-refresh-execution-v2.md). |
| Webhooks, hook install, push trigger | Not delivered |

The preview uses the parent and child IDs, expected tips, plan digest and
idempotency key. For a named operation, status checks the identity and state
returned by the server. Import exposes the latest journal for a repository.
SVN commit and Git push expose individual journals. For a v2 refresh, use
the original request ID and child ID. The response contains a separate
operation UUID. A refresh status request without either ID repeats a preview.

Polling after an accepted submission requires a recognized route in the
pinned repository or pair and the same operation ID in the reply. A failed
lookup or polling limit exits nonzero and preserves the accepted ID. A
verified terminal failure, cancellation or reconciliation hold also exits
nonzero. A direct status read can succeed and report any of those states.
Import cancellation uses the existing cancellation route. The manual client
reports `cancel_not_applicable` for refresh.

## Tests

`python3 scripts/test_git_operation_request.py` runs the client against a
loopback HTTP stub. It covers named status, a superseded import, receipt and
operation ID mismatches, malformed replies, interrupted polling and terminal
states. It also exercises the existing preview pins, import cancellation,
connection failures and redaction. The workflow is checked statically for
manual dispatch, input wiring and read-only repository permissions.

`cargo test -p reposync-web --test truthful_status rs71_operator_status_reads_named_journals -- --exact`
runs the Python command against the regular import, Git-push and SVN-commit
HTTP routes on loopback. The fixture creates two successive import journals,
a held Git push and a running SVN commit in disposable SQLite storage. It
checks the process exit code, returned ID and state, and compares stored
journal values before and after the reads.

## Authority

Workflows and the client only request. Locking, provenance, checkpoints,
cancellation, and recovery stay in RepoSync. No production endpoint, credential,
force-push, remote deletion, or independent `svn merge` is part of this slice.
