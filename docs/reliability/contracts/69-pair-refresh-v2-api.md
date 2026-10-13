# Pair refresh v2 API and retained-job recovery

Refs #69. #69 stays open.

This writer draft adds an explicit v2 service for the bounded execution contract
in [69-pair-refresh-execution-v2.md](69-pair-refresh-execution-v2.md).
The existing v1 preview remains read-only. Its digest cannot approve a v2 action.
This document describes implementation, not completed release qualification or
permission to operate a live installation.

## Prepare and approve

Every call requires a current, enabled database administrator session in the
`Authorization: Bearer <session>` header. Legacy in-memory sessions are refused.
The original actor owns a durable request. A live preparation also belongs to
that exact session and actual running engine.

1. Choose a request ID and retain it across disconnects and retries.
2. Send `POST /api/v2/repos/{child}/refresh/prepare` with
   `{"request_id":"<request>"}`.
3. Inspect the returned `result`. An `approval` contains a preparation ID,
   operation ID, child/parent IDs, approval digest, expiry and `kind`:
   `integration` or `no_op`. A `receipt` returns an existing durable request.
4. Explicitly approve by sending `POST /api/v2/repos/{child}/refresh/action` with
   the same request ID, preparation ID and approval digest, and
   `"action":"execute"`.

The approval expires after five minutes. Preparations use private owned resources
and do not enroll a publication job until execution. Fresh remote observation,
lineage, visibility, credentials, current actor, configuration and writer exclusion
are checked again by the publication pipeline. A no-op has a distinct signed
receipt and performs no SVN commit, Git push or checkpoint update.

There are at most four workers per service. A worker owns its preparation and
supervised commands independently of an HTTP request. Once an approved action
starts, losing the response does not abandon its publication midway through an
external command. Read the durable result with
`GET /api/v2/repos/{child}/refresh/requests/{request}`. Repeating a completed
request returns its original receipt without another publication.

For a live worker, `"action":"reconcile"` independently observes a retained
publication intent. It cannot retry the external write. `"action":"cancel"`
can cancel an unused preparation or an eligible pre-intent durable job. An uncertain
intent retains its hold; cancellation, expiry and a missing worker cannot release
it. Unknown fields and unsupported actions are refused.

## Restart recovery

Recovery begins with
`POST /api/v2/repos/{child}/refresh/recovery/prepare`, using the original request
ID and a current session belonging to its original actor. It returns a new
`kind:"recovery"` approval only after authenticated journal verification,
positive native proof that the prior owner is absent, current writer ownership
and exact retained-resource reconstruction. This preparation does not transfer
the job or publish anything. The next approved action transfers ownership and then
performs only the requested execute, observe or pre-intent cancel operation.
An existing intent must be observed with `reconcile` before execution can continue.

When the ordinary daemon cannot start because the retained job's writer lease
requires authenticated native recovery, an operator can start the binary with
`--config <existing-config.toml> --pair-refresh-recovery`. This mode:

- Holds the existing data directory's local lock and opens an already current
  native WAL database and existing engine Git repository.
- Does not create or migrate the database, checkpoint it on open, seed credentials,
  restore cursors, repair journals, initialize a Git repository or run the scheduler.
- Serves authentication and the recovery/status/action API. Ordinary repository,
  configuration, import and synchronization routes and new refresh preparation
  are absent.
- Obtains a new durable writer epoch only for an authenticated held job after
  native owner-death proof. Normal foreign-host takeover policy is unchanged.

Do not run the ordinary daemon concurrently with this mode. After terminal
recovery, stop recovery mode and restart the ordinary daemon normally. Retained
uncertainty must be investigated; there is no force-adopt or discard action.

Native proof requires the same host, boot, user and PID namespace and a fresh
OS report that the recorded PID is absent. A live or reused PID, unknown boot
identity, foreign host or namespace, copied storage, corrupt history, changed
resources or revoked credentials refuses recovery and preserves evidence.
The macOS test environment currently cannot establish native boot identity;
its restart controls therefore demonstrate refusal and retention. Positive Linux
recovery remains a separate qualification requirement.

## Qualification state

Local tests cover authenticated HTTP boundaries, stored request lookup, actual
SCM publication, caller disconnect during SVN commit, repeat requests, no-op
receipts, restricted recovery serving and storage preservation. Separate ordinary
SyncEngine fixtures cover SVN-origin and Git-origin parent advances and subsequent
child cycles. Actual-engine subprocesses exit at prepared, SVN-write-before-receipt,
SVN-receipt-before-Git, Git-write-before-receipt and completed-checkpoint boundaries.
On macOS, unknown native owner evidence retains active jobs without publication;
the completed request returns its original receipt after restart and the ordinary
child cycle does not publish an echo. Positive native recovery is asserted on
Linux and still needs execution there. Full qualification, required exact-head CI
and independent Grok review remain gates for the draft.
No merge is authorized by this document.
