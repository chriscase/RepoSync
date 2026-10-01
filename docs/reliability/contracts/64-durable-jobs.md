# #64 contract: durable jobs, cancellation, external-write recovery

**Status:** design/contract for remaining work after merged #64-A (PR #74) and
#64-B (PR #75). This PR does not add a general operation service.

**Depends on:** #62 catalog and the #63 logical checkpoint meanings.
**Does not replace:** [import-cancellation.md](../import-cancellation.md),
[RS64-B-reconciliation.md](../RS64-B-reconciliation.md).

## Already shipped (do not reimplement)

| Slice | What it is | What it is not |
| --- | --- | --- |
| #64-A | Per-repository **full-import** journal in v12 `kv_state`, exact operation ID, authorized cancel/status, supervised children, scheduler hold, UI Stop | General sync/pair/refresh/delete jobs |
| #64-B | Explicit admin `POST .../import/{id}/reconcile` that read-only `ls-remote`s the exact intended Git SHA and may finalize | Automatic recovery, SVN write recovery, resume of remaining revisions |

Normal schema stays v12. Import documents are operation records, not lineage.
Cancellation means **stop safely**, not undo published history. A lost push
reply stays `reconciliation_required` until remote proof.

## Required general operation record

When a later slice adds jobs beyond full import, each row must include:

- durable operation ID and idempotency/request key
- authorized initiator
- type (`full_import` already exists; later: `team_sync`, `pair_init`,
  `refresh`, `snapshot_import`, `managed_remove` — each separately reviewed)
- pair ID **and generation** once #63 generations exist; until then the
  existing repository ID plus the import target fingerprint
- pinned SVN/Git tips and policy fingerprint
- phase, owner lease/instance, cancel request, timestamps
- last verified checkpoint, intended external effect, terminal outcome

Suggested states, already used by import: `queued`, `running`,
`cancel_requested`, `cancelling`, `completed`, `cancelled`, `failed`,
`reconciliation_required`. Terminal states are immutable except by an
explicit new recovery operation. Stale cancel of operation A must not cancel
successor B.

Allowed transitions stay those in [import-cancellation.md](../import-cancellation.md).
Any uncertain external effect → `reconciliation_required`. Recovery inspects
actual remotes before repeating a write.

## Writer and subprocess rules

- One writer per shared managed resource (scheduler, manual sync, import,
  delete, pair init, refresh, duplicate requests). Unrelated repos stay
  independent. Single-daemon lockfile enforcement is acceptable.
- Persist intent **before** each remote side effect.
- After SVN commit or Git push, re-query the remote even on timeout/error.
  Compare exact ref parent/tree or SVN UUID/path/revision/tree plus the
  durable operation identity. Messages and final-tree equality are insufficient.
- Supervise children with declared timeouts, process-group termination, and
  reap. Uncertain cleanup cannot be treated as “nothing happened.”
- Missing target, auth failure, or remote deletion fail closed without
  clone/init/reset/reimport or checkpoint advance.
- A cancelled partial import must not become a complete pair for the ordinary
  scheduler (already true for #64-A holds).

SQLite transactions cannot make SVN, Git, and the DB one atomic unit.

## Smallest next implementation slice (after this review): #64-C

Mirror #64-B for **one Git→SVN commit** in the team engine, nothing else.

1. Before `svn commit`, persist operation ID, pair/repo identity, source Git
   SHA/parent/tree, target SVN UUID/path, pre-write revision/tree, projection,
   and intended effect.
2. If the commit is accepted but the reply or local checkpoint write fails,
   leave `reconciliation_required`.
3. Recovery is an explicit, authorized, read-then-maybe-finalize path:
   re-read the exact SVN target; accept only a unique match on
   UUID/path/revision ancestry/changed paths/tree and the durable identity.
4. If the effect is absent and the pre-write target is unchanged, the worker
   may resume that one planned write. Conflicting or unavailable evidence
   stays held. No blind retry, no reimport, no checkpoint reset.

Out of scope for #64-C: automatic background recovery, cross-host fencing,
partial-import resume, general team-sync journal for the whole cycle, refresh,
and pairing.

## Required tests (reuse the #62 harness)

Deterministic barriers, not timing-only sleeps:

- cancel before start / during connect / per-commit / batch publish /
  verification / final publish (import cases exist; team-sync cases do not)
- crash before and after SVN commit and Git push and DB checkpoint
- success response lost
- cancelled partial import then scheduler tick (exists)
- two independent imports (partially exists)
- same remote target registered twice (exists as refuse-existing-target)
- process-tree cleanup (exists for import)

Assert no duplicate revisions/commits, no lost mapping, no write after
confirmed terminal quiescence, and truthful status. Uncertain outcomes remain
`reconciliation_required`, never PASS or “rolled back.”

## Remaining #64 boundaries

Cross-host fencing, automatic reconciliation, held-import resume, and a
general operation journal for sync/pair/refresh stay later reviewed slices.
#73 conflict behavior remains uncovered.
