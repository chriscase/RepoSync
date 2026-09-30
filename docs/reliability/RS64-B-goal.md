# Goal: #64-B — safely reconcile uncertain full-import Git publication without repeating remote writes

Repository: `chriscase/RepoSync`

Start from current merged `main`:

`54e183484961bd9ad12bd03505b3b00610de7cd9`

PR #74 is merged and complete for its bounded #64-A cancellation scope.

Create a **new branch and new draft PR**, preferably:

`feature/import-reconciliation`

Do not continue implementation on the old PR #74 branch.

Issue: #64  
Parent epic: #61

---

# Objective

Implement the next small, mergeable #64 increment:

> An administrator can explicitly reconcile a held per-repository full-import operation when a Git publication may already have succeeded, using the durable operation record plus read-only verification of the actual remote ref.

The reconciliation action must **never repeat the uncertain push**.

It may advance durable confirmation/checkpoints only when the existing external effect is proved to be exactly the effect RepoSync previously intended.

This slice is intentionally narrower than general #64 recovery.

---

# Existing accepted foundation

PR #74 established and merged:

- durable exact per-repository import operation identity;
- `queued`, `running`, `cancel_requested`, `cancelled`, `completed`, `failed`, and `reconciliation_required` states;
- persisted local and confirmed SVN/Git positions;
- durable `intended_ref` and `intended_git_sha` before publication;
- exact-operation status and cancellation APIs;
- scheduler/competing-writer holds;
- restart preservation;
- supervised import subprocess cancellation;
- explicit uncertainty when external or process outcomes cannot be proved;
- reset/reimport refusal;
- mounted UI coverage.

Retain those semantics and tests.

Do not redesign #64-A.

---

# Scope

Implement **explicit read-only reconciliation of held Git publication/finalization states**.

The preferred workflow is:

1. Admin opens a `reconciliation_required` import operation.
2. RepoSync displays the durable recorded evidence.
3. Admin selects **Verify remote** / **Reconcile**.
4. RepoSync performs read-only local/remote verification.
5. It either:
   - proves the previously intended publication already exists and records that proof;
   - proves a fully imported final tip and atomically completes the existing operation/checkpoint;
   - or leaves the operation held with a precise reason.

No Git push is issued by reconciliation.

No SVN write is issued by reconciliation.

---

# API

Add an exact-operation route such as:

`POST /api/repos/{repo_id}/import/{operation_id}/reconcile`

Requirements:

- exact repository ID and operation ID;
- admin authorization for this first slice;
- no legacy session fallback when named users exist;
- operation must still be the repository's active held operation;
- stale operation IDs cannot reconcile a newer operation;
- idempotent when the same already-proved state is checked again;
- request must never perform an external write.

Return a structured result describing:

- operation ID;
- previous lifecycle;
- resulting lifecycle;
- recorded intended ref/SHA;
- observed remote ref/SHA;
- local recorded tip;
- confirmed tip;
- whether publication was proved;
- whether final repository checkpoint completion was proved;
- remaining reason/next action.

Do not expose credentials.

---

# Case A — outstanding publication intent exactly matches the remote

Example durable state:

- state = `reconciliation_required`;
- `intended_ref = refs/heads/main`;
- `intended_git_sha = G`;
- local journal proves G was the intended local import tip;
- remote read reports exactly G.

Required behavior:

1. Revalidate target fingerprint/config before interpreting the result.
2. Verify the exact configured remote/ref.
3. Verify the observed full SHA equals the durable intended full SHA.
4. Verify applicable local Git evidence still contains that object/tree.
5. Record publication confirmation **without pushing again**.
6. Clear the outstanding publication intent only in the same checked local transaction that records confirmation.

Do not rely on:

- short SHA;
- commit message;
- branch name alone;
- local HEAD alone;
- cached remote-tracking refs.

Use a fresh remote read.

If any required evidence disagrees, remain held.

---

# Case B — uncertain finalizer after publication was already confirmed

PR #74 already exercises failures where the Git ref was successfully published but repository checkpoint finalization failed.

For a held operation where:

- no publication intent remains;
- `last_confirmed_git_sha == last_local_git_sha`;
- `last_confirmed_svn_rev == last_local_svn_rev`;
- all expected imported revisions were processed;
- the fresh remote ref still equals the recorded confirmed Git SHA;
- target fingerprint/config still matches;

allow the reconciliation transaction to perform the same final local completion that the original finalizer would have performed:

- update repository per-repo checkpoint;
- update legacy per-repository cursor copies;
- mark operation `completed`;
- remove the active hold.

This must be one checked local transaction using the existing completion rules where possible.

Do not invent a second completion algorithm if the existing `complete_import_operation` logic can be safely reused or factored.

---

# Case C — final publication intent was uncertain, but it is now proved

If Case A confirms an outstanding intent and that publication represents the complete final imported tip, reconciliation may then proceed directly into Case B's checked local finalization in the same request.

Required journey:

```text
push succeeds
→ success response/receipt path is lost
→ operation becomes reconciliation_required
→ daemon restarts
→ admin chooses Verify/Reconcile
→ RepoSync reads remote only
→ exact intended SHA is found
→ publication receipt is reconstructed from verified evidence
→ complete final tip is proved
→ repository checkpoint is finalized once
→ operation becomes completed
→ ordinary scheduler can continue
```

There must be **no second push**.

---

# Partial-batch boundary

Do not overreach.

If a previously uncertain publication is proved but the operation has not imported all SVN revisions, this slice may:

- safely record that particular publication as confirmed;
- preserve the operation as held;
- present an explicit state such as:
  `publication confirmed; partial import resume is not implemented in #64-B`.

Do **not** automatically restart the import from the next revision in this slice.

Safe partial resume is a later review boundary.

Do not clear the active hold for an incomplete import.

---

# Mismatch and failure rules

All of the following remain held and must not change repository checkpoints:

### Remote ref missing

Recorded intent exists but fresh remote lookup says the ref does not exist.

Result:

`reconciliation_required`

No push.

### Remote ref points somewhere else

Observed SHA != intended or confirmed SHA.

Result:

`reconciliation_required`

Report expected and observed identities safely.

No reset, force push, delete, or retry.

### Remote advanced beyond the intended commit

Do not assume ancestry means success.

For this bounded slice, exact equality is required to certify the previously issued publication.

If the remote points to a descendant or unrelated commit:

`reconciliation_required`

Later work can define multiwriter/external-advance recovery.

### Authentication or transport failure

Do not convert inability to inspect the remote into absence.

Remain held.

### Local expected object/tree missing

Remain held.

Do not reclone or synthesize the missing object.

### Target configuration/fingerprint changed

Remain held.

Do not reconcile an operation against a newly configured endpoint.

### Unsupported or malformed legacy operation record

Fail closed.

Preserve it.

---

# Durable state rules

Do not mutate a terminal `cancelled`, `failed`, or `completed` operation into another terminal outcome through this endpoint.

The endpoint is for the exact active reconciliation hold.

Any newly necessary operation-state fields must remain compatible with existing version-1 operation JSON.

Prefer additive serde-default fields rather than schema-version churn unless genuinely necessary.

Ordinary SQLite startup must remain user_version 12.

Do not activate candidate v13/v14 migration work.

---

# Read-only external verification

Reconciliation must not issue:

- `git push`;
- force push;
- remote delete;
- SVN commit;
- SVN propset;
- branch creation;
- remote checkout mutation.

External operations during reconciliation should be inspection only.

Fresh Git verification should use the exact remote target and ref, for example a supervised `ls-remote` equivalent.

All inspection subprocesses need the existing timeout/process supervision.

Inspection failure is not proof of target state.

---

# User experience

Extend the existing import card only enough to make the held state actionable.

For a `reconciliation_required` operation:

Show:

- exact operation identity;
- local imported-through revision/SHA;
- remote-confirmed-through revision/SHA;
- outstanding intended ref/SHA when present;
- outcome reason;
- a clearly named admin action such as **Verify remote**.

After verification:

### Fully reconciled final operation

Show:

`Import completed after remote verification`

and ordinary completed state.

### Publication proved but import partial

Show something equivalent to:

`Publication verified. Import is still partial and remains held; safe resume is not yet implemented.`

### Mismatch/unverifiable

Show:

`Reconciliation still required`

with useful non-secret reason.

Never label remote verification as a retry.

---

# Required real-path regressions

Use the existing disposable SVN/Git/API harness.

Add exact required cases for at least:

## 64B_LOST_REPLY_COMPLETE

Reproduce the existing lost-push-reply condition.

Before reconciliation:

- remote ref already equals intended SHA;
- operation is reconciliation_required;
- repository checkpoint is not falsely completed;
- intent remains recorded.

After explicit reconcile:

- no second push occurred;
- fresh remote read proves exact SHA;
- confirmation recorded once;
- final checkpoint committed once;
- operation completed;
- active hold removed;
- next scheduler/sync behavior is ordinary.

Re-run reconcile or status and prove idempotence/no duplicate effects.

## 64B_FINALIZER_RECOVERY

Reproduce the existing finalizer/checkpoint persistence failure after publication confirmation.

After restart:

- exact remote ref still equals confirmed SHA;
- reconcile performs no remote write;
- existing final local completion succeeds;
- operation completed;
- checkpoint equals verified imported tip;
- no mapping/commit duplication.

## 64B_REMOTE_MISSING

With an outstanding intent, remove or omit the disposable remote ref.

Reconcile:

- remains held;
- checkpoint unchanged;
- intent retained;
- no push.

## 64B_REMOTE_MISMATCH

Move disposable remote ref to a different commit.

Reconcile:

- remains held;
- reports mismatch;
- checkpoint unchanged;
- no destructive correction.

## 64B_REMOTE_ADVANCED

Make the intended SHA an ancestor of a newer remote SHA.

Exact equality requirement rejects automatic confirmation.

No write.

## 64B_INSPECTION_FAILURE

Deterministic auth/transport/timeout inspection failure.

Remain held; no absence assumption.

## 64B_CONFIG_CHANGED

Change relevant repository target configuration after the operation was created.

Reconciliation refuses due to fingerprint mismatch.

No remote write.

## 64B_PARTIAL_CONFIRMED

Create a multi-batch or otherwise incomplete import with an uncertain publication that can be proved.

Reconcile that publication but keep the partial import held.

Do not automatically resume.

## 64B_STALE_ID

A stale operation ID cannot alter the active operation.

## 64B_UI

Mounted existing component against the real fixture API:

- reconciliation-required state;
- click Verify remote;
- proved complete state;
- mismatch/unverifiable held state;
- partial verified-but-held state.

No new frontend framework.

---

# Prove absence of duplicate writes

For recovery-positive cases, record before/after:

- remote commit count;
- remote ref SHA;
- remote tree;
- operation publication counters;
- mappings;
- repository checkpoint;
- per-repo legacy cursor copies;
- audit or command trace.

Explicitly assert:

- zero `git push` commands during reconciliation;
- zero SVN commits;
- no duplicated Git commit;
- no duplicate mapping;
- no checkpoint leap beyond verified evidence.

---

# Restart boundary

At least the positive lost-reply and finalizer-recovery cases must:

1. produce the reconciliation hold;
2. stop/discard the original web worker/process context;
3. reopen the same v12 database and managed Git checkout;
4. invoke reconciliation through the restarted API;
5. obtain the correct result.

Do not prove recovery only in the original in-memory process.

---

# Concurrency boundary

This slice still assumes one daemon owns the data directory.

Do not implement cross-host locking.

Within one daemon, however:

- reconciliation for a repository must acquire the same managed-resource exclusion needed to prevent sync/import/delete from writing concurrently;
- unrelated repositories must remain usable;
- two reconciliation requests for the same exact operation must serialize/idempotently converge;
- scheduler/manual sync must remain held until reconciliation actually completes.

Add a bounded race regression for reconcile vs scheduler/manual writer.

---

# Preserve #64-A

Retain all 110 required reliability cases from merged PR #74.

Do not weaken:

- cancellation semantics;
- process supervision;
- uncertainty classification;
- reset refusal;
- exact-operation identity;
- scheduler hold behavior;
- UI requested/stopping state;
- v12 startup.

The known #73 conflict case remains uncovered unless separately fixed. Do not absorb it here.

---

# Qualification

Run:

- format;
- strict workspace Clippy;
- workspace tests;
- UI lint/build;
- relevant mounted browser journey;
- E2E;
- exact reliability manifest;
- matched comparison against merged base `54e183484961bd9ad12bd03505b3b00610de7cd9`;
- evidence scanning.

Preserve the pinned dependency graph unless a necessary dependency change is independently justified.

A new dependency should be treated as review-significant.

---

# Git workflow

Create a new branch from exact merged main:

`54e183484961bd9ad12bd03505b3b00610de7cd9`

Suggested:

`feature/import-reconciliation`

Open a **draft PR to main**.

Use ordinary commits.

Suggested separation:

1. reconciliation state/API core;
2. verification/finalization integration;
3. real-path recovery regressions;
4. small UI action/browser evidence;
5. documentation/handoff.

No force push.

Do not modify PR #74.

---

# Out of scope

Do not implement:

- automatic background reconciliation;
- automatic partial-import resume;
- arbitrary Git divergence repair;
- cross-host fencing;
- general SVN-commit recovery;
- general sync-operation journal conversion;
- v13/v14 operational migration;
- reset/reimport recovery;
- #73 conflict fix;
- release/deployment/live upgrade.

Those remain future reviewed increments.

---

# Required handoff

Return `READY_FOR_REVIEW` with:

- new PR number and branch;
- base SHA;
- functional/final head SHA;
- tested synthetic merge/tree identity;
- exact new required case IDs;
- retained case count;
- test and comparison totals;
- positive lost-reply and finalizer-recovery state proofs;
- negative mismatch/missing/advanced/config/inspection proofs;
- explicit command trace proving zero remote writes during reconciliation;
- restart proof;
- race proof;
- mounted UI evidence;
- artifact IDs and independently recomputed SHA-256s;
- remaining #64 boundaries.

Stop at the independent review gate.

Do not merge without review.