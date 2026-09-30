# #64-B: explicit read-only full-import publication reconciliation

This increment starts at merged main `54e183484961bd9ad12bd03505b3b00610de7cd9` and retains the #64-A cancellation journal and holds. The source brief is copied byte for byte to `RS64-B-goal.md`. It does not enable automatic recovery, general operation handling, or candidate v13/v14 storage. Ordinary SQLite startup remains v12.

## Entry and exclusion

`POST /api/repos/{repo_id}/import/{operation_id}/reconcile` requires a live admin session. Named-user installations do not fall back to a legacy in-memory session. The route acquires the same process-wide per-repository busy slot used by the importer and scheduler before examining the journal or Git checkout. It accepts only the exact active `reconciliation_required` full-import operation. A second request waits for the slot and converges on the completed result without repeating verification or a remote write. An older operation cannot act on a newer active operation. Other repositories retain their own slots.

The operation's original v1 target fingerprint is recomputed from the current repository configuration and managed workdir. The same fingerprint vocabulary is used at initial enrollment and again inside the checked SQLite reconciliation transaction. A changed target remains held. The managed checkout must still exist at the expected path; `origin` must point to the configured Git URL after removing only HTTP userinfo for comparison. The exact local branch must target the durable full SHA, and the commit and tree objects must be readable.

## External proof

Only a supervised, bounded `git ls-remote --exit-code origin <exact-ref>` inspection is issued. Exit 2 means the ref is absent; other nonzero exits and malformed output are inspection failures. One exact full SHA/ref result is required. Its SHA must equal the durable intended SHA, or the already confirmed SHA in a finalizer hold. A descendant, ancestor, or unrelated SHA does not qualify. No cached tracking ref, local HEAD alone, short SHA, commit message, or branch name alone is accepted as publication proof. The response reports recorded and observed identities and a non-secret reason; inspection errors never become evidence of absence.

There is no push, fetch, SVN command, local checkout rewrite, reset, or remote ref update in the reconciliation path. The fixture command wrapper admits only `ls-remote` and records its invocation. The positive tests compare remote ref, tree and commit count, SVN youngest revision, mappings, checkpoint, legacy cursors and publication counters before and after recovery.

## Checked local transaction

The storage method checks the active operation identity, v1 type/state, current target fingerprint and exact configured ref again under one SQLite transaction. It rejects a changed existing repository checkpoint or legacy per-repository cursor. Journal progress must have a positive total, a positive processed count no greater than total, and exactly one durable local commit per processed revision, matching the importer's existing per-repository behavior. A malformed or unsupported record remains held and unchanged.

For outstanding intent, exact observed SHA equals `intended_git_sha` and `last_local_git_sha`. The transaction records the confirmed SVN/Git tip and increments `confirmed_batches` once while clearing both intent fields. For an already confirmed finalizer hold, the same exact remote SHA must equal both durable local and confirmed tips; no batch counter is incremented. If every expected revision is processed, the transaction calls the shared original completion routine to update the repository checkpoint, both legacy cursor copies, operation `completed` state, and active hold atomically. A failed completion rolls back receipt reconstruction as well. If revisions remain, the publication receipt is recorded but the operation stays held with an explicit no-resume reason. A repeat partial verification cannot duplicate its counter.

The endpoint does not rewrite `cancelled` or `failed` operations. A repeat request for the same already completed operation returns its durable completed status without inspecting or writing the remote.

## User interface and evidence

The existing import card shows the operation ID, local and confirmed SVN/Git tips, outstanding intended ref/SHA and outcome reason. An admin can select **Verify remote** only for a reconciliation hold. Completion, mismatch and partial-but-confirmed results stay visible after reload. The existing mounted Chrome fixture uses the actual component and disposable Rust API for all three outcomes.

The isolated manifest retains all 110 #64-A required IDs and adds exact #64-B recovery, negative, authorization, race, local-object, malformed-record and atomic-failure cases. The mounted `64B_UI` browser case is a separate ordinary E2E check because the sealed isolated runtime has no Chrome. The known #73 conflict test remains ignored and uncovered.

## Remaining #64 boundaries

Partial import resume, automatic/background reconciliation, external advance or arbitrary divergence recovery, cross-host fencing, general SVN write recovery, general sync-operation journaling and active v13/v14 migrations remain outside this increment. A held partial operation remains unavailable to automatic sync until a separately reviewed resume path exists.
