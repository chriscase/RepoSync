# Requesting a refresh

*Back to the [guide index](README.md).*

A *refresh* brings a long-lived child pair up to date with its parent. The
intended default is **update pair from parent**: a coordinated, history-preserving
operation that accounts for pending work on both sides. It is not a rebase, a
reset, or a force-push, and nothing in RepoSync turns one of those into a refresh.

**Short version for developers:** you cannot safely do this yourself with
`git rebase` and `git push --force`. Today you can ask an administrator for a
**read-only preview or review** of what a refresh would do. **Executing** a
refresh is in the tree only as an unqualified draft and is not something this
guide recommends running on a live installation. Automatic refresh does not
exist, and this guide does not describe one as safe.

## What exists, at a glance

| Capability | Route | Who | Label |
| --- | --- | --- | --- |
| v1 preview of "update pair from parent" | `POST /api/repos/{pair_id}/refresh` | admin | **In tree, read-only** |
| v1 execute (`execute: true` or `dry_run: false`) | same route | admin | **Not implemented**: refused with `refresh_execute_not_implemented` |
| Re-anchor / recreate the pair | same route, `operation` = `reanchor`, `re-anchor`, `re_anchor`, `recreate`, `re-create` or `recreate_pair` | admin | **Not implemented**: refused with `reanchor_not_implemented` |
| v2 inspection-only review | `POST /api/v2/repos/{pair_id}/refresh/review` (and `/review/candidate`) | admin | **In tree, draft, read-only** |
| v2 prepare → approve → execute → receipt | `POST …/refresh/prepare`, `POST …/refresh/action`, `GET …/refresh/requests/{request}` | admin | **In tree, draft**: qualification gates open |
| Automatic or push-triggered refresh | none | none | **Held / planned**: not provided |
| A GitHub Action or client that requests refresh | n/a | n/a | **Held**: see [Held and unavailable](held-and-unavailable.md) |

`{pair_id}` is the **child** pair's repository ID. The v2 routes are registered
when the web server starts; the v2 contract itself describes them as a writer
draft whose full qualification, exact-head CI and independent review are still
open.

Sources: [v1 handler][v1], [operation parsing and refusals][core-refresh],
[v2 routes][v2-routes], [v1/v2 contract][v2-api], [web server wiring][wiring].

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

The preview also reports `execute_status` and `reanchor_status`; at the pinned
commit both are `NOT_IMPLEMENTED`, and `approval.eligible` is `false`. **A v1
`plan_digest` cannot approve a v2 action.**

If the pair has no parent, the request is refused as `not_a_branch_pair`.

## Step 2 (draft): v2 review, prepare, approve

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
   existing durable `receipt`.
3. **Approve and execute.** `POST /api/v2/repos/{pair_id}/refresh/action`:

   ```json
   {
     "request_id": "THE_SAME_REQUEST_ID",
     "preparation_id": "00000000-0000-0000-0000-000000000000",
     "approval_digest": "DIGEST_FROM_THE_APPROVAL",
     "action": "execute"
   }
   ```

   `action` is one of `execute`, `reconcile`, `cancel`. Unknown fields are
   rejected. The approval **expires after five minutes**. RepoSync re-observes the
   remotes, lineage, credentials and configuration before it writes.
4. **Read the receipt.** `GET /api/v2/repos/{pair_id}/refresh/requests/{request_id}`.
   Repeating a completed request returns its original receipt and does not
   publish again. A lost HTTP response does not abandon an approved publication
   midway through an external command.

`reconcile` observes a retained publication intent; it cannot retry the write.
`cancel` can cancel an unused preparation or an eligible pre-intent job; an
uncertain intent keeps its hold, and cancellation, expiry or a missing worker cannot
release it.

Sources: [web handlers][v2-handlers], [service lifetime and actions][service],
[v2 contract][v2-api].

### Why this guide does not tell you to run step 3

- The v2 contract's own qualification section says full qualification, exact-head
  CI and independent review are open, and that no merge is authorized by it.
- Positive restart recovery on Linux is documented there as still needing execution.
- The held #71 work (a request client, hooks, webhooks) is **explicitly dependent
  on #69's frozen state**, which this guide cannot establish.

If your administrator decides to exercise it anyway, that is their decision,
made against the contract and the then-current CI, not something to infer from this page.

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
| Replace the pair's history with a fresh start | There is no supported operation. Re-anchor is **not implemented**. See [History rewrites and SVN merges](history-rewrites-and-svn-merges.md). |
| Make refresh happen automatically on every parent push | Not provided. Don't script `git rebase`, `git push --force`, or `svn merge` to imitate one. |

## Safe next steps

- If a preview or review shows `both_advanced`-style conflicts, unknowns, or a
  blocked state, treat it as information. Don't try to "clear" it by resetting
  your branch.
- If you see `refresh_execute_not_implemented` or `reanchor_not_implemented`, the
  server did what it should. There is no flag that bypasses it.
- If an executed refresh left a held or uncertain state, go to
  [Status and reconciliation](status-and-reconciliation.md).

[v1]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/repos.rs#L3721-L3880
[core-refresh]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/pair_refresh.rs#L52-L83
[v2-routes]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/pair_refresh.rs#L20-L36
[v2-handlers]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/pair_refresh.rs#L56-L172
[service]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/pair_refresh/service.rs#L25-L25
[v2-api]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/docs/reliability/contracts/69-pair-refresh-v2-api.md
[wiring]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/lib.rs#L412-L412
