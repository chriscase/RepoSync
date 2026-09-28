# Goal: correct copy-migration admission and qualify typed readers without startup activation

Repository: `chriscase/RepoSync` • branch: `feature/reposync-reliability` • draft PR: **72**.

Start from reviewed head `9f6330e1e3dad7d8917b432d9d367665641a5227`. The functional implementation reviewed here ends at `cdb0907f75512b20f4932f9e4f9cb57aa60d1f42`; original base is `87379741779a6259f7eeb52a68cc6f061174e5ef`. `f74fce855a1f1d80dd631397f436ba33906272e6` is a retained comparison anchor, not the immediate predecessor.

Read the posted **review #5327264198 (review 7)** anchored to this head and `RepoSync-PR72-review-7.md`. The outcome is PARTIAL / CHANGES NEEDED for L01–L03, with typed-reader implementation authorized after their correction in this same pass.

## Preserve work and scope

Keep the executable copy-only v13/v14 implementation, K02 sequence preservation, K03 copy-origin checks, current-row K01 relational protection, and the 70 required cases. Preserve all original GOAL/brief/review bytes. Original GOAL SHA-256 is `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`; lock SHA-256 is `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05`.

The accessible recovery search found no interrupted migration implementation and preserved the known state. Do not repeat the broad 499-location search without new location/transcript evidence. Do not claim to have found Grok code that was not found. Normal non-destructive git status/ref checks still apply; preserve any new local work before editing.

No production endpoints/credentials/data directories, live acceptance, checkpoint reset, reimport-as-recovery, force push, branch/worktree cleanup, merge, release, or deployment. Default `Database::initialize`, daemon, installer and scheduler must remain on the ordinary v12 registry. No general #64 operation service. All candidate DDL/reader tests remain on disposable isolated copies.

## Stage A — correct the three findings with actual implementation regressions

### L01: prove writable copy storage is independent

Inspect `candidate_migration.rs` path admission, manifest, SQLite open/recovery and final source checks. Distinct canonical paths and identical bytes/modes do not prove independent files: hard links share storage.

Add a deterministic test using the real `CopySession` API where source/copy databases are hard links. It must reject before a write-capable connection or journal recovery can alter either source or outside canary. Merely returning an error after a committed source change is failure. Add a target DB hard-linked to a third canary file and a safe ordinary independent-copy control. Test full source/canary bytes and schema version, not just return status. Preserve existing overlap, symlink, source WAL and changed-config cases.

Implement portable-within-the-supported-fixture-platform identity checks. Use device/inode and link-count information where appropriate; reject unknown/unsafe aliasing rather than silently breaking links, deleting journals, copying over inputs, or claiming protection against an unqualified concurrent host. Check/revalidate at the actual mutable-file boundary. The caller-supplied temporary directory is not a substitute for file ownership proof. The nonconcurrent, quiesced, private-directory assumption must remain explicit.

### L02: qualification must enforce the evidence inventory

Inspect `qualify_imported_pair`, the production checkpoint/receipt writers, the inventory and `pinned_unqualified_overlays`.

The qualifier currently checks `no_target_git_outcomes_<repo>` rather than actual per-SHA `handled_git_no_target_<repo>_<sha>` receipts. Tests with invented unused keys are not real-receipt rejection evidence. It also does not reconcile relevant incoming scoped SVN/progress/watermark contradictions or supported unknown-effect evidence.

Create labeled derivatives of the unchanged old generator output. Keep the clean output intact. Exercise each derivative through the actual qualifier and copy conversion:

- Original clean two independent r2 imports plus disabled repository remain accepted/unqualified as appropriate.
- Combined incoming values column=2, scoped=999, global=888, import watermark=777 remain visible and require reconciliation.
- Actual-key v1/v2 unverified no-delta receipts, a historical filtered receipt and a candidate v3 receipt do not pass a narrow old-import qualifier merely because the wrong key was counted. Preserve raw bytes; unsupported shapes may safely refuse without implementing their migration.
- An explicit supported unknown-effect marker must not become `qualified`.
- A contradictory scoped import-progress claim is diagnosed rather than normalized. A singleton/global reference must remain ownership-ambiguous where appropriate, not borrowed across repositories.

Use exact repository ownership when enumerating receipt keys; IDs with shared prefixes must not cause cross-repository matches. Reuse or consolidate the evidence-vocabulary/decision contract so inventory and admission cannot silently drift. A local fixture classification is not remote identity proof. Do not select maxima, invent a baseline, clear receipts or change old cursors to make a fixture qualify.

A failed admission must not leave newly granted canonical authority. Migration should preserve legacy data plus the appropriate read-safe disposition, or refuse before conversion. Validate this end-to-end, not solely by manually assigning a disposition after bypassing qualification.

### L03: prevent baseline rollback and preserve resolved history

Use the real `advance_frontier` writer and the committed schema to reproduce:

1. initial Git baseline B;
2. valid resolved transition to E;
3. attempted outcome-backed transition to B with predecessor E.

Step 3 must reject without changing frontiers or adding a committed false outcome. It is not a baseline INSERT/REPLACE, so existing replacement tests are insufficient. Include a valid two-step forward control, stale/repeated-source controls, and raw-SQL boundary tests. Do not invent an outbound SVN commit just to create a baseline reservation. Keep baseline authority explicitly distinct.

At minimum, known-baseline return must be impossible. Clarify which source-order checks are structural and which require separately verified Git ancestry. Matching a predecessor string must not be advertised as proof of arbitrary Git ancestry or complete processing. Do not build a general rebase/merge engine here.

Resolved outcome evidence must remain immutable after later frontiers cease to cite it directly. Test update, delete and `INSERT OR REPLACE` attempts on an earlier resolved outcome. Preserve pending/unresolved records without pretending the general #64 transition service exists. Prefer an explicit append-only resolved-evidence rule over ad hoc protection of only the current row.

## Stage B — typed mapping/list/status/emitted readers on copies

Once Stage A controls pass, implement this proposed next slice without another planning-only stop. Keep it feature-gated/copy-only. Operational API/UI/daemon activation is not included.

Design typed read results that clearly separate:

- generation-qualified mapped source/target;
- explicitly proved no-target outcome;
- retained legacy mapping useful for display but not canonical authority;
- unresolved nullable/ownerless/conflicting evidence;
- missing mapping or unsupported/missing generation.

Audit the current `lookup_mapping`: its non-NULL path ignores generation and its legacy query does not filter direction. Do not reuse that fast path as proof. A non-NULL old SHA is not automatically evidence for every generation. Preserve old raw values for display even when malformed or unresolved; never promote them to canonical truth.

Implement and test:

1. **Scoped lookup.** Repository, explicit generation, direction and source identity all matter. Reject or return an explicitly unqualified result for wrong/absent generations. Require the appropriate evidence link/outcome for canonical interpretations. Two directions sharing a revision number are not automatically duplicate sources.
2. **List/read model.** Deterministic ordering/pagination; every retained legacy row/ID remains representable, including NULL. No global maximum or last inserted row supplies authority. Ambiguity is surfaced, not arbitrarily selected.
3. **Status/read model.** Show handled sources separately from emitted targets, current qualified generation separately from legacy/unqualified state, and disabled/not-qualified status without activating a repository.
4. **Last emitted target.** After an applied effect followed by a no-target source, report the correct most recent actual emitted object if this is the contract. Do not mistake the latest NULL target for missing history, or advance a handled source from the latest emitted target. Test both directions and intervening pending/effect-unknown records.
5. **Read-only behavior.** Reader calls must not initialize/migrate, grant authority, materialize receipts, update a Git index, synchronize, or contact remotes. Prove DB rows/versions and sealed source/config/ref state are unchanged.

Keep a small explicit decision matrix for generation/owner/direction/NULL/outcome combinations. Add mandatory exact cases for the newly supported readers, not mocks that simply restate query results. Use independently specified fixture expectations.

## Verification and publication

Retain all 70 required case identities and their strict oracles. Add required tests for L01–L03 and readers; keep all earlier failures/ignored tests visible. Test against the single real candidate implementation, not a new duplicate migration algorithm. Source manifests, historical sequence preservation, v13/v14 failure boundaries, and startup-at-v12 remain required.

Use the existing isolated runner, scanner, original-base and accurately labeled retained-anchor comparison. When feasible add the immediate reviewed head as a matched comparison; otherwise explicitly label an archived catalog comparison. Do not claim a broad default-feature run executes feature-gated candidate cases.

Work in separate ordinary reviewable commits: copy safety; evidence admission; frontier/history correction; readers/tests; concise documentation. Use targeted tests during iteration. Run complete required CI after meaningful functional changes; do not repeatedly rerun unchanged trees or produce endless documentation-only receipt commits. A final documentation SHA can be reported in the PR/handoff rather than recursively inserted into its own file. Bound retries; report a concrete blocker rather than weakening assertions or looping.

Keep the draft PR and stop after publishing the next reviewable result. Return PARTIAL rather than READY_FOR_REVIEW-as-complete when required cases are red or missing.

## Compact return handoff

Report exact starting/functional/final/remote/merge/tree identities and original/brief/lock hashes. Include L01–L03 dispositions with actual failing-then-passing test evidence, the typed-reader decision matrix, old-source/canary/no-write proof, added and retained exact-case counts, CI run/artifact IDs and hashes, accurately labeled comparisons, scanners, and known limitations. Reference unchanged historical evidence instead of pasting every manifest again.

Recovery need not be re-investigated; retain the existing audit's accessible-scope limitation. Deployed versions, arbitrary old histories/policies, remote production identity, fencing/backup ownership, installer/scheduler activation, live acceptance, down migration and #64 external-effect recovery remain unqualified. No issue acceptance criteria or closures change in this pass.
