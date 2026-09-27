# RepoSync PR #72 — independent review 8

Posted on PR #72 as [review #5328886119](https://github.com/chriscase/RepoSync/pull/72#pullrequestreview-5328886119), anchored to `a934fa80a11840a92097269b9fd0e425112cbd5a`.

## Decision

**PARTIAL / CHANGES NEEDED for historical resolved-evidence immutability.** Accept L01/L02 and the demonstrated baseline-return, direct-mutation and bounded typed-reader controls. Retain the implementation. Correct **M01** below, then qualify the proposed nullable serialization and additional historical-reader cases in the same pass. No intermediate planning-only review is required.

No normal startup, operational API, scheduler, installer, generation activation, issue closure, merge, release, deployment or general #64 recovery is authorized. The actual deployed version remains NOT ESTABLISHED.

## Reviewed identities and scope

| Identity | Value |
| --- | --- |
| Repository / branch / PR | `chriscase/RepoSync` / `feature/reposync-reliability` / draft #72 |
| Original base | `87379741779a6259f7eeb52a68cc6f061174e5ef` |
| Immediate reviewed start | `9f6330e1e3dad7d8917b432d9d367665641a5227` |
| Functional / final / remote head | `a934fa80a11840a92097269b9fd0e425112cbd5a` |
| Tested synthetic merge | `008a572019b8c2a78aceff8284dfd18a958c6308` |
| Matching feature / merge tree | `13ce5fdf8a2fd0ce803ebd98221459e27789ae43` |
| Retained comparison anchor | `f74fce855a1f1d80dd631397f436ba33906272e6` |

Live GitHub metadata confirms the PR is open, draft and unmerged at the reported head. The comparison contains seven commits after the reviewed start. The changes focus on candidate-only storage/evidence/authority/readers, tests, the shared evidence vocabulary, documentation and qualification tooling.

The uploaded `review7-final-verification.json` and older `latest.md` describe the previous gate. They are not this run's receipt. Current results below were independently read from artifact **10915369356** and current GitHub source/metadata.

## Independently verified evidence

Downloaded artifact SHA-256:

`13e3450b478e3545dfaf2a48bd8773682f7e04bc6e286436440a65becad20a65`

Extracted summary SHA-256:

`6d69e8ccbc20beb18f1070b00f62f7b43cf0ff697693fd4bca2d6c38861e3aaa`

The archive contains 224 files / 1,102,859 bytes. Its recorded complete publication scan covered 223 / 1,102,764 before adding its own 95-byte receipt. An independent exact synthetic-canary scan of all archived files found no match; this is not a comprehensive secret audit.

The 78 required IDs equal the 78 executed IDs: **78 passed / 0 failed / 0 ignored**. Their tiers are 2 baseline defect observations, 75 candidate regression subcases and 1 admission-only control. The prior 70 ID/test/tier identities remain; this is not a claim that every source assertion was byte-identical.

Independent catalog comparisons reproduce all three empty regression lists:

| Executed comparison | Passed / failed / ignored |
| --- | --- |
| Original base | 324 / 5 / 1 |
| Retained f74 anchor | 362 / 1 / 1 |
| Immediate reviewed 9f head | 375 / 1 / 1 |
| Candidate | 375 / 1 / 1 |

The immediate comparison is now a matched execution, not merely an archived-results comparison. The feature-gated migration/readers are covered by the exact suite; ordinary catalog totals do not independently establish those paths.

Final job metadata confirms successful isolated qualification, comparisons, scans/publication and broader E2E/S7/LFS steps. Ordinary CI fails formatting, with later stages skipped. The integration failure and ignored conflict remain visible.

`artifact-verification.json` and `verify_artifact.py` preserve the reviewer's checks. The curated before-fix Rust excerpts are implementing-agent evidence, not reviewer executions.

### Review limitations

Cargo, SVN and Docker are not installed in this review runtime. I did not independently run the complete Rust, original-import, or container suites. I inspected the pinned source and downloaded evidence and executed the exact current v14 SQL on synthetic in-memory SQLite **3.46.1**. Reader effects in the probe use explicitly transcribed current `history`/`last_emitted` query semantics, not a Rust `CopyReaders` execution. No production state, credentials or active repositories were used.

## Accepted bounded progress

**L01:** `private_file` checks regular-file status, link count and device/inode; admission and pre-write checks revalidate recorded identities. The downloaded actual-API cases cover source/copy and canary aliases, replacement after seal, unchanged bytes/version, and independent-copy success. Accept under the declared private-directory/nonconcurrent fixture assumptions; concurrent-host fencing remains unqualified.

**L02:** the real qualifier now invokes the shared evidence decision before endpoint qualification, handles actual receipt prefixes, incoming/progress contradictions and unknown effects, and sets a read-safe disposition on refusal. The overlays exercise qualifier plus conversion; clean independent imports and disabled controls remain. Arbitrary historical schemas/ownership are not qualified by these fixtures.

**L03, partial:** baseline source reuse and direct mutation of resolved historical rows are now rejected. The ordinary two-step structural transition control remains. These do not prove arbitrary Git ancestry, and the conflict-resolution path below still violates the historical immutability guarantee.

**Typed readers:** explicit scope/generation/direction, separate raw legacy values, deterministic first-page handling of zero/negative IDs, linked handled-chain lookup, and applied-then-no-target last-emitted behavior are useful. Read-only byte/manifest controls support their tested boundaries. Preserve these tests and models; do not rebuild them.

## M01 — P1: UPDATE OR REPLACE can replace historical resolved evidence through an unresolved row

### Source

- `crates/core/src/db/candidate/v14.sql`: `outcome_resolved_immutable`, `outcome_resolved_no_delete`, `outcome_resolved_no_replace`, and the currently-cited protections.
- `crates/core/src/db/candidate_readers.rs`: `history` and `emitted_inner` consume historical outcome rows by source key after they cease to be the current frontier's directly cited outcome.

Exact SQL blob: `7895b10d4463c395aef0ff74ca6ac89b50808297`.

The UPDATE trigger checks whether **OLD** is resolved. The replacement guard is a **BEFORE INSERT** trigger. Updating a pending row with `UPDATE OR REPLACE` can collide with an earlier resolved outcome. The pending OLD row does not trigger the resolved-update refusal, the insert guard is not invoked, and conflict resolution can delete the historical victim before completing the update. With `recursive_triggers=0`, the implicit deletion does not execute the resolved-delete trigger.

The current frontier cites a later outcome; the historical predecessor relationship is a string, not a foreign key to the victim. Thus the current FK check need not reject this.

### Exact-schema reproduction

Create a valid structural history:

```text
Imported baseline B
  -> E: applied_verified, emitted SVN r3
  -> F: empty_no_target, current frontier

P: separately recorded pending outcome, outside the handled chain
```

Then update P with `UPDATE OR REPLACE` to conflict with E, by outcome ID or the unique `(repo_id,generation,direction,source_key)` key. The probe tests both replacement outcomes:

| Collision | Replacement kind | Actual result |
| --- | --- | --- |
| Historical ID | pending | Accepted; reader-query chain reaches unresolved evidence |
| Historical ID | applied_verified with target r99 | Accepted; last-applied query changes r3 -> r99 |
| Historical source key | pending | Accepted; reader-query chain reaches unresolved evidence |
| Historical source key | applied_verified with target r99 | Accepted; last-applied query changes r3 -> r99 |

For every accepted variant, `foreign_key_check` remains empty and `integrity_check` is `ok`. The current frontier F need not move. Direct UPDATE of E, DELETE of E and INSERT OR REPLACE of E are correctly refused by the controls, isolating the missing path.

This is an exact-schema finding, not a claim that the current normal transition writer emits this SQL, nor evidence of a production history change. It matters because the candidate contract promises historical resolved evidence cannot be overwritten and the new readers trust that evidence.

### Required correction

Protect resolved victims against conflict-producing UPDATE as well as INSERT/DELETE/direct UPDATE. Cover both unique collision keys and replacement rows that remain unresolved or become resolved. Keep nonconflicting unresolved bookkeeping and legitimate forward transitions usable. Do not solve this by inventing baseline effects, dropping constraints, disabling preservation checks or deleting history.

A solution relying on connection pragmas must establish them on every supported writer and prove their enforcement; a structural guard that also rejects the demonstrated `recursive_triggers=0` case is preferable. Do not claim a guarantee broader than the tested enforcement boundary.

Add actual Rust/raw-SQL candidate cases and re-open the real typed reader after each rejected attempt. Assert full historical rows and evidence bytes unchanged, no fake outcome committed, current frontier unchanged, last emitted remains r3, status/lookup remain consistent, and source/copy/endpoint invariants hold. Check all resolved kinds rather than just `applied_verified` as the protected victim. Retain the existing direct-mutation controls.

Primary explanation: SQLite's [ON CONFLICT documentation](https://www.sqlite.org/lang_conflict.html) describes REPLACE deletion during INSERT **or UPDATE**, and the recursive-trigger condition for deletion triggers. See `probe_review8.py` and `independent-probes.json` for the independently executed commands and outputs.

## Next bounded pass

Correct M01, then implement the requested copy-only nullable serialization / historical-reader qualification in the same run. Do not stop for another planning-only gate between them.

Use existing typed models/readers as the authority source for a documented DTO/serialization adapter; do not change live routes or startup. Preserve the distinction between raw legacy data and canonical qualified results, no target versus missing, unqualified scope, and pending/unknown/nonhandled evidence. In particular, a canonical `Missing` result means no proved handled result; an API consumer must not infer that no pending/unknown record exists. Preserve or expose the separate nonhandled state explicitly in the proposed contract.

Test both directions, wrong/missing generations, nullable/ownerless/malformed/duplicate legacy values, signed pagination IDs, applied-then-no-target and existing pending/effect-unknown cases. Assert serialized null/state tags and consumer parsing against literal expected payloads, not only successful `serde_json` calls. Scope the boundary as API-ready DTO qualification or a test-only router, not operational API activation. Existing v12 live response contracts remain unchanged.

Do not expand into general #64 recovery, arbitrary history proof, broad formatting cleanup, production enrollment or another recovery search. Retain all 78 exact cases, the four clearly named comparison identities, scans, source preservation and v12 startup checks. Add only cases needed for this correction and DTO contract.

## Next handoff

Return compact deltas: M01 fail-before/pass-after and preservation/reader checks, new JSON decision matrix and literal examples, retained/new exact IDs, comparison outcomes, startup/API nonactivation evidence, functional/final/CI SHAs and tree, artifact IDs/hashes and actual remaining gaps. Keep PR #72 draft and stop at the independent review gate.
