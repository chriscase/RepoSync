# #66 contract: rewritten or unproven history before reset/replay

**Status:** smallest remaining product slice after the merged team
pre-reset gate and hide/push pending selection (#109). Team
`inspect_team_history` classifications are unchanged; durable
`reconciliation_required` blocks now cover rewrite (`non_fast_forward`,
`observed_remote_rewrite`) and UnsupportedHistory (`unsupported_merge_dag`,
`unsupported_backlog`, `unproven_pending_range`), including fetch-time
second-line hits. Personal-mode Git→SVN inspect now shares the same
`inspect_fetched_history` gate with proven containment tests; merge-DAG
replay and >1000 continuation remain later.

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
| `unsupported_backlog` | more than 1000 pending commits |
| `unsupported_merge_dag` | pending history has a merge commit |
| `unproven_pending_range` | P is not an ancestor of R |

Ordinary qualified linear P→R is admitted and pinned to that R. Later
checkout/reset must use the same object. Pending-commit selection uses a
hide/push ancestry frontier (`P..R`) so a visited-order stop at `P` cannot
omit older pending commits; the proof lives in `pending_frontier` unit tests
(`hide_push_frontier_includes_older_pending_side`), not in
`candidate_r10_merge_dag_older_side_not_silently_skipped` (that case only
asserts merge-DAG fail-closed before writes).

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
| UnsupportedHistory (`unsupported_merge_dag`) + durable restart | **proven** — `candidate_r66_personal_merge_dag_contained` |
| Observed-remote rewrite (`observed_remote_rewrite`) | **NOT RUN** — team polling cases cover O→R; personal inherits inspect |
| >1000 backlog durable restart | **NOT RUN** — team `candidate_r10_durable_backlog_block_survives_restart` |
| Merge-DAG replay / >1000 continuation | **remaining** |

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
   Until proved, unqualified merge DAGs and >1000-commit backlogs **reject
   before writes**. Do not silently drop older work.
6. Normal SVN merges append revisions. A revision-number jump is not a Git
   rewrite. Path delete/recreate and UUID/copy-origin change are identity
   events for #63/#15, not this gate’s `non_fast_forward`.
7. Polling is the safety gate. Webhook `forced` is a hint. Re-validate pinned
   targets before publication.
8. Old healthy linear histories continue after migration. Unsupported history
   is reported, not “fixed” by resetting or clearing maps.

## Smallest next implementation slice (after this review)

Durable rewrite block, personal inspect (with containment proofs), webhook
`forced` hint, exact skip disposition, hide/push pending selection, durable
UnsupportedHistory blocks (including fetch-time persistence), polling O→R
force-push detection, and restart enforcement have landed. Remaining #66 work is
explicit recovery algorithms, not more pre-reset classification:

1. Continuation past 1000 pending commits.
2. Merge-DAG replay.

Automatic rewrite reconciliation stays a later slice with its own proofs.

## Required tests

Already present: ordinary fast-forward; unchanged tip with new SVN work;
L=R lagging P; rewrite of already-synced work; metadata amend; missing
branch/object/shallow; ancestry-command error; merge DAG reject (including
older-side hide/push frontier vs visited-order skip in unit tests); >1000 reject;
ignored-path preservation; repo scoping; durable block survives restart for
rewrite, observed-remote rewrite, UnsupportedHistory (merge DAG and backlog);
fetch-time UnsupportedHistory persistence; personal-mode rewrite and merge-DAG
containment with durable restart (inspection/tracking refs retargeted to restored
tip before reopen).

Still required before claiming #66:

- `unproven_pending_range` engine-level durable restart (deferred: inspect
  P→R ancestry gate rejects the same shapes first; unit coverage lives in
  `pending_frontier::non_ancestor_tip_fails_closed`)
- equal-looking content with different provenance (already partly R09 amend)
- SVN mergeinfo-only versus UUID/path incarnation (R15 remains PARTIAL)
- merge-DAG replay and >1000 continuation algorithms

Assert zero unwanted remote changes and stable checkpoints on rejection, and
no duplicates on accepted ordinary replay. The coworker’s exact production
graph remains unreproduced; do not invent it.
