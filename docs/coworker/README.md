# RepoSync coworker guide: working in Git on an SVN-origin product

This guide set is for developers who work in Git while RepoSync keeps a Git
branch and an SVN branch in step. It answers practical questions: what is safe
to do, what is not, how to see what RepoSync thinks is happening, and what to do
when it stops and asks for help.

It is part of the preventive-documentation outcome of
[issue #71](https://github.com/chriscase/RepoSync/issues/71) (under epic
[#61](https://github.com/chriscase/RepoSync/issues/61)). It documents behavior.
It does not change any branch rule, webhook, credential, workflow or running
installation, and it does not close #71 (see [Held and unavailable](held-and-unavailable.md)).

## Read this first

- **RepoSync decides, not you and not a script.** RepoSync owns locking,
  provenance, checkpoints, cancellation and recovery. A developer, a GitHub
  Action or a local hook can *ask* it to do something through its authenticated
  API. None of them should run `svn merge`, edit its SQLite database, delete
  remote branches, or force-push to repair it.
- **Publishing a rewrite is the dangerous move.** Rebasing commits nobody else
  has seen is fine. Rewriting commits already pushed to a paired branch is what
  RepoSync stops and holds for. See [History rewrites and SVN merges](history-rewrites-and-svn-merges.md).
- **A hold is a result, not a bug to route around.** `reconciliation_required`
  means RepoSync could not prove what happened to an external write and is
  refusing to guess. See [Status and reconciliation](status-and-reconciliation.md).
- **Nothing here is a release or a live qualification.** The statements below are
  checked against pinned source. See [Evidence and its limits](#evidence-and-its-limits).

## Reading order

| If you want to… | Read |
| --- | --- |
| Push, pull and understand what RepoSync does with your commits | [Ordinary sync](ordinary-sync.md) |
| Start a branch from SVN-derived history, or pair an existing Git branch later | [SVN-origin rule and pairing](svn-origin-and-pairing.md) |
| Bring a long-lived pair up to date with its parent | [Requesting a refresh](refresh.md) |
| See what RepoSync thinks, or respond to a stop | [Status and reconciliation](status-and-reconciliation.md) |
| Know when a rebase is safe, and what an SVN merge or branch recreation means | [History rewrites and SVN merges](history-rewrites-and-svn-merges.md) |
| Decide what to enforce and where (Actions, local hooks, server rules) | [Branch policy, hooks and Actions](branch-policy-and-hooks.md) |
| Quick answers, with current-versus-planned labels and release/evidence facts | [FAQ](faq.md) |
| See exactly what is not delivered yet | [Held and unavailable](held-and-unavailable.md) |

## Availability labels

Every capability in this guide set carries one of these labels. They describe
the source tree, not a deployment.

| Label | Meaning |
| --- | --- |
| **In tree** | Code exists at the pinned commit. It is *not* a release, and it is not evidence that a live installation has it or that it was qualified live. |
| **Local-only, sandbox-evidenced** | Tested within the cited disposable fixture's scope. Does not establish network, release or live support. |
| **Read-only** | Inspects and reports. It never writes to Git or SVN. |
| **Not implemented** | The server refuses the request with an explicit code. |
| **Held** | Deliberately not delivered by this documentation lane. Dependent on other work. |
| **Planned** | Wanted, not built. Nothing here promises a date or a design. |

## Calling the API without leaking your token

Every API call in this guide set uses the same session token the web UI uses.
Several operations need an **administrator** session and say so. This guide
does not show how to obtain or store credentials beyond this: keep the token out
of the command line, shell history, logs, pull requests and chat.

```bash
umask 077
HDR="$(mktemp)"
IFS= read -rs -p 'RepoSync session token: ' REPOSYNC_TOKEN; echo
printf 'Authorization: Bearer %s\n' "$REPOSYNC_TOKEN" > "$HDR"
unset REPOSYNC_TOKEN
BASE="https://reposync.example.com"   # placeholder: your RepoSync base URL
# Example read-only call:
curl --fail-with-body -sS -H @"$HDR" "$BASE/api/status?repo_id=REPO_ID"
# When finished:
rm -f "$HDR"
```

`-H @file` needs curl 7.55 or newer, and `--fail-with-body` needs 7.76 or newer.
`printf` is a shell builtin, so the token does not appear in the process list.
Examples elsewhere in this guide set reuse `$HDR` and `$BASE`.

Use HTTPS to a RepoSync you are authorized to use. Never send the token to
any other host, and never put it in a URL.

## Evidence and its limits

- **Pinned source.** Behavior statements were checked against commit
  [`ade6f9c`](https://github.com/chriscase/RepoSync/commit/ade6f9cb383f76d65c6610ef21eec2199f6c23b5)
  (the tip of `epic/reposync-reliability-61` when this guide was written).
  The refresh, operation-status and safeguards availability summary was checked
  against [`0f010a2`](https://github.com/chriscase/RepoSync/commit/0f010a232a29489b31a1c73500040f766c739f81).
  Source links and prior evidence retain their own pins; the latter are not
  fresh runtime runs at this commit.
- **Read from source, not run.** Route lists, authorization rules, request
  fields and refusal codes were read from the handlers. The Git commands in these pages
  (ancestry exit codes, hook arguments, `--no-verify`) were run once in a throwaway repository.
  No RepoSync server, SVN server or GitHub service was exercised for this guide.
- **No release.** The repository has no published GitHub release and its
  workspace version is `0.1.0`. No release number is claimed to support any
  workflow here. See the [FAQ](faq.md#which-release-supports-this).
- **No passing live qualification is claimed.** The project's own
  [acceptance matrix](https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/docs/reliability/acceptance-matrix.md?plain=1#L22)
  states that the deployed executable and schema are **not established** and
  that no live acceptance is authorized by it.

## Related existing documents

The [operator guide](../reliability/git-workflows.md) has been updated for named
status and local hooks. Where older guidance disagrees with the pinned source,
use the source; see the [FAQ](faq.md#where-older-documents-disagree-with-the-source).

- [`docs/reliability/coworker-FAQ.md`](../reliability/coworker-FAQ.md) and
  [`docs/reliability/git-workflows.md`](../reliability/git-workflows.md): the first #71 slice
- [`docs/reliability/contracts/`](../reliability/contracts/README.md): per-issue contracts (#63–#69, #71)
- [`docs/personal-branch/`](../personal-branch/README.md): single-developer Personal Branch Mode, a different deployment style
