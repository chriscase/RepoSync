# #71 slice: manual operation request and workflow docs

This is the smallest product slice of issue #71. It does **not** close #71 or
#61. Auto-triggered refresh, live webhooks, hook installation, and live branch
rules are not in this change.

## What shipped

| Item | This slice |
| --- | --- |
| Git workflow documentation | Ordinary sync, late-pair preview, refresh preview versus execute, status, reconciliation-required, SVN-origin examples |
| Coworker FAQ | Current versus implemented versus planned, including SVN append versus a lineage event |
| Manual client | `scripts/git_operation_request.py` calls the authenticated API |
| Manual workflow | `.github/workflows/git-operation-request.yml` is `workflow_dispatch` only |
| Branch policy | Recommendation text only. No ruleset was applied |
| Refresh execute | Not invented. Client reports `refresh_execute_not_implemented` |
| Webhooks, hook install, push trigger | Not delivered |

The client pins `repo_id`, `pair_id`, expected tips, `plan_digest`, and an
idempotency key. It polls only an existing same-origin `/api/` status path
that contains the pinned id. Import cancel and import/SVN-commit status use
the routes that already exist. Refresh cancel authenticates and then reports
that no durable refresh job exists.

## Tests

`python3 scripts/test_git_operation_request.py` uses a loopback HTTP stub.

Covered: authorized preview, import status, and import cancel; wrong repo,
pair, and token; stale approval; repeated request and a moved digest; an
idempotency key reused for another pair; untrusted branch text with zero HTTP
calls; a refused connection; a redirect that must not receive the bearer
token; redaction of the token and of URLs.

Not run: live GitHub Actions secrets, a self-hosted enterprise runner, or a
production RepoSync. The workflow file is checked statically for
`workflow_dispatch`, `contents: read`, and the absence of a pull-request
trigger. Hosted-runner network policy is documented, not exercised.

## Authority

Workflows and the client only request. Locking, provenance, checkpoints,
cancellation, and recovery stay in RepoSync. No production endpoint, credential,
force-push, remote deletion, or independent `svn merge` is part of this slice.
