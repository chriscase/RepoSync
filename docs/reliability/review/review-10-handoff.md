# PR72 review10 compact delta handoff

This bounded pass starts at reviewed `5e10a89db30dc742b32e43b0e0274f22c651f028` on `feature/reposync-reliability`, draft PR72. Original base `87379741779a6259f7eeb52a68cc6f061174e5ef`; retained f74 anchor `f74fce855a1f1d80dd631397f436ba33906272e6`; immediate comparison is **5e10a89**, not a934. The exact final functional/PR/CI merge/tree identities and downloaded artifact digest belong in the PR handoff after publication, not in a recursive SHA-only commit. [Review9 handoff](review-9-handoff.md) retains earlier accepted evidence.

Original GOAL SHA256 `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`; locked dependency graph `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05`. Original GOAL, previous briefs, the 26 literal v1 payloads and all83 prior required ID/test/tier objects are unchanged. New cases N01_FRONTIER and S54_HTTP_COPY bring the required manifest to85.

## N01: actual failure, correction and controls

[Curated local reproduction and raw logs](review-10-reproduction.json) separate reviewed-schema failure from corrected-schema success. Before correction, an actual Rust candidate test using a pinned-old-import fixture committed an `INSERT OR REPLACE` baseline for a second pair with the first pair's hidden frontier rowid; the no-loss assertion failed (0/1/0). The reviewed independent SQLite probes also cover a valid forward UPDATE OR REPLACE variant. The correction makes **all five new candidate authority tables** `WITHOUT ROWID`; [the identity checklist](../frontier-identity.md) enumerates their declared conflict keys and supported writer boundaries. Legacy AUTOINCREMENT IDs and v12 remain unchanged.

The corrected actual candidate case passes 1/0/0. Its 48 attempts span baseline INSERT and valid forward UPDATE, all rowid/oid/_rowid_ aliases, both directions, separate repositories/same-repository generations and recursive triggers off/on. Each refusal preserves full outcome/frontier values, DB bytes, subsequent connection usability, and actual reopened lookup/status/last-emitted DTOs for both old-imported pairs. Qualified baseline creation and public-writer forward transitions pass. A superseded rowid-backed v14 frontier shape refuses migration/read without repair. New generation-two rows and subsequent hashes are explicitly structural fixtures, not evidence of new remote effects or Git ancestry.

## One complete copied read journey

[The read adapter and boundary](../copy-read-transport.md) describe four feature-gated `GET /__reliability/copy-read/{lookup,list,status,last-emitted}` paths over existing CopyReaders/DTOs. The pinned original importer completes first; old writers exit; copies are independent and sealed. Separate in-memory v12 auth state validates sessions. Axum requests are in-process with no candidate listener. No global page, request-selected path, mutating operation or alternate authority query is exposed. Current session validation has no repository grants, so this is not deployment authorization proof.

One local end-to-end test passes 1/0/0: old import → copy migration 12→14 → 30 real HTTP responses → actual Node consumer. It checks both-direction mapping/no-target; applied-then-no-target; missing with pending/effect-unknown; NULL versus empty, ownerless/duplicate display; generation/disabled/unqualified scopes; signed/zero pagination and empty page; invalid auth/input/method; v12 handler response and candidate-route absence. Unsafe positive and negative i64 cursors are explicitly refused **before** `JSON.parse` rounds them. The existing v1 wire version and 26 literal payloads are unchanged. CI writes all response bytes as `S54_HTTP_COPY-wire.json` for independent inspection. Source/candidate/config/ref/endpoint file bytes and modes remain equal across requests.

## Commands and evidence boundary

- `REPOSYNC_OLD_GENERATOR=<pinned original873> cargo test -p reposync-core --features reliability-fixture --locked --test copy_migration n01_frontier_identity_preservation -- --exact --nocapture --test-threads=1`
- `REPOSYNC_OLD_GENERATOR=<pinned original873> cargo test -p reposync-web --features reliability-fixture --locked --test copy_inspection copy_only_http_to_javascript_journey -- --exact --nocapture --test-threads=1`
- `cargo test -p reposync-core --locked --test startup_schema -- --nocapture`; default web compilation lists zero feature-gated copy-inspection tests.
- `scripts/reliability-container.sh --all` runs every required case and six fixture boundary canaries in the sealed runtime; `scripts/reliability-compare.sh` measures original/f74/immediate5e/candidate with the same lock and runtime method.
- Scanner status, exact executed/required IDs, raw logs, generated HTTP bytes, comparison catalogs and downloaded ZIP hashes are required before claiming final qualification.

The known metadata integration failure, ignored team-conflict case and ordinary formatting failure remain visible. Local Docker was not used for the final gate; the PR's isolated CI supplies that qualification. No production, checkpoint reset/reimport recovery, general #64 service, recovery search, force push, merge, release or deployment. Normal startup/routes/daemon/installer/scheduler stay v12. Stop at independent review; operational activation and authorization require later explicit review.
