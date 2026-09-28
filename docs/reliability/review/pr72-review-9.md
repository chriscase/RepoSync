# PR #72 independent review 9

Posted as PR #72 review **#5331642648**, anchored to `5e10a89db30dc742b32e43b0e0274f22c651f028`.

## Decision

**M01 and the demonstrated offline serialization slice: ACCEPTED FOR THEIR TESTED BOUNDS.** Retain the outcome replacement guards, candidate `WITHOUT ROWID` outcome storage, the 26 literal payloads, and all 83 required cases. Overall candidate activation remains **HOLD**. One related schema finding, **N01**, needs correction before the checkpoint-storage guarantees are accepted.

Proceed with a finite **frontier-identity correction and one end-to-end copy-only read transport/consumer qualification**. This is not another open-ended DTO-variant campaign. There is no intermediate planning-only gate. Normal application startup, operational routes, daemon, installer and scheduler must remain at the existing v12 activation boundary. No production access, merge, issue closure, release, deployment, checkpoint reset/reimport recovery, or general #64 service is authorized.

Reviewed identities:

| Identity | SHA |
|---|---|
| Original base | `87379741779a6259f7eeb52a68cc6f061174e5ef` |
| Immediate reviewed start | `a934fa80a11840a92097269b9fd0e425112cbd5a` |
| Functional/final feature | `5e10a89db30dc742b32e43b0e0274f22c651f028` |
| Tested PR merge | `26e5e5d8e656f325d51935d84e07d951bbd7518e` |
| Matching tree | `92e8db80fb0a7e0add3c09db9daf496bc15a933e` |

## Independently checked evidence

Downloaded artifact `10936434108` hashes to `64c3fd3d119721a896ab4faf336eb9ba6dbe19d475a92b61b16fe81ec88f1833`, matching GitHub's API digest. Summary hashes to `dcfb049abc3096bba25238a0606cda71680581cc7baf15817e5587c86b1f18a6`. All three comparison and complete-scan hashes match the PR. The archive contains 236 files / 1,182,250 bytes. Publication scanning preceded its own 95-byte receipt: 235 / 1,182,155. My independent exact synthetic-canary scan found no match; this is not a comprehensive secret audit.

Required and executed IDs match: **83 passed / 0 failed / 0 ignored**, with 2 baseline defect observations, 80 candidate subcases, and 1 admission-only case. All previous 78 ID/test/tier identities remain. Source review covers the changed tests; identity comparison is not a claim that every assertion in the repository was byte-audited.

The downloaded catalogs independently reproduce original base **324/5/1**, retained `f74fce8` **362/1/1**, immediate `a934fa8` **375/1/1**, and candidate **375/1/1**. No formerly passing identity became missing/nonpassing in any comparison. All catalog GOAL and lock hashes match. Candidate-feature coverage comes from the required suite, not the default totals.

GitHub metadata confirms final isolated qualification/comparison/scanning/publication and broader E2E/S7/LFS step success. Ordinary CI still fails formatting and skips later steps. The metadata integration failure and ignored team-conflict case remain visible.

I verified the artifact's literal JSON file against the committed Git blob `19aa2848c4ce58970e14aed6d2ab19cc2c4cd6ad`. It contains 26 named full payloads. The new consumer adapter is feature-gated offline code over the existing readers, not a live API rollout.

**Execution limits:** Cargo, SVN and Docker are absent in this review runtime. I did not independently rerun their suites or original-import fixtures. I ran exact-current-schema SQLite 3.46.1 probes with synthetic in-memory data. The repeated M01 query results use the previously documented transcription of reader queries, not Rust `CopyReaders`. The agent's before-fix Rust and 224-attempt evidence remain implementing-agent evidence, inspected rather than rerun.

## Accepted progress

The exact v14 SQL is Git blob `441cb08442a64564f9c6946a81822085d10e6d20`. My four previous UPDATE OR REPLACE probes now reject both unique-key collisions and both pending/applied replacements, preserve all outcome rows, and retain the transcribed last-applied SVN target r3. Direct UPDATE/DELETE/INSERT OR REPLACE controls also reject. The hidden outcome rowid no longer exists. Downloaded M01 evidence contains 16 combinations / 224 rejected attempts and reopened actual Rust reader checks.

The DTO makes no-target, unresolved legacy evidence, missing handled authority and nonhandled records distinct. Literal expected JSON, decode/round-trip tests, both directions, pagination, NULL versus empty text, and read-only file preservation are useful. The decoder's declared scope is transport/type validation, not authentication or proof of external effects. Do not reinterpret it as such.

The accepted L01/L02, sequence/copy-origin, restart/rollback and startup-nonactivation cases should not be restarted. Recovery search is complete for its accessible scope and is not part of this continuation.

## N01 — P1 before activation: frontier rows still have an unguarded replacement identity

**File:** `crates/core/src/db/candidate/v14.sql`, `pair_frontiers` and `frontier_initial` / `frontier_advance` / `frontier_no_delete`.

The M01 change removes implicit rowid from **pair_outcomes**, but **pair_frontiers** remains a rowid table. Its INSERT/UPDATE guards validate declared repository/generation/direction identities and a valid baseline or resolved transition. They do not guard an implicit rowid collision with a different frontier. No other table foreign-keys to the victim frontier.

Independent exact-schema reproduction with foreign keys ON and recursive triggers OFF:

1. Create separately owned lineages A and B. A has a legitimate initial outgoing frontier.
2. `INSERT OR REPLACE` B's legitimate baseline frontier, explicitly supplying A's rowid.
3. B satisfies the new-row baseline guard. Conflict resolution removes A's frontier; `frontier_no_delete` does not run for that implicit deletion in this configuration.

A second variant first creates B's frontier and valid next outcome, then performs a valid forward `UPDATE OR REPLACE` for B while supplying A's rowid. It also removes A's frontier.

Both variants succeed, leave **foreign_key_check empty / integrity_check ok**, and remove A's checkpoint without granting A a replacement. With recursive triggers ON, both refuse with `frontier cannot be deleted`.

This is **not a failed M01 outcome fix** and not evidence that the current checked writer emits these statements or that production was affected. It is a neighboring gap in the claimed schema-level prohibition on frontier deletion/replacement. A missing frontier also makes the current status/history reader unable to retrieve that direction. That reader consequence is source-traced, not independently executed in Rust here.

Required correction: remove or protect every implicit frontier identity at the schema boundary. `WITHOUT ROWID` is one option for this candidate-only composite-key table; another solution must enforce its configuration assumptions at every supported writer. Preserve legacy AUTOINCREMENT tables and IDs. Test both demonstrated statements and implicit-key aliases, both directions and independent repositories/generations, plus legitimate baseline creation and forward transitions. Reopen actual readers/DTOs and prove both pairs' frontiers/status remain intact. Add a focused identity checklist for the candidate authority tables so the same replacement class is not rediscovered table by table. Do not change unrelated schema or make raw DDL a supported user operation.

Sources explaining the mechanism: SQLite's [WITHOUT ROWID documentation](https://www.sqlite.org/withoutrowid.html) and [ON CONFLICT documentation](https://www.sqlite.org/lang_conflict.html). `probe_frontier_identity.py` and `frontier-identity-probes.json` preserve the exact commands and results.

## Next finite milestone

Correct N01 first. Then connect **one read-only copy-inspection route family** to the existing CopyReaders/DTOs in a test-only/feature-gated web adapter and exercise a real consumer, rather than adding only more same-model Rust JSON round trips.

Use an in-process request/response path: no live listener or operational route activation is necessary. Begin from the pinned original topology, copy/migrate, then request lookup/list/status/last-emitted. Reuse existing request/auth validation where practical; isolate any authentication-store writes from the immutable candidate database and label unsupported authorization behavior honestly. A global legacy page is privileged diagnostic display, not an accidental cross-repository response. Do not introduce a user-controlled filesystem path or permit any write operation.

Test one actual JavaScript/TypeScript consumer of emitted payload bytes, including nullability, explicit scope, missing-with-unknown, applied-then-no-target, and signed pagination. The v1 contract requires i64 precision: a browser Number round trip is not proof for all i64 values. Add a lossless consumer or explicit safe refusal before returning an inaccurate cursor; any wire-version change must be explicit and preserve existing v1 fixtures. This is additional activation-prerequisite coverage, not a newly asserted failure of the current Rust DTO contract.

Retain v12 route shape/status/auth behavior under the default build and prove candidate routes are absent. Old source/copy/config/ref/endpoint preservation remains mandatory. Do not attempt general #64 recovery, canonical sync activation, arbitrary-history automatic migration, or production qualification.

Return a compact handoff: N01 before/fix tests, candidate identity matrix, exact route/consumer entry points, default absence and v12 compatibility proof, end-to-end data decisions, new/retained cases and three comparisons, exact heads/tree and downloadable evidence. Freeze passing work and stop; no speculative additional-topology campaign or recursive documentation-only qualification loop.
