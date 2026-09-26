# PR72 review 7 — copy migration corrections before typed-reader qualification

Posted on [PR72 as review #5327264198](https://github.com/chriscase/RepoSync/pull/72#pullrequestreview-5327264198), anchored to `9f6330e1e3dad7d8917b432d9d367665641a5227`.

**Decision:** PARTIAL / CHANGES NEEDED. Retain the executable copy-only migration, the demonstrated K02/K03 corrections, current-row K01 ownership checks, and all 70 cases. Correct L01–L03 below, then implement the proposed #54 typed readers on migrated copies in the same bounded pass. Normal startup remains v12. No issue closure, merge, release, deployment, production access, checkpoint reset, or general #64 operation service is authorized.

## Reviewed identities

| Identity | Value |
| --- | --- |
| Original base | `87379741779a6259f7eeb52a68cc6f061174e5ef` |
| Immediate reviewed start | `c52dea6554a629efb7725ea620659501ad6af7a1` |
| Functional head | `cdb0907f75512b20f4932f9e4f9cb57aa60d1f42` |
| Final feature head | `9f6330e1e3dad7d8917b432d9d367665641a5227` |
| CI merge | `3ecbe6ad63ca552a033df615fa3fcc160687faae` |
| Matching feature/merge tree | `674a0d669852d20527df1502e39059c35ccdd851` |

## Evidence independently checked

The final feature and CI merge have the same tracked tree. Artifact `10912198824` was downloaded and its ZIP hash independently recomputed as `8e37fb3baea313d9532d6bbcad60bb0f7fdfece759a7c2d8a63a662e9c0d560a`. All key extracted hashes in the uploaded verification receipt match, including summary `4bea4481d64ee67a4933e23cab976f8abc5aceb29c17bc06ab686def28b6c8d0`.

The archive contains 191 files / 964973 bytes. Its complete scan records 190 / 964879 before adding the 94-byte scan receipt. An independent exact synthetic-canary scan found no match; that is not a comprehensive secret audit.

Required and executed IDs agree: **70 passed / 0 failed / 0 ignored**. There are 2 defect observations, 67 candidate subcases and 1 admission-only control. The earlier 51 ID/test/tier identities remain. Source comparison shows their existing test implementation was not changed in this continuation.

Downloaded catalogs independently confirm base 324/5/1, retained f74 anchor 362/1/1, candidate 375/1/1; no earlier passing identity regressed in those catalogs. Comparison with the archived immediate-start candidate catalog (374/1/1) also finds no formerly passing identity missing or nonpassing. That additional comparison is archived evidence, not a new matched execution. The feature-gated migration cases are in the required suite; ordinary comparison totals do not themselves exercise all candidate-only modules.

Final job metadata reports isolated tests/comparisons/scan/publication success and broader E2E/S7/LFS success. Ordinary CI still fails formatting and skips later steps. The integration failure and ignored conflict test remain visible.

I inspected the committed code, recovery audit, tests, and downloaded evidence. I did **not** rerun Cargo/SVN/Docker; they are unavailable in this runtime. I independently ran the exact committed v14 SQL (verified Git blob `f5c1a1f4ce7e12e1bb5f194ec209a4013e60148b`) on SQLite 3.46.1, plus explicitly labeled filesystem/SQLite and qualification-query component probes. These are not whole-Rust-engine or original-import-fixture executions. No production state was used.

## Recovery and accepted progress

The committed audit supports a limited conclusion: no interrupted migration implementation was found in the accessible locations, and known local state was preserved. I cannot repeat the search on Chris's machine or establish another machine's state. Do not claim Grok implementation was recovered, and do not spend another pass searching without new location/transcript evidence.

K02 preserves retained values and historical sequence state, including empty-used tables. K03 explicitly rejects half-present copy origin. Candidate modules are feature-gated; default initialization still calls the unchanged v12 registry. The copy tests add useful rollback, abrupt-exit, shape-refusal, and source-preservation evidence. These improvements should remain.

## L01 — P1: distinct copy paths can refer to the same physical database

**Source:** `candidate_migration.rs::{owned_root,manifest,CopySession::seal,check_files,migrate}`.

The boundary rejects overlapping canonical directory paths and symlinks, and compares hashes/modes. It does not establish that `source/reposync.db` and `copy/reposync.db` are separate files. Hard links are regular files; distinct paths can share a device/inode. `manifest` records bytes/mode, not storage identity or link count.

A hard-linked target therefore meets these path/type/hash predicates. Opening the target read-write also changes the source. The post-operation `source_unchanged()` check occurs after the per-version commit: detecting that damage afterward cannot make the operation source-preserving.

My disposable filesystem/SQLite probe creates distinct source/copy directories with a hard-linked DB, confirms the relevant predicates and common inode, writes only through the target, and observes the source version/hash change. This is a component demonstration plus source trace, not execution of `CopySession`.

**Required:** reject shared writable DB storage before any write-capable open or journal recovery. Compare storage identity, handle/reject hard-linked mutable DBs and unsafe aliases, and keep the trusted-directory/quiescence assumptions explicit. Do not silently unlink or replace user inputs. Add actual CopySession tests for a hard-linked source/target and an aliased target with an untouched outside canary; assert source/canary bytes and version never change, not merely that the call returns an error. Retain independent-copy success and existing symlink/WAL/refusal controls.

## L02 — P1: generation qualification does not consume all disqualifying legacy evidence

**Source:** `candidate_migration.rs::qualify_imported_pair`, `copy_migration.rs::pinned_unqualified_overlays`.

Qualification checks the SVN column and scoped **Git** cursor, applied rows, and disposable remote identities. It does not reconcile scoped incoming SVN cursor disagreement, relevant import progress/watermarks, or the existing `effect_unknown_<repo>` evidence shape.

The receipt exclusion checks `handled_git_baseline_<repo>` and `no_target_git_outcomes_<repo>`. Actual runtime no-target receipts use **`handled_git_no_target_<repo>_<sha>`**. The negative test named `v1` inserts the unused `no_target_git_outcomes_pair` key, so it does not establish rejection of real stored v1/v2/v3 receipts.

A labeled overlay can retain the qualifying Git objects, repository SVN=2 and mappings while setting scoped SVN=999, global SVN=888, import watermark=777, or inserting an actual-key unverified receipt. Those fields do not affect the current proof predicates. The design's classification table explicitly requires reconciliation for the 2/999/888/777 shape. A separate inventory report does not enforce that rule inside this admission function.

My component probe executes the relevant SELECTs and confirms the checked values/receipt count are unchanged by these overlays. This is not a complete endpoint qualification run; the implementing agent must run that regression.

**Required:** use one explicit decision over the supported legacy evidence vocabulary before granting a generation. Enumerate actual receipt keys with exact repository ownership, incoming cursor sources, progress and unknown-effect markers. Explain benign global references separately; never use a maximum or demand unrelated repositories share a cursor. For this narrow old-import qualifier, unsupported/conflicting evidence must refuse qualification while preserving raw legacy values and a read-safe disposition. Exercise actual-key and combined-cursor overlays through qualification AND migration, not only through inventory or a manually assigned disposition. Retain both independently qualified clean pairs and disabled-state behavior.

## L03 — P1: a later Git frontier can return to the already-handled baseline

**Source:** `candidate_authority.rs::advance_frontier` and `candidate/v14.sql`.

The writer checks that the supplied predecessor equals the current source and that the new source differs from the current source. SQL additionally requires increasing revisions only for the SVN direction. The initial Git baseline has no `pair_outcomes` row, correctly avoiding an invented outgoing SVN effect; consequently the outcome source-uniqueness constraint does not reserve that baseline.

Using the exact committed DDL and the writer's SQL/preconditions, my probe accepts baseline B → resolved E → an outcome-backed frontier B. It remains `authority_kind='outcome'`; no forbidden baseline replacement is attempted. Foreign-key and integrity checks pass. The published 13-negative matrix does not cover this path.

The same probe also demonstrates that an earlier resolved outcome becomes editable after the frontier advances: the immutability trigger only protects outcomes currently cited by a frontier. Such records are still historical authority and future reader evidence.

**Required:** reject returning to the already-handled baseline, repeated/resurrected source identities, and unsupported backward transitions. Distinguish predecessor-label equality from verified Git ancestry; do not claim arbitrary source ordering is proved by a string alone. Add actual public-writer and raw-SQL negative tests for known-baseline return, plus a normal two-step forward control. Make resolved historical evidence immutable after later advances (including replacement/deletion paths), or implement an equally explicit append-only rule. Pending-to-resolved mechanics remain separately scoped; this is not the general #64 service.

## Next pass: correction, then typed readers on copies

Correct L01–L03 first and run their tests. Then implement the proposed #54 lookup/list/status/emitted-hash adapters against migrated copies in the same pass; no intermediate planning-only gate is needed.

The current non-NULL `lookup_mapping` fast path ignores the requested generation and does not filter direction. Treat it as a prototype, not a canonical authority reader. Preserve legacy display values separately from generation-qualified results; test wrong/missing generations, both directions at the same SVN number, ownerless/NULL/duplicate rows, explicit evidence links, unresolved outcomes and applied-then-no-target emitted-hash behavior. No automatic `MAX(generation)` or global last-row authority.

Preserve all 70 cases, default v12 startup, original GOAL/briefs, isolated tooling, accurate comparison anchors, known failures and scanned evidence. Do not restart accepted F/G/H/I/J or K02/K03 work. Keep recovery complete for its accessible scope. No broader migration topology, general operation service, ordinary API/daemon activation, production access, reset/reimport fallback, force push, merge or deployment.

Return a compact delta handoff with actual L01–L03 reproductions/fixes, typed-reader matrix, unchanged source/ref evidence, precise candidate/final/CI identities, retained/new cases, comparison results and downloadable hashes. Separate executed proofs from source-traced claims. Stop at independent review.
