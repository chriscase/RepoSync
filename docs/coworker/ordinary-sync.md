# Ordinary sync

*Back to the [guide index](README.md).*

Day to day, you work on a paired Git branch the normal way. RepoSync's
scheduler replays your new commits to the paired SVN branch and replays new SVN
revisions back to Git. You do not need to call anything for that to happen.

## What you do

1. Commit on the paired Git branch.
2. Push with an ordinary **fast-forward** push. Your push adds commits on top of
   what is already on the remote branch. Do not use `--force` or
   `--force-with-lease` on a paired branch.
3. If `git push` is rejected as non-fast-forward, **stop**. Do not "fix" it with a
   force push. Fetch, look at what is on the remote, and integrate with a merge
   or a new commit. Rewriting a paired branch is covered in
   [History rewrites and SVN merges](history-rewrites-and-svn-merges.md).

```bash
git fetch origin
git status -sb                       # how far ahead/behind are you?
git log --oneline origin/feature/widget..HEAD   # what you are about to publish
git push origin feature/widget       # plain push: fast-forward only
```

That plain `git push` is the safe default because the Git host itself refuses a
non-fast-forward update unless you override it. (Whether the *host* also refuses
an override is a server-side rule; see [Branch policy](branch-policy-and-hooks.md).)

## What RepoSync does with your push

**In tree.** The scheduler wakes on a timer (`poll_interval_secs`; the example
configuration uses 15 seconds) and runs one sync cycle per managed repository.
In a cycle, the team engine fetches the remote branch into an inspection area
and checks it **before** it resets its own checkout or writes to SVN. A push
that adds commits on top of what RepoSync has already handled is admitted and
replayed. A push that is not a descendant of what RepoSync has handled is
**rejected for replay and the pair is held**. The rejection reasons are listed
in [Status and reconciliation](status-and-reconciliation.md#why-a-pair-gets-held).

RepoSync's ordinary Git publication goes through a plain (non-force) push
method. The setup and import paths differ from each other, so they are not
lumped together:

- **Journaled import publication** (a snapshot import, or a full import with an
  operation record) publishes its first commit set with
  `--force-with-lease=refs/heads/<branch>:`. The empty expected value means Git
  creates the branch only if it is absent and refuses if it already exists. That
  is a creation, not a rewrite. Later batches are plain pushes.
- **An older un-journaled full-import path**, used when no repository and
  operation record are present, pushes its first batch with a plain `git push --force`.
- **Setup "Reset & Reimport"** (admin-only) deletes RepoSync's local clone and
  first force-pushes a new empty reset commit to the configured branch; it then
  prepares and starts the reimport. That push rewrites the configured branch.

None of these is part of ordinary sync. How they relate to a branch rule is in
[branch policy](branch-policy-and-hooks.md#what-rules-will-and-will-not-interfere-with-reposync).

Sources: [scheduler tick loop][scheduler-loop], [config example][config-example],
[the #66 history-containment contract][c66], [ordinary Git push][git-push],
[journaled import lease][import-force], [snapshot import call][import-snap],
[full-import first batch][import-full], [un-journaled `--force`][import-legacy],
[setup reset & reimport][reset-reimport].

## Can I make it sync now?

`POST /api/repos/{repo_id}/sync` exists and any signed-in user may call it. Read
its result literally:

```bash
curl --fail-with-body -sS -X POST -H @"$HDR" "$BASE/api/repos/REPO_ID/sync"
# => {"ok":true,"message":"Sync triggered","repo_name":"…","enabled":…,"import_phase":"…"}
```

At the pinned commit, this handler checks that the repository exists, refuses if
an import is active or unresolved or if removal is in progress, writes an audit-log
row, and returns. **We found no code in the tree that reads that audit row to
start a cycle.** The reply says the request was recorded; it does not say a
cycle ran or finished. Your push is picked up by the next scheduled poll. To
see what happened, read [status](status-and-reconciliation.md#reading-the-status), not this reply.

Source: [`trigger_sync`][trigger-sync]. If a later commit wires this to the
scheduler, this section should be rechecked.

## Webhooks that already exist

**In tree.** The web server has `POST /webhook/github` and `POST /webhook/svn`.
They are separate from the authenticated API above. What the handlers do today:

| Behavior | GitHub / Gitea push | SVN post-commit |
| --- | --- | --- |
| Authenticity check | HMAC signature header checked **only if** a webhook secret is configured; if none is configured the handler logs a warning and accepts the payload | Shared-secret header checked **only if** configured; otherwise accepted with a warning |
| What it does on a valid event | Sends one "sync now" signal to the scheduler and broadcasts a UI notification | Same |
| `forced` flag | Reported back as a **hint** (`forced_hint`, `inspection: polling_safety_gate`); never used to reset anything | n/a |
| Duplicate delivery IDs | Not tracked by the handler | Not tracked |
| Stale or out-of-order events | The event is not replayed; it only prompts a poll, so a stale event costs one extra poll | Same |

So a webhook is only an accelerator: the same history inspection that polling
performs is what decides whether to replay. A forced push is caught by polling
even if no webhook arrives. Hardening webhook authenticity, deduplication and
stale-event handling to the standard in issue #71 is **held**; see
[Held and unavailable](held-and-unavailable.md). This guide does not tell you
to create or change a webhook.

Sources: [`github_webhook`][gh-webhook], [`svn_webhook`][svn-webhook],
[scheduler webhook tick][scheduler-webhook].

## Where the work is *not* done by sync

- A **late pair** (a Git branch that existed before it was paired) is an admission
  decision with a review step, not a normal sync. See [SVN-origin rule and pairing](svn-origin-and-pairing.md).
- Pulling a parent's newer work into a long-lived pair is a **refresh**, not a
  sync and not a rebase. See [Requesting a refresh](refresh.md).

[scheduler-loop]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/daemon/src/scheduler.rs#L333-L351
[scheduler-webhook]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/daemon/src/scheduler.rs#L345-L351
[config-example]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/config.example.toml#L4-L6
[c66]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/docs/reliability/contracts/66-history-containment.md
[git-push]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/git/client.rs#L578-L580
[import-force]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/import.rs#L2625-L2634
[trigger-sync]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/repos.rs#L1178-L1226
[gh-webhook]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/webhooks.rs#L87-L205
[svn-webhook]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/webhooks.rs#L207-L257
[import-snap]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/import.rs#L3121-L3135
[import-full]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/import.rs#L3846-L3860
[import-legacy]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/import.rs#L3884-L3930
[reset-reimport]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/setup.rs#L1468-L1572
