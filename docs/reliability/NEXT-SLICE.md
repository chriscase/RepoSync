# RepoSync PR #72 — bounded correction and history-containment pass

/goal

Continue `chriscase/RepoSync` on `feature/reposync-reliability`, draft PR #72. Execute this pass, publish its evidence, and stop for review. Do not implement the whole epic.

## Identity and scope

Start from current reviewed head `3d7bb2f0156b804761678fbdff0b49f01b3ae5a3`, whose functional parent is `deacc6b39728b81b9fde74045fa4cb691761d614`. Original review base is `87379741779a6259f7eeb52a68cc6f061174e5ef`.

Preserve `docs/reliability/GOAL.md` byte-for-byte at SHA-256 `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`. Read the original goal, epic #61, #62/#63/#64/#66, and the accompanying independent review. Copy that review into `docs/reliability/review/phase0-independent-review.md` and this brief into `docs/reliability/NEXT-SLICE.md`; record the latter's hash. These supplement the original requirements; they do not relax them.

The review found useful Phase 0 work but did not approve merge, rollout, migration or complete sandbox qualification. You may repair the findings and then implement the bounded gate in this same pass. Do not require another planning-only review between them when the prerequisites below demonstrably pass. If a prerequisite cannot be met, publish the useful work and the blocker instead of bypassing it.

## A. Make the runtime boundary and evidence trustworthy first

1. Fix review F01. The current `(allow default)` plus loopback outbound exception is not filesystem isolation and admits unrelated local services. Use a constrained OS policy or disposable VM/container that limits the application/test runtime to intended fixture state and services. A container must not inherit host networking, the host Docker socket, credential stores, SSH agents or production mounts. Build/install dependencies separately from isolated execution.
2. Set `persist-credentials: false` for diagnostic checkout. Ensure the application/test runtime cannot read real source-checkout credentials or host configuration through absolute paths. Keep only synthetic secrets in fixtures; do not inspect actual production secrets to prove denial.
3. Enforce fixture-owned canonical paths and enrolled network targets before operations, including `file://` paths and traversal/symlink escapes. Private network addresses alone are insufficient. Add deterministic synthetic canaries demonstrating permitted fixture access, denied external egress, denied access to an unrelated listening loopback endpoint, and denied read/write of a fake host-private file. A clean runner or private namespace may provide this boundary; document exactly what is proven. Never silently run unsandboxed when a guard is unavailable.
4. Fix F05. Replace the any-positive-test-total gate with a required manifest of exact test names/scenario subcases. Fail on any missing, renamed-out-of-filter, ignored or failed required case. Prove this gate itself rejects omission of the R09 containment case. Preserve counts for diagnostics, candidate regressions and skipped coverage separately. Existing bug-observation tests may remain for unfixed behavior, but must not be counted as candidate acceptance.
5. Retain per-case sanitized Git ref/tree/ancestry and SVN revision/tree manifests, relevant DB mappings/checkpoints before/after, outcomes and command status. Save the exact resolved dependency graph/lockfile and hash, tool versions, source commit/tree and original goal hash. Do not export production data, fixture credentials or raw DBs containing credentials. Apply a fixture-secret-canary check to exported logs/manifests.
6. Use a repeatable preparation path from a clean checkout. Apply the same recorded dependency resolution/toolchain to base and candidate comparison runs. Do not rely on an undocumented local `cargo update`. A full project-wide dependency-policy change is not required for this pass. Document any old-source build overlay and keep production source unchanged in the baseline run.

## B. Correct the diagnostic boundaries and design

Address F02–F04 before claiming the next gate proven.

- Build R09 with a developer clone, an isolated bare remote, and a distinct RepoSync bridge checkout. Establish a real SVN-derived baseline and verify its actual Git/SVN contents and production mapping, not merely a positive sync count or a provenance message. The developer rewrites/pushes from the developer clone only.
- Keep the old reset/replacement case as a generic non-fast-forward example. Add a real rebase or metadata-only amend case with already-synchronized history; require an explicit rejection reason as well as zero remote writes. Equal content alone does not prove the gate executed.
- Where using `git merge-base --is-ancestor`, require exit 0 for positive, 1 for a valid negative, and handle other failures separately. Do not turn missing-object/command errors into asserted proof of a rewrite.
- Split the current R06 evidence: the core fixture is a manually established checkpoint-consequence diagnostic; the API fixture is an unrelated Git-first admission diagnostic relevant to R08. It is not the same SVN-derived graph. Correct the matrix, sandbox guide and current handoff accordingly. An integrated valid SVN-origin API→persisted child→engine R06 test is required before #67 acceptance, but need not delay this isolated #66 slice.
- R03 remains route coverage with synthetic progress; do not describe it as actual in-flight cancellation. All unexecuted R01–R24 acceptance remains open. Do not mark a whole scenario PASS from one subcase.

Define four separate Git values in the design and implementation:

`P` = last durably handled Git checkpoint for this repo/pair; `O` = prior observed remote tip, if present; `R` = fresh remote tip for this inspection; `L` = bridge checkout tip.

`O == R` and `L == R` do not imply `P == R`. With `P=A`, `L=O=R=C`, and `A -> B -> C`, B and C are still pending. `P == R` means no new Git-direction work, not no SVN-direction work. Do not return early from the entire cycle because the Git tip is unchanged.

## C. Implement the narrow pre-reset team-history gate

Outcome: reject unsupported or unproven incoming Git history before destructive local reset, logical checkpoint adoption/advancement, conflict application, or writes to either remote. Preserve ordinary qualified linear sync. This is a containment slice, not a rebase/reconciliation implementation.

1. Trace the whole team cycle and its callers. The current cycle fetches SVN first, and that path can adopt checkpoints. Place admission before any such logical mutation. Do not implement a nominal pre-reset check after another path has already advanced state. Inspection fetch objects/refs and bounded rejection diagnostics are allowed effects; their allowlist must be explicit.
2. Fetch/inspect the exact configured remote branch using a fresh, unambiguous result, preserving the old bridge state. Do not treat a stale remote-tracking ref or old FETCH_HEAD as proof of a current successful fetch. Missing branch, auth/transport failure, shallow/incomplete history, missing checkpoint object, actual non-fast-forward history and ambiguous legacy state must have distinguishable outcomes.
3. Resolve the checkpoint through a documented repository-scoped legacy path. Do not select another repository's global cursor, infer origin from an editable commit message, adopt the largest available revision, or manufacture a new baseline. Use provably associated legacy evidence where available. Uncertain history is blocked before mutation and reported; healthy legacy configurations must not be blanket-disabled merely because future generation columns do not yet exist. Full canonical-state migration remains #63, not this pass.
4. Gate replay relative to `P`, not local checkout equality. Preserve unpublished local commits and dirty/index state; do not erase them as a hidden recovery step. Reject ambiguous local state without mislabeling every such condition as a remote force push.
5. Rejected history leaves both upstream Git refs and SVN revisions/trees unchanged; does not advance directional cursors, add applied mappings, increment success counters, or alter protected bridge HEAD/index/worktree. A bounded inspection namespace and explicit failure/audit/status records are permitted. Generic cycle finalization must not convert rejection into success or erase its meaningful reason.
6. Repeated polling and a process/engine restart must reject the same unchanged unsafe state again with no unwanted writes. An additive repo-scoped blocking record in existing storage is acceptable if its semantics and persistence failures are handled correctly. A generic error string alone must not be advertised as a complete durable quarantine workflow. Do not add a general operation schema or migration in this pass.
7. Keep fresh inspection and subsequent mutation tied to pinned inputs. Never silently reset to a different ref after validating one. Report the remaining multi-writer/publication race and recovery limits under #64; an ancestry check is not distributed atomicity. Do not perform automatic rebase, reset, force-push, reimport, orphan-history adoption or checkpoint clearing as recovery.
8. Do not quietly declare the existing capped/revwalk selection safe for all DAGs and backlogs. For unqualified merge-DAG/overflow conditions touched by this gate, either prove the selected handling correct or reject the condition before writes with an explicit reason. No silent dropping of older work beyond 1,000 commits. Full support remains open where unimplemented; do not broadly rewrite the replay engine to close the entire issue.

## D. Required proof for this pass

Run real-engine assertions, not only helper mocks:

- **Rewrite rejection:** already-handled Git history rewritten from the independent developer clone; both changed-content replacement and rebase/amend coverage; bridge and remote preservation plus explicit rejection.
- **Repeat/restart:** the same rejected remote state remains no-write on another cycle and after reopening persistent fixture state.
- **Healthy Git fast-forward:** at least two pending commits from a verified SVN-derived baseline reach SVN once in correct order, with intermediate content/mapping assertions; the next unchanged run is a no-op.
- **Caught-up checkout, lagging cursor:** `L=R` but `P` is an older ancestor; pending work is not skipped.
- **Unchanged Git, new SVN work:** `P=R` with a new SVN revision; the normal SVN direction remains eligible and is not suppressed by an early cycle return. Assert actual tree/mapping behavior. If a pre-existing unrelated apply defect prevents a full success, retain its failing integration evidence and separately demonstrate that the new gate admits the correct path; do not call the full round trip PASS.
- **Missing/unknown:** fresh remote ref missing, required object absent/incomplete, ambiguous checkpoint and ancestry-command failure; no stale-ref fallback, no initialization/reset.
- **Local preservation:** unpublished bridge commit or dirty/index state is retained on rejection.
- **Repo scoping:** one rejected pair does not borrow another repo's checkpoint or disable unrelated healthy work.
- **Selection boundary:** targeted merge-DAG and >1,000 pending-commit tests either prove correct processing or explicit pre-write unsupported rejection; name any unimplemented behavior.
- **Harness:** required-case omission fails, and fixture-only boundary canaries pass without accessing production.

Positive controls matter: an implementation that simply returns an error for every repo must not pass this slice. Keep the existing suite intact. Compare the exact base and candidate using matched dependencies to identify newly introduced failures. The four existing team E2E failures, ignored conflict case and unrun LFS/provenance remain visible. Do not disable assertions, convert ordinary failures into success, or format unrelated files to make a dashboard green.

Record the existing SVN-apply-failure checkpoint-advance path (review F06) as a P0 release blocker under the existing #62/#63 discussion/evidence. Characterize it, but do not turn this pass into a broad repair of all synchronization semantics. Do not alter original issue acceptance criteria or close #62/#63/#64/#66.

## E. Publication and stop condition

Keep separate reviewable commits for harness/evidence fixes and the runtime gate. Use ordinary pushes on the existing feature branch and update draft PR #72. No merge, force push to this feature branch, release, deployment, production endpoints, live credentials, active synchronized repository writes or branch-rule changes. Force-updating deliberately disposable fixture refs remains permitted inside the qualified test boundary.

Return:

- Original base, previous reviewed head, new functional head and final documentation head; remote/worktree status.
- Original GOAL SHA-256 and NEXT-SLICE SHA-256; any deviation stated explicitly.
- F01–F06 dispositions: corrected, partially corrected, tracked existing blocker, or not run, with code and evidence references.
- Required test names and per-subcase PASS/FAIL/PARTIAL/NOT RUN; actual counts/ignored/filtered cases; exact commands and dependency-lock hash.
- Before/after bridge state, remote tree/revision and checkpoint/mapping evidence for rejected and accepted cases.
- CI source head and merge SHA/tree relationship, run/job/artifact links, digest origin and expiration. Do not invent self-referential final hashes inside an earlier evidence file.
- No-new-regression comparison, known base failures and remaining legacy/lineage/migration/cancellation/race gaps.
- A specific recommendation for the next smallest slice, without starting it.

Stop at READY_FOR_REVIEW or BLOCKED with the useful evidence. The deployed version is still NOT ESTABLISHED; no live acceptance is authorized.
