# Git-centric RepoSync workflows

This is the first slice of issue #71. It documents how to stay in Git and
request RepoSync operations, and it adds an opt-in manual client. It does not
close #71 or #61.

RepoSync is the authority for locking, provenance, checkpoints, cancellation,
and recovery. GitHub Actions, a local script, and the web UI are clients. They
call the authenticated HTTP API. They do not run `svn merge`, edit SQLite,
delete remotes, or force-push.

The session token is the same `Authorization: Bearer` token the UI sends.
Refresh and late-pair routes require an admin session. Status and cancel
require the initiating user or an admin. A missing, expired, or wrong token is
rejected. Do not put the token in a URL, a log line, or a pull request.

## SVN-origin restriction

A managed history has to descend from a verified SVN import. A branch name, a
matching file, or an edited commit message is not lineage.

Valid example. SVN trunk is imported to Git `main`. The verified mapping says
Git `a1b2c3d4e5…` is SVN r1200. A developer creates the feature branch from
that commit:

```text
git checkout main
git merge-base --is-ancestor a1b2c3d4e5… HEAD   # exit 0
git checkout -b feature/widget
```

`feature/widget` is an SVN-derived Git branch. Pairing it later with
`branches/widget` is the late-pair workflow below.

Invalid example. A second repository was created with `git init` and has no
ancestor in the SVN import. Its branch is also named `feature/widget`, and its
README matches the SVN tree. Those facts do not admit the branch. RepoSync
refuses the pairing when `git merge-base --is-ancestor` does not show that the
verified SVN baseline is an ancestor of the Git tip.

## Ordinary sync

Day to day, developers commit and push fast-forward updates on the paired Git
branch. RepoSync's scheduler is what replays those commits to SVN and SVN
revisions back to Git.

An operator can request a cycle the same way the UI does:

```text
POST /api/repos/{repo_id}/sync
Authorization: Bearer <session>
```

The response `{"ok": true, "message": "Sync triggered"}` means the request was
recorded. It does not mean the cycle finished. Read the repository status
(`GET /api/status?repo_id={repo_id}`, same bearer token) or the UI. If the
repository is held for reconciliation or removal, the sync route refuses and
no new cycle starts.

Ordinary publication appends commits. It is a fast-forward of the paired ref.
It is not a force push.

## Creating or late-pairing an SVN-derived branch

1. Create the Git branch from the verified SVN-derived tip, as in the valid
   example above. Push that branch with a fast-forward. Do not publish an
   unrelated root.
2. Ask RepoSync to admit it. The UI calls:

```text
POST /api/repos/{parent_id}/branches
Authorization: Bearer <session>
Content-Type: application/json

{
  "svn_branch": "branches/widget",
  "git_branch": "feature/widget",
  "preview": true,
  "dry_run": true
}
```

`dry_run` defaults to true. `dry_run: false` is a publish attempt and is
refused with `publish_not_implemented`. This slice does not create the SVN
branch, the child row, or a checkpoint. `auto_create_svn_branch` and
`auto_create_git_branch` are ignored. The plan reports the pinned tips and
whether the SVN-origin baseline was proved.

## Requesting refresh

`POST /api/repos/{pair_id}/refresh` with an admin bearer token is the same
route the UI uses.

| Request | Server result today |
| --- | --- |
| `{"operation":"update_pair_from_parent","execute":false}` | Preview. Pins both tips, the SVN revisions, and `plan_digest`. No durable job and no remote write. |
| `execute: true` or `dry_run: false` | `refresh_execute_not_implemented`. No job is created. |
| `operation` of `reanchor` or `recreate` | `reanchor_not_implemented`. |

Issue #69 execution is not implemented. The manual client calls the preview
route, checks the pins, and stops when the plan says execution is not
approved. It does not invent an execute job. If a future server marks
`approval.eligible` true and `execute_status` is not `NOT_IMPLEMENTED`, the
client sends `execute: true` with the same token and idempotency key, then
polls a same-origin status path only when that response includes one under
`/api/` for the pinned repo or pair.

Pin all of these on every preview:

- parent `repo_id` and child `pair_id`
- expected pair and parent Git tips when you know them
- `plan_digest` once the first preview has shown it (required before any
  execute attempt)
- an idempotency key that you do not reuse for a different pin set

The current preview handler does not store that idempotency key, because it
does not start a job. The client keeps a local journal and refuses to send a
different repo, pair, tip, or branch under the same key. A repeated preview
with the same pins is sent again. If the digest moved, the client reports
`stale_approval` and does not execute.

## Checking status

Refresh preview has no operation id. `status` with `operation_kind=refresh`
repeats the authenticated preview and reports that plan. `durable_job_started`
stays false.

Durable operations that already exist are read with the same bearer token:

| Kind | Route |
| --- | --- |
| Import | `GET /api/repos/{repo_id}/import/status` |
| SVN commit | `GET /api/repos/{repo_id}/svn-commit/{operation_id}` |

The client prints the lifecycle the server returned. It does not translate a
transport error into success.

## Responding to reconciliation-required

`reconciliation_required` means an external effect may have happened and
RepoSync could not prove it. The hold is the result. It is not a cue to repair
the branch from a laptop.

1. Read status with the bearer token and keep `operation_id`, `intended_ref`,
   `intended_git_sha`, the last local and last confirmed positions, and
   `outcome_detail`.
2. Compare those records with the remote ref. Do not force-push, delete the
   ref, edit SQLite, or run an independent `svn merge`.
3. A later recovery call, when one exists for that operation, is still an
   authenticated RepoSync request (`POST /api/repos/{repo_id}/import/{operation_id}/reconcile`
   for a held import). This client does not call it. If the lifecycle stays
   `reconciliation_required`, stop.

Cancel is authenticated too. Import cancel is
`POST /api/repos/{repo_id}/import/{operation_id}/cancel`. Refresh cancel
checks the session at `GET /api/auth/me` and then reports
`cancel_not_applicable`, because refresh execute never started a job.

## Manual client and workflow

Run the client against the RepoSync base URL. The token comes from
`REPOSYNC_API_TOKEN` and is not a command-line argument.

```text
export REPOSYNC_API_TOKEN=...   # admin session, same credential the UI uses
python3 scripts/git_operation_request.py preview \
  --base-url https://reposync.example \
  --repo-id <parent-id> \
  --pair-id <pair-id> \
  --idempotency-key <unique-key> \
  --expected-pair-tip <sha> \
  --expected-parent-tip <sha>
```

Use `execute`, `status`, or `cancel` as the subcommand. Pass
`--operation-kind import` or `svn-commit` with `--operation-id` for those
durable routes. `--git-branch` is compared with the JSON plan only. Values
that contain shell metacharacters, a leading `-`, or `..` are rejected before
any HTTP call.

The GitHub workflow `.github/workflows/git-operation-request.yml` is
`workflow_dispatch` only. It checks out with `persist-credentials: false`,
grants the GitHub token `contents: read`, and passes the operator inputs
through the environment into the same script. It does not run on push. It
does not run from a pull request. The job is limited to `refs/heads/main` and
to the `reposync-operations` environment.

That environment is not created by this change. An administrator who later
chooses to enable the workflow should:

- store `REPOSYNC_BASE_URL` and `REPOSYNC_API_TOKEN` as environment secrets,
  not as repository secrets that other workflows can read
- require a reviewer before the environment is used
- allow the environment only on the default branch

Until those exist, dispatch has nowhere to send a token. This issue does not
authorize creating the secrets, the environment, a runner, or a webhook.

Refresh `execute` exits non-zero with `refresh_execute_not_implemented` while
#69 execution is absent. A failed execute job is the honest result.

## Least privilege and redaction

The workflow's GitHub token cannot write contents, packages, or pull requests.
The RepoSync token is not given to a job that runs pull-request code. Branch
names are not interpolated into `run:` steps. The client rejects redirects so
the bearer token is not forwarded to another host, and it refuses a non-HTTPS
base URL except loopback HTTP used by the unit tests.

Stdout is JSON. Token strings, URL userinfo, and any `http://` or `https://`
URL are replaced with `[redacted]` or `[redacted-url]` before printing.
Server error bodies are passed through that same redaction.

## Network reachability

The runner only needs HTTPS to the RepoSync API. RepoSync itself reaches SVN
and the Git host; this workflow does not.

GitHub-hosted runners have public egress. They cannot open a connection to a
RepoSync bound to localhost or to a private address. A self-hosted runner used
for an enterprise install has to sit where that HTTPS call is allowed. GitHub
Enterprise allow lists must include that runner, or the runner must already be
inside the network. No webhook ingress is configured here, and polling the
API is what observes a rewrite when webhooks are absent.

The client tests cover an API that refuses the connection. They do not claim a
particular GitHub-hosted or self-hosted runner image was exercised with live
secrets.

## Branch-policy recommendation

This is documentation only. No ruleset, protected-branch setting, or bypass
was applied to this repository, and none should be applied from this slice.
Live changes need a separate decision by Chris or another admin.

For each paired ref, a host rule that blocks non-fast-forward updates and
blocks deletion preserves ordinary fast-forward sync. Do not require a pull
request or a status check on that ref if RepoSync publishes the fast-forward
directly. Do not grant this workflow, or its token, a bypass.

Illustrative GitHub ruleset shape, not an applied rule:

```json
{
  "name": "reposync-paired-ref-recommendation",
  "target": "branch",
  "enforcement": "active",
  "conditions": {
    "ref_name": {
      "include": ["refs/heads/feature/widget"],
      "exclude": []
    }
  },
  "rules": [
    {"type": "deletion"},
    {"type": "non_fast_forward"}
  ],
  "bypass_actors": []
}
```

Name the paired refs. Do not use this pattern to freeze every developer
branch. A force push that the rule rejects should stay a visible failure.
RepoSync's own ancestry checks still apply; a ruleset is not a substitute.

## Local hooks

A `pre-rebase` hook can refuse a local rebase, and a `pre-push` hook can warn
before a non-fast-forward publish. Both are optional. Users can skip them with
`--no-verify` or by changing `core.hooksPath`. They are not enforcement.

This slice does not install or remove hooks, does not change `core.hooksPath`,
and does not modify a clone. A later guarded installer would have to preserve
an existing hook chain, honor the branch name `pre-rebase` receives, and work
with linked worktrees. That installer is not part of this change.

## Still later

- Webhook signature checks, event deduplication, and live webhook configuration
- Auto-triggered refresh on push
- Hook installation
- Applying the branch-policy recommendation
- Pair-refresh execution and re-anchor (#69)
- Closing #71 or #61
