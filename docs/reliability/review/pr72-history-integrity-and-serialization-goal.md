# /goal — protect historical evidence, then qualify nullable serialization on copies

## Identity and authority

Repository: `chriscase/RepoSync`.
Branch: `feature/reposync-reliability`. Draft PR: **#72**.
Reviewed start: `a934fa80a11840a92097269b9fd0e425112cbd5a`.
Original main/base: `87379741779a6259f7eeb52a68cc6f061174e5ef`.
Retained comparison anchor: `f74fce855a1f1d80dd631397f436ba33906272e6`.
Original GOAL SHA-256: `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`.
Existing reviewed brief SHA-256: `7f108a2d3ad8c212cc0db4049d1a8672e9dd750c6107ca9bf7856e8257a6e8f0`.
Dependency lock SHA-256: `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05`.

Read `RepoSync-PR72-review-8.md` and posted review **#5328886119** on PR72 anchored to this start. Preserve this goal byte-for-byte in `docs/reliability/review/`, without overwriting original GOAL, prior briefs or handoffs. Check current branch/local changes before editing; preserve concurrent or unexpected work and report material drift. Do not repeat the completed Grok recovery search without new evidence.

## Outcome and scope

Accept and retain L01/L02, baseline-return prevention, direct historical-mutation guards, copy-only migration, K02/K03 and the bounded typed-reader implementation. Historical immutability is still PARTIAL because **M01** permits an UPDATE OR REPLACE collision through an unresolved row.

First correct M01 and prove historical rows/reader results remain unchanged on rejection. Then qualify nullable API serialization and additional historical-reader cases using the current copy-only models. These two stages are authorized in one implementation pass; no intervening proposal-only review is needed.

No activation: normal Database::initialize, installer, daemon, scheduler and operational API routes remain on the reviewed v12 behavior. No general #64 operation/recovery service, production access or live acceptance.

## Stage A — M01 historical conflict-resolution protection

Read the review's exact-SQL probe and source at `crates/core/src/db/candidate/v14.sql`. The probe's Git blob is `7895b10d4463c395aef0ff74ca6ac89b50808297`.

The current BEFORE UPDATE trigger checks whether OLD is resolved. A pending OLD row can use UPDATE OR REPLACE to collide with and implicitly delete a different historical resolved row. The BEFORE INSERT replacement guard is not sufficient for this operation. With recursive_triggers=0, the implicit deletion bypasses the resolved-delete trigger. The current frontier can still cite a later outcome, so the FK check stays clean.

Reproduce through the actual candidate test infrastructure before fixing:

1. Create a valid lineage/baseline B and advance using the public writer to resolved E emitting SVN r3, then a no-target F. F is current, E is historical.
2. Add a clearly labeled pending P outside the handled chain.
3. Attempt UPDATE OR REPLACE on P so it collides with E by ID, and separately by the unique repository/generation/direction/source key.
4. Test a replacement that stays unresolved and a replacement claiming applied_verified with another target such as r99.
5. Before the fix, preserve a concise failing reproduction. Do not publish large raw DB/secret dumps.
6. Correct the database/writer enforcement so every supported conflict path refuses to overwrite resolved historical proof before any damage is committed.

Implementation choice is yours: protect conflicting victims explicitly, make source identities immutable where justified, or another equivalently enforced design. Do not rely only on application convention. Do not simply turn off the negative test or change the claim to permit lost history. A connection-pragma approach must prove enforcement for every supported writer; prefer protection that also withstands the demonstrated recursive_triggers=0 case.

### Required negatives and positive controls

Cover ID and source-key collisions; historical victims of each resolved kind; replacement via INSERT OR REPLACE and UPDATE OR REPLACE; ordinary direct UPDATE/DELETE; current as well as historical referenced evidence where relevant. Test both recursive-trigger settings or clearly enforce/refuse the unsupported configuration before write. Preserve valid nonconflicting pending bookkeeping and ordinary two-step forward transitions. Repeated/backward baseline checks remain required.

For each rejected operation assert the historical row values/evidence, pending row, frontier, and outcome counts are unchanged; a fake target has not been committed. FK/integrity checks are supplementary, not the only oracle. Verify transactional rollback leaves the connection usable.

Reopen the **actual CopyReaders API** after the failed mutation and verify lookup/status/last_emitted still describe the original history and target r3. Do not substitute only translated SELECTs for this integration test. The independent review probe is component evidence, not a completed Rust regression.

Changes remain in the same reusable candidate SQL/writer. Existing prototype copies with a superseded physical schema must be refused, not silently repaired. Fresh qualification copies come from the preserved pinned v12 source; that is test setup, not reset/reimport of active state.

## Stage B — copy-only historical evidence and nullable serialization

Build on `candidate_readers.rs`; do not create an alternate authority algorithm. Produce an explicitly versioned/documented DTO or serialization adapter suitable for eventual API use, compiled/tested without activating existing endpoints. A test-only router is optional, not permission to change live routes.

The serialized contract must preserve:

- explicit requested repository/generation/direction and qualification state;
- raw legacy evidence separately from proved baseline/outcome authority;
- mapped target versus proved no-target, missing canonical result, unresolved legacy evidence and unqualified scope;
- actual current target versus the last recorded applied target after no-target work;
- pending/effect-unknown/nonhandled records without accidentally presenting them as a handled result;
- nullable mapping values, ownerless records, signed/zero pagination IDs and deterministic ordering.

A `Canonical::Missing` result is absence of a proved handled result, not proof that no pending/unknown record exists or that a retry is safe. State this distinction and preserve the relevant separate status in the DTO. Do not weaken canonical authority to make a display row look resolved.

Use literal expected JSON payloads and consumer-deserialization assertions, not just `to_string(...).is_ok()`. Show representative success, no-target, unresolved-null, ownerless, wrong-generation, disabled and pending/unknown examples. Verify null is not serialized as an empty-string SHA, omitted as an accidental missing outcome, or converted to a fabricated zero revision. Give raw unusual legacy values an explicit tagged representation or explicit unsupported response; never silently drop rows or serialize a secret-bearing internal dump.

Retain both directions sharing the same SVN revision, duplicate/conflicting legacy rows, explicit evidence links, applied-then-no-target, and negative/zero ID first-page tests. Add targeted historical-chain assertions around M01. All read/serialize operations remain offline and preserve complete source/copy DB bytes/version, config/refs, and fixture endpoints.

No generation inference from MAX, last row, equal byte trees, or untrusted labels. No new arbitrary ancestry or production qualification is claimed.

## Validation and iteration

Retain all **78** required case identities and strict oracles, including earlier migration interruptions/sequence/null checks, copy boundaries, startup nonactivation and readers. Add exact required IDs for M01 and DTO cases. Include meaningful before/after source identities; do not record an expected defect observation as candidate success.

Use targeted tests while iterating, then one final full qualification at the functional tree. Keep original-base, retained-f74 and immediate-reviewed-start comparisons accurately named. Preserve existing known integration/formatting failures and the ignored conflict case; unrelated broad cleanup is not part of this pass.

Keep internal/host/publication evidence scans and artifact provenance. Stop instead of repeatedly running unchanged full suites or committing recursive handoff/checksum-only updates. Preserve a useful partial handoff if an actual blocker or budget boundary prevents completion; do not self-approve or change acceptance criteria.

## Restrictions

No production endpoints/credentials, active installation writes, force pushes, checkpoint resets, reset/reimport recovery, general #64 service, operational API/startup/schema activation, merge, release, deployment or issue closure. No recovery search repetition. No changing unrelated branches. Only disposable fixture copies and read-only test serialization are in scope.

## Required return handoff

Return READY_FOR_REVIEW only for the completed bounded assignment; otherwise return an honest PARTIAL/BLOCKED result.

Include: exact start/functional/final/remote/CI merge/tree SHAs; goal/brief/lock hashes; M01 reproduction and correction with actual API preservation results; SQL conflict/recursive-trigger matrix; JSON contract/version and literal examples; scope/nonactivation proof; new/retained case counts and three comparisons; current known failures/limitations; artifact/run IDs and downloadable ZIP/key-file hashes. Link to committed code/tests and a compact per-case report, not another repeated full history.

Keep PR72 draft. Stop for independent review. Deployed installations, remote production identity, arbitrary legacy history/policy, writer fencing, credential variants, live acceptance and general #64 recovery remain unqualified.
