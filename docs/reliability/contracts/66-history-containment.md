# #66 contract: rewritten or unproven history before reset/replay

**Status:** team pre-reset gate, hide/push pending selection, linear >1000
continuation, and qualified merge-DAG replay are shipped. Team
`inspect_team_history` classifications are unchanged; durable
`reconciliation_required` blocks now cover rewrite (`non_fast_forward`,
`observed_remote_rewrite`) and UnsupportedHistory (`unsupported_merge_dag`,
`unsupported_backlog`, `unproven_pending_range`), including fetch-time
second-line hits. Personal-mode Git→SVN inspect shares the same
`inspect_fetched_history` gate with proven containment tests. Linear and merge-DAG backlogs continue in explicit oldest-first replay
batches when the full frontier fits in one batch. Merge-DAG backlogs that
exceed the cap fail closed before writes.

**Depends on:** #62 reproductions and the #63 checkpoint meanings.
**Coordinates recovery persistence with:** #64. An ancestry check is not
distributed atomicity.

## Four Git values

| Symbol | Meaning |
| --- | --- |
| P | last durably handled Git checkpoint for this pair |
| O | prior observed remote tip, if any |
| R | freshly fetched exact configured remote-branch tip |
| L | bridge checkout tip before inspection |

`O == R` and `L == R` never establish `P == R`. For `P=A`, `L=O=R=C`,
`A → B → C`, B and C remain pending. `P == R` means only the Git direction
has no new commits; SVN work still runs. No entire-cycle early return follows
from an unchanged Git tip.

## Already shipped (team engine)

`SyncEngine::do_sync_cycle` calls `inspect_team_history` **before**
`fetch_svn_changes` and before destructive `git reset`. Inspection may fetch
into `refs/reposync/inspection/incoming` and write a scoped rejection
audit/status. It must not reset the checkout, adopt a logical checkpoint,
apply SVN/Git changes, or write either remote.

Current classifications (exact reason strings in
`crates/core/src/sync_engine.rs`):

| Reason | Meaning |
| --- | --- |
| `invalid_branch` | configured branch fails `check-ref-format` |
| `unknown_local_tip` / `local_status_error` / `local_dirty` | unpublished or unreadable bridge state |
| `ignored_path_collision` | incoming tree would replace ignored local paths |
| `remote_branch_missing` | `ls-remote --exit-code` status 2 |
| `remote_auth_failed` / `remote_transport_failed` | distinct from missing |
| `remote_fetch_failed` / `inspection_object_missing` | stale tracking ref is not proof |
| `remote_changed_during_inspection` | advertised SHA ≠ fetched SHA |
| `ambiguous_checkpoint` / `missing_checkpoint` / `missing_checkpoint_object` | P unusable |
| `incomplete_history` | shallow clone |
| `non_fast_forward` | handled Git cursor P is not an ancestor of R |
| `observed_remote_rewrite` | prior observed remote tip O is not an ancestor of R |
| `ancestry_command_failed` | exit not in {0,1} — not a rewrite claim |
| `unpublished_local_history` | L outside the P→R path |
| `unsupported_backlog` | legacy durable block reason; linear replay now continues in batches |
| `unsupported_merge_dag` | legacy durable block or frontier that cannot be topologically ordered |
| `unproven_pending_range` | P is not an ancestor of R |

Ordinary qualified linear P→R is admitted and pinned to that R. Later
checkout/reset must use the same object. Pending-commit selection uses a
hide/push ancestry frontier (`P..R`) so a visited-order stop at `P` cannot
omit older pending commits; the proof lives in `pending_frontier` unit tests
(`hide_push_frontier_includes_older_pending_side`), not in
`candidate_r10_merge_dag_older_side_replayed_with_delta` (engine replay with a
Z-side tree delta).

While Git replay continuation is incomplete (`has_more`), the team engine
resets the bridge only through the current batch tip (not the full admitted
`R`), replays that batch Git→SVN, and defers the whole cycle with no bridge
reset or writes when pending SVN work would overlap the not-yet-replayed Git
prefix. Conflict detection uses the full admitted P→R path set before any write.

Durable `reconciliation_required` blocks persist for `non_fast_forward`,
`observed_remote_rewrite`, and UnsupportedHistory reasons via
`record_history_block` / `enforce_durable_history_block`.
They survive process restart and refuse fetch, replay, reset, remote write, SVN
commit, and watermark or mapping change until an explicit operator action clears
them. `fetch_git_changes` records the same durable block when its second-line
`pending_commits_between` check hits UnsupportedHistory.

### Personal-mode Git→SVN inspect

| Area | Status |
| --- | --- |
| P/O/R/L inspect before replay (`inspect_personal_history`) | **shipped** — same classifications as team |
| Rewrite containment (`non_fast_forward`) + durable restart | **proven** — `candidate_r09_personal_rewrite_contained` |
| Qualified linear admission | **proven** — `candidate_r66_personal_linear_history_admitted` |
| Merge-DAG inspect admission | **proven** — `candidate_r66_personal_merge_dag_admitted`, team `candidate_r10_merge_dag_replayed` |
| Legacy UnsupportedHistory (`unsupported_merge_dag`) + durable restart | **proven** — seeded-block `candidate_r66_personal_merge_dag_contained`, `candidate_r10_durable_unsupported_history_block_survives_restart` |
| Observed-remote rewrite (`observed_remote_rewrite`) | **NOT RUN** — team polling cases cover O→R; personal inherits inspect |
| >1000 linear inspect admission | **proven** — `candidate_r66_personal_linear_over_1000_admitted` |
| >1000 legacy backlog durable restart | **proven** — team `candidate_r10_durable_backlog_block_survives_restart` (seeded block), personal `candidate_r66_personal_backlog_block_survives_restart` |
| >1000 team replay continuation + restart | **proven** — `candidate_r10_over_1000_pending_commits_batched`, `candidate_r66_team_history_continuation_survives_restart` |
| >1000 continuation with pending SVN work | **proven** — `candidate_r66_continuation_mixed_pending_fail_closed` (cycle deferred with no bridge reset or writes) |
| Merge-DAG team replay (engine cycle) | **proven** — `candidate_r10_merge_dag_replayed`, `candidate_r10_merge_dag_older_side_replayed_with_delta` |
| Merge-DAG continuation over cap | **proven** — `candidate_r10_merge_dag_continuation_batched` (cap=1/2/3), `candidate_r10_merge_dag_continuation_batched_distinct_trees_cap1` / `cap3`, `candidate_r66_merge_dag_continuation_survives_restart`, `pending_frontier::merge_dag_continuation_batches_without_reordering_or_skips` |
| Durable `has_more` across restart | **proven** — linear `candidate_r66_team_history_continuation_survives_restart`; merge-DAG `candidate_r66_merge_dag_continuation_survives_restart` (`git_replay_continuation_{repo}` pins admitted R and handled set); missing-row idempotency `candidate_r66_merge_dag_continuation_missing_row_idempotent` |
| Drain Git batches past SVN-only commits | **proven** — `candidate_r66_drain_git_batch_past_svn_only` (metadata-only SVN defers only on file-content delta) |
| Conflict detection gating | **proven** — `candidate_r66_incomplete_conflict_coverage_fail_closed` and `sync_engine::tests::incomplete_conflict_coverage_fails_closed`; outer gate compares post-filter `conflict_coverage.len()` against applicable pending commits |
| Personal Git→SVN engine-cycle merge-DAG replay | **NOT RUN** — inspect admission is proven; PR-based replay is a separate surface |

Baseline R09 still demonstrates that the original pull-then-walk path
duplicates rewritten history. Candidate R09 cases reject replacement and
metadata-only amend with zero remote writes, including repeat/reopen.

## Rules that remain binding

1. Fetch into an inspection context. Validate against proven P **before**
   destructive checkout/reset, checkpoint adoption/advancement, conflict
   application, or writes to either remote.
2. Unknown is not “start from zero.” Remote absence is not permission to
   create a replacement repository.
3. Rejected history leaves upstream Git refs and SVN revisions/trees
   unchanged and does not advance directional cursors or add applied mappings.
   Repeated polling, webhook delivery, and process restart perform no
   duplicate replay.
4. Do not declare work already in SVN from messages, timestamps, short SHAs,
   or patch-id equality. Future auto-reconciliation needs its own review.
5. Pending-commit selection must be a correct ancestry/frontier algorithm.
   Qualified merge DAGs replay in deterministic oldest-first topological
   order (parents before children; tie-break by committer time then OID) when
   the full frontier fits in one batch. Linear backlogs over 1000 replay in
   explicit oldest-first batches of at most 1000 commits per cycle; merge-DAG
   backlogs that exceed the cap fail closed before writes because a single Git
   SHA cannot checkpoint a cut through the DAG. The handled Git checkpoint is
   the durable continuation cursor for linear batches only. While continuation is incomplete,
   opposite-direction SVN work with pending Git backlog defers the cycle with
   no mutation and surfaces `deferred_mixed_pending` on `SyncStats`; conflict
   detection sees the full P→R path. Do not silently drop older work.
6. Normal SVN merges append revisions. A revision-number jump is not a Git
   rewrite. Path delete/recreate and UUID/copy-origin change are identity
   events for #63/#15, not this gate’s `non_fast_forward`.
7. Polling is the safety gate. Webhook `forced` is a hint. Re-validate pinned
   targets before publication.
8. Old healthy linear histories continue after migration. Unsupported history
   is reported, not “fixed” by resetting or clearing maps.

## Merge-DAG replay (shipped slice)

**Supported:** any qualified P→R frontier where `P` is an ancestor of `R`.
Pending commits are collected via hide/push (`P..R`), topologically sorted
oldest-first with deterministic tie-breaking. Merge-DAG frontiers replay in
one cycle when `total ≤ cap` (`has_more = false`). Linear backlogs over the
reviewed cap continue in explicit oldest-first batches of at most 1000 commits
per cycle. The handled Git checkpoint advances per confirmed Git→SVN commit and
survives restart. `SyncStats` exposes `git_replay_has_more`,
`git_pending_total`, and `deferred_mixed_pending`.

**Still fail-closed:**

- `unproven_pending_range` when `P` is not an ancestor of `R`
- Legacy seeded durable `unsupported_merge_dag` / `unsupported_backlog` blocks
  (operator must clear before replay resumes)
- Fetch-time `UnsupportedHistory` injection (test fault path)
- A frontier that cannot be fully topologically ordered (should not arise for
  valid Git objects)
- Merge-DAG overflow when `total > cap` (`has_more = true`; a single Git SHA
  cannot checkpoint a cut through the DAG)

Automatic rewrite reconciliation stays a later slice with its own proofs.

## Required tests

Already present: ordinary fast-forward; unchanged tip with new SVN work;
L=R lagging P; rewrite of already-synced work; metadata amend; missing
branch/object/shallow; ancestry-command error; merge-DAG replay when the full
frontier fits in one batch (older-side hide/push frontier vs visited-order skip
in unit tests); merge-DAG continuation over cap fail-closed; >1000 linear
continuation with restart and mixed-pending deferral; ignored-path preservation; repo scoping; durable
block survives restart for rewrite, observed-remote rewrite, UnsupportedHistory
(merge DAG and legacy backlog); fetch-time UnsupportedHistory persistence;
personal-mode rewrite and merge-DAG containment with durable restart
(inspection/tracking refs retargeted to restored tip before reopen).

Still required before claiming #66:

- `unproven_pending_range` engine-level durable restart (deferred: inspect
  P→R ancestry gate rejects the same shapes first; unit coverage lives in
  `pending_frontier::non_ancestor_tip_fails_closed`)
- equal-looking content with different provenance (already partly R09 amend)
- SVN mergeinfo-only versus UUID/path incarnation (R15 remains PARTIAL)
- personal Git→SVN engine-cycle proof for qualified merge-DAG admission (inspect
  admission is proven; PR-based replay is a separate surface)

Assert zero unwanted remote changes and stable checkpoints on rejection, and
no duplicates on accepted ordinary replay. The coworker’s exact production
graph remains unreproduced; do not invent it.
