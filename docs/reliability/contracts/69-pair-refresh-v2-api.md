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

## Authenticated inspection-only journey

The following separate surface implements review, not the preparation/approval
flow above. Source qualification for this addition is pending. It never changes
`execution_eligible:false`, creates an approval or durable request, or admits
changed-child execution. AC7 re-anchor remains **NOT_IMPLEMENTED**. Authentication
authorizes viewing only; conflict choices remain untrusted inspection intent.

Use a current enabled database administrator session against the actual running
engine. A legacy memory-only session is insufficient. The service rechecks the
same actor/session and effective policy after configured-source reads. The
recovery-only server does not expose these two routes.

1. Request `POST /api/v2/repos/{child}/refresh/review` with the strict empty JSON
   object `{}`. No actor, baseline receipt, mapping, endpoint or filesystem root
   may be supplied by a client.
2. Inspect `pending.child` and `pending.parent` independently. Each contains
   registered Git, SVN and local-bridge observations. `pending_count:null` is
   unknown, not zero. Git advertisements that differ from handled checkpoints
   report their observed tips but do not invent a count, ancestry or mapping.
   SVN observations list visible branch-incarnation revisions after the stored
   checkpoint; they do not subtract repository revision numbers. An empty
   visible list alone never sets `complete:true`: that flag requires a
   `visibility` pin from the existing opaque correspondence factory's fresh
   isolated full WC31 checkout and empty-ancestor proof, bound to this
   registered role/URL/UUID/root, inspected revision and
   handled checkpoint. Its maximum changed revision must not exceed the
   checkpoint. Visible unhandled SVN work remains blocked because remaining visibility/correspondence
   has not been proved for it. Unrelated repository revisions are not pending
   branch changes merely because HEAD advanced.
3. Check the tagged `state`. `blocked` includes explicit role/source reason
   codes and has no review pin, applicability or candidate. Missing, foreign or
   linked local bridges, unfinished operations, extra local branches, suppressed
   tracked entries (`assume-unchanged` or `skip-worktree`), sparse policies/index
   directories, unmerged stages, dirty or ignored/untracked files, local tip differences, unreadable history and
   observations exceeding bounds do not become an empty-work assertion.
   Local tracked files are independently opened as regular no-follow files and
   compared byte-for-byte by Git blob identity and executable mode against the
   already checked index, before and after status. Neither a stable index digest
   nor stat-cache/status suppression can substitute for these reads. Filters,
   symlinks and transformations are not normalized into a clean result.
   Retain and investigate the work; this API does not drain, synchronize, reset,
   repair or discard it. This is deliberately a conservative supported subset.
4. For `state:"ready"`, inspect the fresh `applicability` manifest and its exact
   P1/R/C/P2 identities, history/mapping/configuration/projection digests, both
   side deltas, conflicts and unverified gates. Those deltas describe changes
   since the historical receipt; they are distinct from currently unhandled
   work. The existing supported topology and M1 refusal rules remain unchanged.
5. Construct an exact conflict-choice request for
   `POST /api/v2/repos/{child}/refresh/review/candidate`. The strict envelope is:

   ```json
   {
     "review_pin": "<ready response review_pin>",
     "resolution": {
       "schema": 1,
       "policy": "issue69_live_bound_resolution_candidate_review_v1",
       "manifest_digest": "<applicability.manifest_digest>",
       "choices": [
         {
           "path": "<exact conflict path>",
           "baseline": "<copy the exact baseline entry object>",
           "child": "<copy the exact child entry object>",
           "parent": "<copy the exact parent entry object>",
           "choice": "child"
         }
       ]
     }
   }
   ```

   The three entry values in this illustration must be the actual JSON objects,
   not strings. Choose `base`, `child` or `parent` consciously for every conflict;
   send each conflict exactly once and no non-conflict/unknown paths. Both
   uncontested deltas remain preserved by the existing resolver. Absence, blobs,
   file modes and directory topology keep their existing distinct identities.
6. A `state:"candidate"` reply contains the freshly recomputed in-memory result
   tree and digest in `candidate`, bound to the exact current applicability
   manifest. It writes no result object/ref, worktree, SVN result, checkpoint,
   publication intent or journal. No `preparation_id`, `approval_digest` or
   durable request is created. A review/candidate pin cannot authorize `/action`.

The wrapper `review_pin` is a separate domain-separated comparison digest over
the actual actor/session-policy fingerprint, registered intake digest and core
manifest digest. It neither changes the core manifest's meaning nor replaces
any execution capability. There is no new TTL. Every submission recaptures the
sources, validates the wrapper and complete conflict triples, and repeats source,
local-bridge, native candidate/mapping and actor checks before returning. It
does not lock remotes or promise atomic freshness after return.

Malformed JSON/unknown fields receive the normal strict-parser refusal. The
inspection API emits fixed, credential/path-free error codes:
`inspection_malformed_request`, `inspection_stale_review`,
`inspection_choices_or_sources_refused`, `inspection_sources_changed`,
`inspection_authorization_changed` or `inspection_unavailable`. Authorization
change is unauthorized; other admitted request refusals are conflicts. On stale
or changed input, request a new review and inspect it again. A blocked report
requires resolving its actual source/work/visibility prerequisite, not resending
an old pin or asking the API to force a result.

### Operator example

On an already admitted installation, use the existing authenticated API client
or a protected shell session. Keep the bearer out of history/logs. For example,
with `BASE`, `CHILD` and `SESSION` supplied securely by the operator:

```sh
umask 077
curl --fail-with-body --silent --show-error \
  -H "Authorization: Bearer $SESSION" -H 'Content-Type: application/json' \
  --data '{}' "$BASE/api/v2/repos/$CHILD/refresh/review" > review.json
```

Inspect both roles, completeness/refusal codes and the exact conflict entries.
Save the consciously selected complete envelope above as `choices.json`. Then:

```sh
curl --fail-with-body --silent --show-error \
  -H "Authorization: Bearer $SESSION" -H 'Content-Type: application/json' \
  --data-binary @choices.json \
  "$BASE/api/v2/repos/$CHILD/refresh/review/candidate" > candidate.json
```

Inspect the intended result and keep the input/result pins with the review.
There is deliberately no execute/approve step in this journey.

### Bounded observation, custody and qualification

Read-only observation can create disposable private Git/authentication scratch
and make configured Git/SVN read requests. It never fetches into shared bridge
refs or updates a local index. Local inspection is limited to the canonical
daemon locations `data_dir/repos/{registered-id}/git-repo`, the one configured
local branch, its index/worktree and ignored/untracked status. Other arbitrary
clones/unreachable objects are not declared enumerated. Missing or unsupported
locations are unknown and block a result. SVN role completeness is independently
derived only after the strict correspondence factory succeeds. Raw intake
always leaves an empty visible pending list incomplete. It may attempt that
factory only when other registered observations are clear; a blocked source
leaves SVN completeness unknown, never presumed. Failed or missing presence
evidence keeps that role incomplete, while actual visible pending revisions
remain reported. A fully proved unrelated repository HEAD advance is not invented
branch work.

Inventory limits are 1,000 visible SVN entries/local status entries/tracked index
entries/choice rows, a 16 MiB index, and 16 MiB cumulative tracked file bytes per
comparison pass (two passes per local observation); these are
refusal bounds, not promises of end-to-end duration, download or executor quotas.
Native transport/parser allocations and command timeouts still require the
separately admitted execution profile and outer resource/deadline controls.

Inspection workers share the existing four-worker service admission budget.
They own their actual futures/handles and scratch after an HTTP disconnect;
their handles join through the existing shutdown collector before successful
shutdown. Receiver loss is not cleanup proof. No worker waits for a later actor
approval, and no caller can convert a read worker into a publication worker.

The new source regressions are **AUTHORED, UNRUN** until exact-head qualification.
Default CI alone does not execute the positive HTTP fixture journey. Its explicit
selection is the `pair_refresh_v2` integration target with the existing
`reliability-fixture` feature on the qualified Unix/Linux profile, specifically
`authenticated_review_http_journey::authenticated_http_review_real_sources_choices_staleness_and_no_approval`.
The feature confines a disposable CA to a supplied test transport client/router;
it must not be enabled as an ordinary deployed trust-policy change. No fixture
script, dependency, global fixture environment mode or production trust store is
changed by this addition. Independent source review and fresh qualified profile,
operator/resource/cancellation/run/deadline binding precede publication/testing.


The completeness correction adds four core regression sources: both-role tracked
suppression/sparse refusal and exact file/index/ref preservation; real svnserve
authz-filtered older history refusing an empty-work claim; fully proved unrelated
HEAD versus real visible pending work; and an actual blocked service response
whose other-source refusal cannot promote hidden SVN history to complete. These
are source assertions only, not executed authz or HTTP qualification. The existing
seven case bodies remain unchanged. The revised profile therefore requires all
**eleven** named cases, including the same separately feature-gated positive HTTP
case. The existing four-role checkouts/ancestor proof are reused, not duplicated in
raw intake. Additional direct tracked reads and the new svnserve regression
fixtures must be included in fresh resource/effects admission. No index flags, sparse settings,
source credentials or server access policy are changed by production inspection;
new authz/index mutations occur only in disposable regression setup.
