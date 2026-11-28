# Requesting a refresh

*Back to the [guide index](README.md).*

A *refresh* brings a long-lived child pair up to date with its parent. The
intended default is **update pair from parent**: a coordinated, history-preserving
operation that accounts for pending work on both sides. It is not a rebase, a
reset, or a force-push, and nothing in RepoSync turns one of those into a refresh.

**Short version for developers:** ask a RepoSync administrator for a **read-only
preview or review**. Do not use `git rebase`, a force-push, or an independent
`svn merge` to imitate a refresh. The tested local update/re-anchor does not
make network or deployed refresh safe. There is no automatic refresh.

## What exists, at a glance

| Capability | Entry point | Boundary |
| --- | --- | --- |
| v1 preview of "update pair from parent" | `POST /api/repos/{pair_id}/refresh` | Admin; read-only. A plan digest is not execution approval. |
| v1 service-backed update or explicitly approved local re-anchor | Same route | **Local-only, sandbox-evidenced**; separate writer path. |
| v2 inspection-only review | `POST /api/v2/repos/{pair_id}/refresh/review` (and `/review/candidate`) | Admin, read-only; cannot authorize execution. |
| v2 prepare → approve → execute → receipt | `POST …/refresh/prepare`, `POST …/refresh/action`, `GET …/refresh/requests/{request}` | Original administrator; bounded service path, not a live or enterprise qualification. |
| Optional CLI preview and named status | [Manual operator guide](../reliability/git-workflows.md#manual-client-and-workflow) | CLI tested in disposable fixtures. It does not submit an approved refresh against the v1 preview. |
| Automatic or push-triggered refresh; network re-anchor | None supported by this guide | **Unavailable**. |

`{pair_id}` is the **child** pair's repository ID. Do not copy a fixture's
operation or use a writer route against a deployed pair based on this page.

Sources: [v1 handler][v1], [v2 routes and handlers][v2-routes].

## Step 1: preview (v1, read-only)

```json
{ "operation": "update_pair_from_parent" }
```

```bash
curl --fail-with-body -sS -X POST -H @"$HDR" -H 'Content-Type: application/json' \
  --data-binary '{"operation":"update_pair_from_parent"}' \
  "$BASE/api/repos/PAIR_ID/refresh"
```

An empty `operation` is treated as `update_pair_from_parent`. The preview
fetches the current tips on both sides and returns a **plan** that pins:

- both Git tips and both SVN revisions and UUIDs;
- each side's verified baseline and its pending (unsynchronized) commits;
- conflicts, such as both sides having countable unsynced work;
- a `plan_digest`: a SHA-256 over those pinned inputs, so a moved tip gives a different digest.

It starts no durable job and performs no external write. It does not say work was
discarded. If a side's history was rewritten or is unproven, the plan reports it and
does not treat replacement commits as new work.

The preview reports `execute_status` and `reanchor_status` as
`refresh_service`, but `approval.eligible` stays `false`. That is a pointer
to a separate path, not permission to execute. A v1 `plan_digest` cannot
approve a v2 action or local re-anchor. [Preview fields][core-refresh]

If the pair has no parent, the request is refused as `not_a_branch_pair`.

## Step 2: v2 review, prepare, approve

The v2 flow is separate from v1. Every call needs a **current, enabled
database-administrator session**; the legacy in-memory sessions are refused. A
request is owned by the actor who started it.

1. **Review (inspection only).** `POST /api/v2/repos/{pair_id}/refresh/review`
   with the strict empty body `{}`. It shows what is pending on child and parent
   and whether the state is `ready` or `blocked`. It never changes
   `execution_eligible: false`, never creates an approval or a durable request,
   and does not drain, synchronize, reset or repair anything. `pending_count: null`
   means *unknown*, not zero.
2. **Prepare.** `POST /api/v2/repos/{pair_id}/refresh/prepare` with
   `{"request_id":"<id you choose>"}`. Keep and reuse that ID across retries and
   disconnects. A successful reply may contain an `approval` (preparation ID,
   operation ID, approval digest, expiry, `kind` = `integration` or `no_op`) or an
   existing durable `receipt`. An approval also reports the full Git object IDs in
   `details.git_tips.child` and `details.git_tips.parent`. Compare both with the
   tips you intend to approve. Submit the digest from this same preparation only
   if both match. Older servers omit these fields. A client that requires pinned
   tips must stop when either field is absent. A recovery approval reports the
   original operation's input tips, including after a partial publication.
3. **Approve and execute.** `POST /api/v2/repos/{pair_id}/refresh/action`:

   ```json
   {
     "request_id": "THE_SAME_REQUEST_ID",
     "preparation_id": "00000000-0000-0000-0000-000000000000",
     "approval_digest": "DIGEST_FROM_THE_APPROVAL",
     "action": "execute"
   }
   ```

   The **prepared v2 flow** uses `execute`, `reconcile` or `cancel`. Unknown
   fields are rejected. The approval **expires after five minutes**. RepoSync
   re-observes the remotes, lineage, credentials and configuration before it
   writes. The same action route accepts `reanchor` separately; that action
   uses the local service and its own approval and journal, not this prepared
   flow.
4. **Read the receipt.** `GET /api/v2/repos/{pair_id}/refresh/requests/{request_id}`
   reads the prepared v2 flow's execution or no-op receipt. Repeating a completed
   request in that flow returns its original receipt and does not publish again.
   This GET does not read **v1 local updates or local re-anchors**. A 404 after
   either local operation is not evidence that nothing was written. A lost
   HTTP response does not abandon an approved publication midway through an
   external command.

`reconcile` observes a retained publication intent; it cannot retry the write.
`cancel` can cancel an unused preparation or an eligible pre-intent job; an
uncertain intent keeps its hold, and cancellation, expiry or a missing worker cannot
release it.

Sources: [web handlers][v2-handlers], [service lifetime and actions][service],
[v2 contract][v2-api].

## Local-only update and re-anchor

[#69 closed](https://github.com/chriscase/RepoSync/issues/69#issuecomment-6783961127)
through [#258](https://github.com/chriscase/RepoSync/pull/258), for local Git
directories under the daemon data directory and a shared `file://` SVN
repository. The local service refuses non-local SVN URLs. A v1 update write
binds the pins read by its own request, not an earlier preview. The local update
refuses pending child Git or SVN work and leaves it in place. Local re-anchor
requires its own approval and has two distinct effects:

- **Default new generation (also the only v1 choice):** creates a new SVN path
  and Git generation ref, retaining the old SVN path and generation evidence
  without moving the existing local paired branch.
- **Separately approved same-name re-anchor:** commits replacement contents at
  the current SVN path and moves the existing local paired Git branch tip. It
  keeps prior SVN revisions and Git history, but does not leave the current
  contents or tip unchanged.

Neither mode deletes an SVN path or force-pushes a network Git ref. Local update
and local re-anchor use a separate journal from the prepared v2 flow. Do not
substitute the v2 receipt GET for an administrator's inspection of an uncertain
local result. [Local service][local] · [SVN effects][local-svn] ·
[Git effects][local-git] · [Bounded execution contract][execution]

The v2 service also restricts what it can safely admit. A stale approval,
unproven lineage or unsupported content is a reason to stop, not a reason to
fall back to the v1 writer. Do not interpret the local evidence or a v2 receipt
as support for a live network installation.

## Restart recovery (administrators)

If the daemon cannot start because a retained refresh job needs authenticated
native recovery, the contract describes a separate recovery mode
(`--pair-refresh-recovery`, with the existing config). It serves only the
recovery/status/action surface, does not run the scheduler, and must not run
alongside the ordinary daemon. There is no "force-adopt" or "discard" action.
Details and refusal conditions are in the [v2 contract][v2-api]. This guide does
not walk through it, because it is an operator procedure on a live data directory.

## What to do instead of rebasing onto the parent

| You want to… | Do this |
| --- | --- |
| Get the parent's newer work into your pair | Ask an administrator for a v1 preview or a v2 review, and wait for a decision. Meanwhile, keep committing on top of your own branch. |
| Check whether you are behind the parent | Ask for the preview. Don't compare tips by eye: SVN revision numbers alone do not tell you what is pending on a branch. |
| Start a new pair generation | Do not replace history yourself. The default approved local re-anchor creates a new path and keeps the old generation. Same-name re-anchor changes the current contents and local tip. Network re-anchor is not supported. See [History rewrites and SVN merges](history-rewrites-and-svn-merges.md). |
| Make refresh happen automatically on every parent push | Not provided. Don't script `git rebase`, `git push --force`, or `svn merge` to imitate one. |

## Safe next steps

- If a preview or review shows `both_advanced`-style conflicts, unknowns, or a
  blocked state, treat it as information. Don't try to "clear" it by resetting
  your branch.
- If the server refuses the operation or its approval, stop. Do not change
  flags, repeat with a new ID or try another route to bypass that refusal.
- If an executed refresh left a held or uncertain state, go to
  [Status and reconciliation](status-and-reconciliation.md).

[v1]: https://github.com/chriscase/RepoSync/blob/0f010a232a29489b31a1c73500040f766c739f81/crates/web/src/api/repos.rs#L3661-L3723
[core-refresh]: https://github.com/chriscase/RepoSync/blob/0f010a232a29489b31a1c73500040f766c739f81/crates/core/src/pair_refresh.rs#L724-L786
[v2-routes]: https://github.com/chriscase/RepoSync/blob/0f010a232a29489b31a1c73500040f766c739f81/crates/web/src/api/pair_refresh.rs#L24-L50
[v2-handlers]: https://github.com/chriscase/RepoSync/blob/0f010a232a29489b31a1c73500040f766c739f81/crates/web/src/api/pair_refresh.rs#L81-L207
[service]: https://github.com/chriscase/RepoSync/blob/0f010a232a29489b31a1c73500040f766c739f81/crates/core/src/pair_refresh/service.rs#L387-L405
[v2-api]: ../reliability/contracts/69-pair-refresh-v2-api.md
[local]: https://github.com/chriscase/RepoSync/blob/0f010a232a29489b31a1c73500040f766c739f81/crates/core/src/pair_refresh/local_operations.rs#L82-L156
[local-svn]: https://github.com/chriscase/RepoSync/blob/0f010a232a29489b31a1c73500040f766c739f81/crates/core/src/pair_refresh/local_operations.rs#L428-L466
[local-git]: https://github.com/chriscase/RepoSync/blob/0f010a232a29489b31a1c73500040f766c739f81/crates/core/src/pair_refresh/local_operations.rs#L474-L559
[execution]: ../reliability/contracts/69-pair-refresh-execution-v2.md
