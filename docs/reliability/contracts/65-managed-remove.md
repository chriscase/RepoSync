# Managed removal: retained recovery and explicit remote actions

This contract covers whole issue #65: the seven original lifecycle criteria and
four RS-16 requirements. It uses the existing operational schema 13 and KV
journal namespace. It introduces no schema migration, daemon/process controller,
refresh execution, service action or deployment.

## Separate actions

- **Pause / disable** stops admission of new syncing. It preserves registration,
  mappings, credentials, local data and both remotes. Root `DELETE /api/repos/:id`
  remains disable-only; `POST /disable` is the explicit equivalent.
- **Remove from RepoSync** disables the registration, waits for existing ownership
  to become quiet, removes only its confined owned tree and registration/secrets,
  and retains mappings, audit history and a recovery tombstone.
- **Delete a remote Git ref or SVN path** is a separately selected part of a
  reviewed removal. Both choices default to false. SVN path deletion retains SVN
  history; Git ref deletion does not rewrite commits, but does not promise that a
  remote will retain unreachable objects forever.

Children are not cascaded. A parent with registered children is refused. A remote
Git ref shared by another registration or an SVN deletion containing another
registration's target is refused. Sibling paths and other owners' credentials
remain intact. No action invokes reset, force import, checkpoint rollback or
upstream repository deletion.

## Versioned records

Legacy valid version 1 removal documents and their omitted default fields remain
readable. New reviewed operations explicitly use version 2 under the existing
`managed_remove_v1:` namespace. They contain a nested reviewed plan, typed
per-side receipts and independent local cleanup evidence. A v1/v2 blend, future
version, unknown field, duplicate field, wrong owner/type/ID, contradictory
summary or incomplete completion proof refuses. A dangling pointer is an error,
not an absent operation. Existing active, history and tombstone guards remain.

Version 2 plans bind the authenticated actor, request identity, repository and
parent incarnations, configuration and credential generations, dependency
registrations, canonical managed data root, exact selected targets and choices.
Git capture records the endpoint, full ref and expected SHA or positively
observed absence. SVN capture records repository UUID/root, literal path, baseline
revision and positively observed presence/absence. Unavailable tooling, failed
authentication and incomplete observations are not absence.

The review pin is a digest of server-derived evidence. It is neither a bearer
credential nor execution authority on its own. Approval recaptures the inputs;
clients cannot submit their own plan, actor, effect receipt or completed flag.

## HTTP and UI sequence

1. An administrator posts explicit booleans to `/api/repos/:id/removal/review`.
   This is read-only. It returns the exact targets, dependency information,
   retention policy and opaque `review_pin`. Failed or unavailable review blocks
   confirmation. Choice, target or configuration changes require a fresh review.
2. Explicit approval posts `delete_git`, `delete_svn`, `review_pin` and a stable
   `request_id` to `/removal/approve`. The browser retains that request before
   sending it. The server atomically claims the durable identity and disables the
   registration before any destructive native work.
3. Read `/removal` or `/removal?operation_id=...` for the durable outcome. If the
   approval reply was lost before an operation ID was received, use the read-only
   `/removal/request?request_id=...&review_pin=...` lookup. It matches the original
   authenticated actor, repository incarnation, request and pin. Reload never
   sends another approval, continuation or restore automatically. An explicit
   continuation uses the original operation, not a new set of flags.
   `/removal/continue` accepts only its existing ID and same authenticated actor;
   it does not approve different work. Failed local receipt storage retains the
   original pending request for lookup instead of silently discarding it.
4. A completed receipt remains available after the row disappears. Restore via
   `POST /restore` returns a **disabled** registration. It does not recreate a
   deleted remote branch/path or silently enable syncing. An old completed request
   cannot remove the restored incarnation.

Missing/invalid sessions return 401. Valid users without the required role or
operation ownership receive 403; that is not a logout instruction. Bodies reject
unsupported fields. The UI preserves account/repository ownership, original
mutation callbacks and receipts across navigation/reload. It keeps unresolved
request evidence when a response is lost; a different preview cannot replace that
request. A confirmed server rejection permits a fresh review. No browser storage
contains plaintext VCS credentials.

Legacy branch-pair DELETE requests that omit remote flags, or request either
remote action, now receive **409 before effects**, with review/approval guidance.
Their `remote_deletion` metadata describes the requested flags, not effects.
Explicit-false local-only legacy requests retain supported managed removal.
Existing v1 partial receipts remain retained and can retry local cleanup; they
cannot authorize a new remote action. Root DELETE stays non-destructive.

## Effect and cleanup protocol

Each requested side has a durable `prepared` then `intent_recorded` state before
native I/O. Git deletion uses an exact old-tip lease and empty refspec; a prior
lookup followed by unconditional provider REST DELETE is not an equivalent CAS.
SVN uses `svnmucc` with the captured revision and unique operation marker. The
root/UUID and target are checked again. Private command configuration avoids
ambient credential fallback; password input is owner-only before bytes are
written. Supervised commands use the existing bounded process infrastructure.

Successful native results are independently observed and stored through a
non-deserializable producer capability. Per-side labels, evidence digests and
completion agree. Requested and unrequested sides remain separate. Cleanup is
admitted only after all selected sides have verified outcomes, current native
writer/dependency/registration guards pass and owned-path preflight succeeds.
Remote receipts are persisted before local cleanup; a cleanup failure cannot
turn a completed remote action back into `untouched`.

The exact path is `{data_dir}/repos/{repo_id}`. IDs are single safe components;
root/target symlinks and escaped roots refuse before new effects. Nested links
are unlinked rather than followed by the existing confined cleanup. A later
filesystem change or cleanup error retains the registration and receipts.
Cleanup retries recheck ownership and perform no second verified remote action.
Ordered bounded errors remain present even after eventual completion.

An intent without a persisted collected-result receipt is **not** automatically
replayed. Git ref absence does not prove request attribution. An SVN marker may
help inspection but does not prove predecessor collection. Unknown collection
or effect evidence stays held. No PID absence, empty registry, acknowledgement,
reset or journal deletion clears it. The current protocol does not provide an
automatic release for such uncertainty; report the retained operation for
explicit investigation instead of claiming it completed.

Queued/cancelling means work still owns the registration. Running is not a
completed local cleanup. Failed, reconciliation-required, each remote outcome,
cleanup stage and registration visibility are shown separately. HTTP 202 is not
success; held/failed operation receipts use 409. Only a completed durable receipt
supports a removed outcome. Known busy/import ownership remains queued or
cancelling; durable late-pair/refresh/import/replay/publication holds are honored.

## Retention and qualification

Tombstone, audit, mapping and recovery evidence are retained indefinitely. There
is no automatic expiry/purge or advertised trash UI. Protect retained database
and encryption-key backups as before. Restore availability is explicit; it does
not promise that remote objects or paths can be reconstructed.

The three bounded #69 foreign-reader compatibility edits consume strict v1/v2
removal proof only. They do not relax other families, active/orphan/history/
tombstone checks or enable refresh runtime. Future #69 composition must carry
these helpers/consumers and the five accepted #67 dependencies together; frozen
historical source packets remain unchanged.

See `../65-managed-removal-closeout.md` for the whole acceptance map and exact
case inventory. Author-local tests, independent review, exact-head hosted CI and
separate postmerge proof remain distinct gates. #69 native/process/execution/AC7,
#121 reader/usability, main promotion and production holds are unchanged.
