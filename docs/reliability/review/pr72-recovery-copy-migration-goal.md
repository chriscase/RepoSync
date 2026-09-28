# Goal: recover interrupted RepoSync work, determine exact implementation state, then complete the bounded copy-only migration goal

Repository: `chriscase/RepoSync`

Primary draft PR: `#72`

Expected long-lived feature branch: `feature/reposync-reliability`

Last independently reviewed Git head before the interrupted Grok Build implementation:

`c52dea6554a629efb7725ea620659501ad6af7a1`

Do **not** assume Grok Build finished, committed, pushed, or even remained on that branch. It may have left uncommitted changes, commits in another worktree, a detached HEAD, or generated evidence that was never published.

Your first responsibility is to recover the exact work state without destroying anything.

---

# Phase 0 — locate and preserve the interrupted Grok Build work

Before changing files:

1. Inspect the repository and all Git worktrees.

   Use appropriate read-only commands such as:

   - `git worktree list --porcelain`
   - branch/ref inspection
   - `git status --short --branch`
   - `git log --all --decorate --graph`
   - relevant reflogs where useful
   - filesystem timestamps only as supporting evidence, never as the sole authority

2. Find the worktree Grok Build was using.

   Likely signals include:

   - branch `feature/reposync-reliability`
   - a worktree descended from reviewed head
     `c52dea6554a629efb7725ea620659501ad6af7a1`
   - recent changes concerning:
     - v13 nullable `commit_map.git_sha`
     - v14 generation/lineage/frontier/outcome tables
     - copy-only migration tooling
     - migration interruption tests
     - K01, K02, or K03
   - uncommitted or untracked files related to migration qualification

3. Assume local changes are valuable until proven otherwise.

   **Do not run any destructive cleanup command**, including:

   - `git reset --hard`
   - `git clean`
   - checkout/restore that overwrites files
   - rebase
   - force checkout
   - force push
   - deleting worktrees
   - deleting branches

4. If more than one worktree plausibly contains continuation work:

   - compare them;
   - identify which contains the newest coherent implementation;
   - preserve every unique change;
   - report the alternatives in the handoff.

5. Record before touching implementation:

   - physical worktree path
   - current branch or detached HEAD
   - HEAD SHA
   - upstream SHA if any
   - merge base with
     `c52dea6554a629efb7725ea620659501ad6af7a1`
   - commits after that reviewed head
   - staged changes
   - unstaged changes
   - untracked files
   - whether the working tree is clean
   - whether any relevant commits exist only locally
   - whether remote `feature/reposync-reliability` moved since the last reviewed head

6. Preserve the recovered state before materially modifying it.

   Prefer ordinary Git commits on the existing feature branch if the changes are coherent enough to commit.

   If they are not yet coherent, preserve their exact diff/state in a safe local recovery ref or equivalent non-destructive mechanism before editing further.

   Do not hide or discard partial Grok work merely because it is unfinished.

---

# Phase 1 — reconstruct where Grok stopped

Read the repository's existing reliability documents and PR #72 history, including:

- `docs/reliability/GOAL.md`
- `docs/reliability/design.md`
- `docs/reliability/migration-proposal.md`
- `docs/reliability/required-cases.json`
- `docs/reliability/review/latest.md`
- prior review/goal files under `docs/reliability/review/`
- PR #72 review comments if available

Preserve the original GOAL bytes and prior brief/review documents.

The independently reviewed boundary was:

- original base:
  `87379741779a6259f7eeb52a68cc6f061174e5ef`
- last independently reviewed head:
  `c52dea6554a629efb7725ea620659501ad6af7a1`

At that review:

- J01–J03 were accepted for their tested bounds.
- Existing runtime receipt/inventory fixes were to be retained.
- No schema migration had yet been approved for normal startup.
- The next authorized work was a **corrected copy-only migration implementation and qualification pass**.
- Normal `Database::initialize`, daemon startup, installer startup, and scheduler behavior were to remain unchanged with respect to v13/v14 activation.
- No production access, deployment, merge, reset/reimport recovery, general #64 operation service, or feature-branch force push was authorized.

Determine precisely which parts Grok implemented.

Produce an internal checklist showing each requirement below as:

- `DONE AND VERIFIED`
- `DONE BUT NOT VERIFIED`
- `PARTIAL`
- `NOT STARTED`
- `IMPLEMENTED DIFFERENTLY — REVIEW NEEDED`

Do not start over if Grok already implemented valid portions.

---

# Phase 2 — complete the previously authorized bounded goal

The bounded goal is to correct K01–K03 and implement/qualify candidate v13 and v14 **only through an explicit fixture/copy-only migration entry point**.

## K01 — enforce frontier/evidence ownership correctly

The previous migration proposal did not adequately bind a frontier to the evidence outcome that justified it.

Correct this.

A later handled frontier must not be allowed to cite:

- an outcome from another repository;
- another generation;
- the wrong direction;
- the wrong source identity;
- an incompatible projection/policy version;
- a pending outcome;
- `effect_unknown`;
- `reconciliation_required`;
- nonexistent evidence.

Explicitly distinguish:

### Initial-baseline authority

A generation's initial frontier may be established only from a separately verified lineage baseline.

Do not require an invented Git→SVN remote effect for an SVN-imported baseline.

The baseline must match the lineage's proved source identities and baseline values.

### Subsequent frontier authority

A later handled frontier must be justified by a resolved, generation-owned outcome whose identity and policy match the frontier transition.

Use structural SQL constraints where appropriate and a single validated transactional writer for semantic relationships that SQLite constraints cannot adequately express.

Do not rely on an unconstrained nullable `evidence_outcome_id`.

Tests must prove both valid and invalid cases.

At minimum reject:

- cross-repository evidence
- cross-generation evidence
- wrong direction
- wrong source SHA/revision
- policy/projection mismatch
- pending outcome
- unknown-effect outcome
- nonexistent outcome
- arbitrary NULL evidence after the initial baseline

Prove a legitimate initial baseline and legitimate later resolved outcomes succeed.

Do not automatically select the highest generation. Define explicit active-generation semantics or leave activation outside this slice if it has not been reviewed.

---

# K02 — preserve AUTOINCREMENT history through the v13 rebuild

Candidate v13 rebuilds `commit_map` so `git_sha` can be NULL.

The migration must preserve:

- every retained row
- every retained `id`
- every old column value
- indexes
- historical AUTOINCREMENT high-water state

Do not merely reset the sequence to `MAX(id)`.

Preserve the original `sqlite_sequence` high-water mark in the same transaction as the rebuild/version update.

Required cases include at least:

1. Never-used table.
2. Ordinary populated table.
3. Sparse IDs.
4. Historical high ID deleted before migration:
   - e.g. committed IDs 1 and 100, then 100 deleted.
   - next generated ID after migration must still be at least 101.
5. Empty-but-previously-used table:
   - old sequence must survive even with zero retained rows.
6. Reasonable high-water boundary/large value case.

Other tables' sequences must remain unchanged.

A row-count or row-content digest alone is insufficient.

---

# K03 — repair copy-origin constraints

The previous conditional CHECK allowed:

- non-NULL `copy_from_path`
- NULL `copy_from_rev`

because SQLite CHECK accepts NULL results.

Correct the schema so copy origin is exactly one of:

1. no copy origin:
   - `copy_from_path IS NULL`
   - `copy_from_rev IS NULL`

or

2. complete copy origin:
   - `copy_from_path IS NOT NULL`
   - `copy_from_rev IS NOT NULL`
   - `copy_from_rev > 0`

Preserve empty-string path as a valid representation of the SVN repository root where the contract requires it.

Test all meaningful combinations:

- NULL / NULL
- path / NULL
- NULL / positive rev
- path / zero
- path / negative
- path / positive
- empty path / positive revision

Audit other nullable conditional constraints introduced by this migration for the same SQLite three-valued-logic problem.

---

# Candidate v13 requirements

Implement one reusable candidate migration implementation, not separate production and test algorithms.

Candidate v13 should:

- rebuild `commit_map`
- make `git_sha` nullable
- preserve every legacy row/ID/value
- preserve historical `sqlite_sequence`
- restore the intended indexes
- validate the exact expected v12 starting shape
- reject forged/partial/unsupported schema shapes
- execute migration SQL, conversion, validation, and `PRAGMA user_version = 13` in one checked transaction
- leave the source copy unchanged on failure

NULL `git_sha` is not by itself proof of a filtered outcome.

Do not infer new filtered decisions from old NULL/ownerless records.

---

# Candidate v14 requirements

Implement the proposed generation/ownership tables in a corrected form, including the K01/K03 fixes.

The design remains based on:

- repository + generation lineage ownership
- SVN identity
- Git identity
- baseline identity
- projection identity
- separate directional frontiers
- typed outcomes
- preserved links to legacy evidence
- read-safe disposition for unqualified repositories

Only independently proved fixture pairs may receive a canonical generation.

Do not convert solely because the local inventory says
`qualified_fixture_shape`.

Unknown, already-pruned, historical-filtered, v1/v2 unverified receipt, endpoint-replaced, lineage-unknown, or effect-unknown states must retain all legacy bytes and a read-safe disposition.

Do not reset their checkpoints.

Do not silently normalize them.

---

# Copy-only migration boundary

This is critical.

Create/use an **explicit copy-only migration command or test entry point**.

It may migrate only:

- synthetic fixtures;
- quiesced installation copies;
- old-code fixture copies generated by the existing reliability harness.

It must not become reachable automatically from ordinary startup.

For this pass:

- default `Database::initialize()` must not automatically apply v13/v14;
- normal daemon startup must not automatically apply v13/v14;
- installer/startup behavior must remain unchanged;
- scheduler behavior must remain unchanged.

Prove this explicitly with tests.

The candidate migration registry/implementation should nevertheless be the same implementation intended for eventual production activation, so the qualification path cannot diverge from a future production algorithm.

---

# Transaction and interruption requirements

Each migration version is independently atomic.

For each candidate version:

1. verify expected starting version and exact schema shape;
2. verify foreign-key enforcement is enabled before the transaction;
3. begin checked transaction, preferably `BEGIN IMMEDIATE`;
4. perform conversion;
5. validate rows, ownership constraints, indexes, foreign keys, integrity and sequence preservation;
6. set `PRAGMA user_version` inside that transaction;
7. commit.

Failure must not report success.

If v13 commits and v14 later fails, it is acceptable for the copied DB to remain a valid v13 DB.

Do not falsely claim the entire 12→14 sequence rolled back.

Required failure/interruption tests should cover meaningful boundaries, including:

- before transaction
- during rebuild
- after copy before old-table drop
- after table replacement before validation
- before user_version update
- after user_version update but before commit
- v14 partial conversion
- constraint failure
- simulated write failure/read-only database
- restart/retry after a committed v13 and failed v14
- repeated invocation/idempotence
- forged user_version with wrong physical shape
- unsupported future version

For each result prove either:

- original version and original contents remain, or
- a complete, valid committed version exists.

No half-version state may be reported as success.

---

# Old-install qualification

Reuse the existing pinned old-code generator and sealed-copy infrastructure.

Old writer must exit before candidate migration opens a copy.

Do not mutate the original fixture.

For qualified synthetic pair(s), verify before and after:

- DB/schema version
- row counts
- ID-keyed legacy row digests
- `sqlite_sequence`
- repository IDs
- enabled/disabled state
- parent hierarchy
- configuration bytes
- credential ownership references
- checkpoint values
- applied mappings
- no-target receipts
- local refs
- path policy
- synthetic remote Git refs and SVN trees where fixture-local read-only proof is required
- new generation/outcome/frontier/evidence rows

For unqualified synthetic shapes:

- preserve all legacy rows and bytes;
- record read-safe migration disposition;
- do not create unjustified canonical frontiers.

Keep remote SVN UUID/copy ancestry and remote Git identity marked unqualified unless the disposable fixture itself establishes them through the reviewed proof path.

---

# #54 compatibility

The v13 implementation must coordinate with #54 rather than create a competing rebuild.

The future schema semantics must distinguish:

- mapping exists with a Git SHA
- mapping exists with NULL Git SHA and a proved typed no-target outcome
- unresolved legacy NULL/ownerless row
- no mapping row

Audit relevant readers accordingly.

Do not close #54 or #63 merely because the copy-only prototype passes.

---

# Testing/evidence requirements

Retain every existing required reliability case unless an explicitly reviewed test was already authorized to change.

Do not weaken existing assertions.

Add explicit required cases for the copy migration, including:

- v13 row preservation
- v13 sparse/high sequence preservation
- v13 empty-used sequence preservation
- v13 failure rollback/retry
- v14 valid initial baseline
- v14 valid resolved outcome frontier
- v14 cross-repo evidence rejection
- v14 cross-generation evidence rejection
- v14 pending/unknown outcome rejection
- v14 policy mismatch rejection
- v14 source mismatch rejection
- K03 copy-origin matrix
- unqualified state preservation
- v13-complete/v14-failed restart
- repeated copy migration
- default ordinary startup does not activate v13/v14

Continue using:

- exact required-case manifest
- sandbox boundary canaries
- complete artifact scanning
- matched original-base comparison
- retained reviewed comparison anchor
- broader E2E where applicable

Keep the known formatting failure visible rather than doing a broad unrelated formatting rewrite.

---

# Safety boundaries

Do not:

- contact production;
- use active repository credentials;
- migrate an active installation;
- enable these migrations during normal startup;
- reset synchronization checkpoints;
- reimport as recovery;
- run a general #64 operation/recovery implementation;
- perform remote force pushes;
- merge PR #72;
- release;
- deploy;
- modify issue acceptance criteria.

No production DB is needed.

---

# Work organization

First preserve/recover Grok Build's existing local work.

Then continue from it.

Prefer separate reviewable ordinary commits for:

1. recovery/preservation of interrupted work if necessary;
2. K01 ownership/frontier correction;
3. K02/K03 and candidate v13 migration;
4. candidate v14 copy-only migration;
5. interruption/negative tests and harness;
6. documentation/handoff.

Do not squash away evidence of how the interrupted work was recovered unless there is a strong reason.

Push ordinary commits to the existing feature branch once the recovered state is coherent and safe to publish.

Do not force push.

Keep PR #72 draft.

---

# Required return handoff

Return `READY_FOR_REVIEW` only when you reach the next independent-review gate.

The handoff must include:

## Recovery identity

- Grok Build worktree path found
- branch/detached state found
- HEAD at recovery
- local-only commits discovered
- staged/unstaged/untracked changes discovered
- how those changes were preserved
- whether any competing worktree contained unique changes
- exact reviewed starting SHA:
  `c52dea6554a629efb7725ea620659501ad6af7a1`

## Final identity

- functional head
- final documentation head
- remote feature head
- PR synthetic merge SHA
- matching tree SHA
- clean/dirty state
- original GOAL SHA-256
- copied goal/brief SHA-256
- dependency lock SHA-256

## Goal status

For every major requirement above:

- DONE AND VERIFIED
- PARTIAL
- NOT DONE
- BLOCKED

Especially K01, K02, K03, v13, v14, interruption behavior and startup nonactivation.

## Test evidence

- exact required-case IDs added/changed
- total exact cases
- failed/skipped counts
- matched comparison counts
- broader E2E results
- scanner results
- any still-failing ordinary CI steps

## Migration evidence

- pre/post schema and user_version
- row preservation manifests
- sqlite_sequence before/after
- FK/integrity results
- invalid ownership/constraint rejection matrix
- interruption/retry matrix
- unqualified-state preservation
- proof normal startup still does not apply candidate v13/v14

## Artifacts

- CI run IDs/links
- artifact ID
- API/recomputed artifact SHA-256
- key extracted evidence hashes
- expiration date

## Remaining risks

Explicitly list anything not qualified, including deployed-version, installer/daemon activation, remote lineage, live acceptance and #64 recovery.

Then stop. Do not merge or proceed into production activation without the next independent review.