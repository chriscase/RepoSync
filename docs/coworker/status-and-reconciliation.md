# Status, and responding to "reconciliation required"

*Back to the [guide index](README.md).*

## Reading the status

Any signed-in user can read a repository's status:

```bash
curl --fail-with-body -sS -H @"$HDR" "$BASE/api/status?repo_id=REPO_ID"
```

The reply keeps older fields (`state`, `last_sync_at`, `last_svn_revision`,
`last_git_hash`, error and conflict counters) and adds a version-1
**`observation`**. Read the `observation` first:

| Field | Meaning |
| --- | --- |
| `health` | One of `idle`, `running`, `initializing`, `busy`, `disabled`, `paused`, `conflict`, `held`, `error`, `unknown`. |
| `configured` | Whether the repository is enabled, disabled or unknown. Enabled does **not** mean healthy. |
| `observed_at` | When this snapshot was taken. |
| `last_successful_sync_at` | The last *recorded* success, or unknown. |
| `remote_freshness` | Reads `not_verified_by_status`: status is a database read, **not** a new look at Git or SVN. |
| `evidence_complete` | `false` means some facts could not be established; do not treat missing as zero. |
| `operations[]` | Durable operations (import, SVN commit, Git push, late pair, removal, …) each with `kind`, `state`, `held`, and, when you may see them, `operation_id`, `reason` (redacted), `target_ref`, `intended_git_sha` and `confirmed_svn_revision`. |
| `safe_next_action` | A plain-words hint. When status is unavailable it says that this is *not* permission to retry, skip or release a hold. |

Visibility: an operation's identifiers and details are shown to its
**initiator or an administrator**; history-block detail (the tips involved) is
administrator-only. Someone else sees the operation as held without its details.

A few cautions the source is explicit about:

- `unknown` is not `idle`. A failed or missing status read shows as unknown even if an old snapshot was green.
- A busy slot, a running job and a terminal outcome are different facts. A *cancelled* or *failed* import can still carry a hold.
- **Skip, Retry and error acknowledgement do not prove that anything was reconciled and do not release a hold.**
- Status never repairs, backfills or migrates anything.

Sources: [`get_status`][get-status], [observation types][obs], [truthful-status contract][c116].

### Looking at one specific operation

Which route you can use depends on the kind of operation, and **who may call it
differs by route** (verified in the handlers, not in older docs):

| Operation | Action | Route | Who |
| --- | --- | --- | --- |
| Import | Read status | `GET /api/repos/{id}/import/status` | initiator or admin |
| Import | Cancel | `POST /api/repos/{id}/import/{op}/cancel` | initiator or admin |
| Import | Re-inspect (reconcile) | `POST /api/repos/{id}/import/{op}/reconcile` | admin |
| Import | Resume | `POST /api/repos/{id}/import/{op}/resume` | admin |
| Git → SVN commit | Read status | `GET /api/repos/{id}/svn-commit/{op}` | **admin** |
| Git → SVN commit | Re-inspect (reconcile) | `POST /api/repos/{id}/svn-commit/{op}/reconcile` | admin |
| SVN → Git push | Read status | `GET /api/repos/{id}/git-push/{op}` | **admin** |
| SVN → Git push | Re-inspect (reconcile) | `POST /api/repos/{id}/git-push/{op}/reconcile` | admin |
| Late pair | Status, resume, cancel | `POST /api/repos/{parent}/branches` with `"action"` and `"reconciliation": true` | the same admin who started it |
| Pair refresh (v2, draft) | Read receipt | `GET /api/v2/repos/{pair}/refresh/requests/{request}` | the original actor |
| Pair refresh (v2, draft) | Reconcile or cancel | `POST /api/v2/repos/{pair}/refresh/action` with `"action"` | the original actor |

A reconcile call takes the repository's busy slot, waits briefly for it (about ten
seconds), and otherwise answers "repository is busy; retry remote verification
later". It is **observe-first**: it looks at the remote and records what it can
prove, and it finalizes only on a unique verified match. It does not blindly retry
the original write (a verified partial import authorizes a later resume but does
not auto-resume). For SVN-commit and Git-push operations the reply's `observed.kind`
is one of `unique_match`, `absent_unchanged`, `conflict` or `unavailable`, next to
`publication_proved`, `checkpoint_completed` and `may_resume`. `conflict` and
`unavailable` are not good news; stop and escalate.

Sources: [import status authorization][import-status], [import cancel and reconcile][import-routes], [svn-commit and git-push][commit-routes], [route table][route-table].

## What "reconciliation required" means

`reconciliation_required` means: **an external effect may have happened, and
RepoSync could not prove whether it did.** Examples are a Git push whose result
was never confirmed, an SVN commit whose revision was not recorded, or published
history that no longer descends from the last point RepoSync handled. RepoSync
stops that repository and keeps the evidence, rather than guess and risk
publishing something twice or losing something.

The hold survives restarts. A restart does not resume it, and the scheduler does
not treat it as complete.

### What you do

1. **Stop pushing to that branch.** More commits on top make the comparison harder.
2. **Read the status** (above) and note the `operation_id`, `target_ref`,
   `intended_git_sha`, `confirmed_svn_revision` and `reason`. If you are not the
   initiator or an administrator, you will see only that it is held. Ask someone who can.
3. **Preserve your own evidence.** Record the SHA of your last good local tip
   (for example `git rev-parse HEAD` and `git reflog -5`), and create a local
   backup ref (`git branch backup/before-hold`) if you had unpublished work.
4. **Tell an administrator** the repository ID, branch, operation ID and what you
   did last. They decide whether and how to ask RepoSync to re-inspect.
5. **Wait for the re-inspection result.** Re-inspection compares RepoSync's
   recorded intent with the remote and records what it can prove. For an import
   it reports whether publication was proved and whether it may resume. It can
   still end in `reconciliation_required`; if it does, stop and escalate.

### What you do not do

- Do not force-push, delete the remote branch or tag, or recreate it.
- Do not edit RepoSync's SQLite database, kv state or checkpoints.
- Do not run `svn merge`, `svn copy`, `svn delete` or `svn commit` against the
  paired branch to "line it up".
- Do not use the setup wizard's "Reset & Reimport" to "fix" a held repository.
  It deletes RepoSync's local Git clone and force-pushes a fresh import to the
  configured branch.
- Do not try to "clean up" a held pair with the legacy `DELETE /api/repos/{id}/branch-pair`.
  Its handler *interprets* an omitted `delete_git` or `delete_svn` as `true` when
  `explicit_remote_deletion_opts=true` is not given, and the reply's `remote_deletion`
  object echoes those requested flags. It does not delete anything remote: the
  managed-removal guard answers **HTTP 409** (`state: "blocked"`, `review_required: true`,
  `remote_git` and `remote_svn` both `"untouched"`) before any remote deletion or local
  cleanup. Remote deletion requires the separately reviewed `/removal/review` then
  `/removal/approve` flow; passing explicit `false` flags requests only RepoSync's own
  local managed removal. Removing a pair is not a repair for a hold. Details:
  [Branch policy](branch-policy-and-hooks.md#legacy-branch-pair-delete-interpreted-defaults-versus-actual-refusal)
  and [docs/reliability/65-managed-removal-closeout.md](../reliability/65-managed-removal-closeout.md).
- Do not treat "the status page is green again" as proof. Status is a read of recorded state.

Sources: [import reconcile][import-routes], [flag interpretation][delete-flags], [managed-removal guard][remove-guard],
[reset & reimport][reset-reimport], [RS64-B reconciliation][rs64b].

## Why a pair gets held

When polling finds the remote Git branch in a state RepoSync cannot prove safe,
it **records a durable hold before it resets its checkout or writes to either
remote**. Git and SVN are left untouched. These are the reasons, as used by the
history gate. The gate compares four Git values: **P** (the last point RepoSync
durably handled), **O** (the remote tip it last observed), **R** (the remote tip
it has just fetched) and **L** (its own bridge checkout).

| Reason | Meaning in plain words |
| --- | --- |
| `non_fast_forward` | P is not an ancestor of R: the published history was replaced (a force push, a rebase of pushed commits, an amend). **Durable.** |
| `observed_remote_rewrite` | The previously observed remote tip O is no longer an ancestor of R. **Durable.** |
| `unproven_pending_range` | P is not an ancestor of R, so the pending range cannot be proven. **Durable.** |
| `unsupported_merge_dag` / `unsupported_backlog` | The pending graph cannot be ordered safely, or a legacy block of that kind. **Durable.** |
| `unpublished_local_history` | RepoSync's own bridge checkout (L) holds commits outside the P→R path. |
| `local_dirty`, `local_status_error`, `unknown_local_tip` | RepoSync's bridge checkout has unpublished or unreadable state. |
| `ambiguous_checkpoint`, `missing_checkpoint`, `missing_checkpoint_object` | The recorded checkpoint P is unusable. |
| `incomplete_history` | The fetched clone is shallow. |
| `remote_branch_missing` | The remote branch is gone. Absence is not permission to recreate it. |
| `remote_auth_failed`, `remote_transport_failed`, `remote_fetch_failed` | Access or transport problems, deliberately distinct from "missing". |
| `inspection_object_missing`, `remote_changed_during_inspection` | The evidence was incomplete or the remote moved mid-check. |
| `ignored_path_collision`, `invalid_branch` | Incoming files would overwrite ignored local paths, or the configured branch name is invalid. |
| `ancestry_command_failed` | The ancestry command failed with an exit code other than 0 or 1. **Not** a claim that history was rewritten. |

The rows marked **Durable** persist across restarts and polls, even if a later
fetch looks fine. The remaining rows are fail-closed refusals of this inspection
that report their reason; the contract says which ones persist, and this table
does not guarantee more than that.

Sources: [#66 contract][c66], [reason strings][reasons], [durable-hold persistence][durable].

### If you caused it

If your own rewrite of a paired branch triggered `non_fast_forward`:

- Do not push anything else to that branch, and do not try to "undo" it with
  another force push.
- Keep the old tip: `git reflog show feature/widget` and make a local branch at
  the previous tip.
- Tell an administrator what the old and new tips are. They can compare these
  with the P, O, R and L values on the history-block detail, which only they can see.

**We found no authenticated API in the pinned tree that releases a durable
history hold, and this guide does not document one.** Whether and how a hold is
released is an administrator and maintainer decision made with the contract in
hand; it is not something a developer can or should engineer around.

[get-status]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/status.rs#L127-L236
[obs]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/status_observation.rs#L9-L75
[c116]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/docs/reliability/116-truthful-status.md
[import-routes]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/repos.rs#L2209-L2368
[commit-routes]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/repos.rs#L2702-L2800
[route-table]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/repos.rs#L308-L383
[delete-flags]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/repos.rs#L3952-L3969
[reset-reimport]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/setup.rs#L1376-L1385
[rs64b]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/docs/reliability/RS64-B-reconciliation.md
[c66]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/docs/reliability/contracts/66-history-containment.md
[reasons]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/history_inspect.rs#L1-L30
[durable]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/core/src/history_inspect.rs#L109-L176
[remove-guard]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/repos.rs#L948-L955
[import-status]: https://github.com/chriscase/RepoSync/blob/ade6f9cb383f76d65c6610ef21eec2199f6c23b5/crates/web/src/api/repos.rs#L2070-L2095
