# PR72 review9 compact delta handoff

Bounded M01 correction plus nullable DTO/historical-reader qualification. PR remains draft on `feature/reposync-reliability`. Starting reviewed head `a934fa80a11840a92097269b9fd0e425112cbd5a`; original base `87379741779a6259f7eeb52a68cc6f061174e5ef`; retained f74 anchor `f74fce855a1f1d80dd631397f436ba33906272e6` is not the immediate predecessor. Exact functional/final/remote/CI merge/tree and downloaded receipts are reported in PR72 and the return, without recursive receipt-only commits.

## Preservation and correction

Original GOAL SHA-256 `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`; dependency lock `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05`; exact current brief `66ec0ec28b1e88f7f753792238abcb5b8a05882bff6f75a155ba84939633f6de`; previous brief `7f108a2d3ad8c212cc0db4049d1a8672e9dd750c6107ca9bf7856e8257a6e8f0`. Original/prior briefs and [previous handoff](review-8-handoff.md) remain byte-exact. Acceptance criteria and issues are unchanged. Accessible recovery search was not repeated.

Actual before-fix Rust test on reviewed runtime committed a source-key UPDATE OR REPLACE from pending to applied r99. Reopened actual CopyReaders lookup and last_emitted returned r99 rather than historical r3. [Curated failing reproduction](review-9-reproduction.json) retains command, runtime/SQL identity, result0/1/0 and raw-log hash; no large DB dumps are published. Reviewer exact-schema/transcribed-query probes are [separate evidence](review-8-independent-probes.json), not substituted for the Rust integration.

A residual actual Rust probe on intermediate71c demonstrated hidden-rowid replacement deleting the historical source and breaking reopened readers. Candidate-only outcome storage now uses WITHOUT ROWID to remove that unguarded identity; legacy commit_map IDs/sequences remain unchanged. M01 uses a prospective resolved-victim BEFORE UPDATE guard, independent of recursive-trigger configuration. Existing direct UPDATE/DELETE and INSERT guards remain. **224 rejections** cover two recursive settings × four resolved kinds × current/historical victims × both explicit conflict keys plus hidden-rowid attempts × both replacement states/methods, plus direct UPDATE/DELETE. Exact explicit-key guard-error and unsupported-rowid error checks prevent unrelated SQL errors satisfying the replacement oracle. Both explicit conflict keys plus attempted hidden-rowid replacement are covered. Every rejection preserves complete rows/evidence/frontiers/counts and DB bytes, and real reopened lookup/status/emitted results retain r3. Transaction rollback stays usable; nonconflicting pending UPDATE OR REPLACE bookkeeping and ordinary forward transitions pass. Superseded prototype shape refuses migration/read without repair. Source/config/refs and endpoints remain unchanged.

## DTO and exact cases

[Copy DTO contract/decision matrix](../serialization-contract.md) uses `reposync.copy_read.v1` over the existing readers, compiled only with `reliability-fixture`. [26 literal payloads](../../../crates/core/tests/fixtures/copy-reader-v1.json), SHA-256 `0ac640a1899881fc195ec6d21bbc6df765b92ca96f08d5b2ac29c67bbca7edc9`, are independent expected data. Actual payload equality, consumer decode and reserialization are asserted.

| Added ID | Test / delta |
| --- | --- |
| M01_HISTORY | m01_conflict_history_preservation: SQL matrix, full preservation, reopened reader results and ordinary controls |
| S54_JSON_LOOKUP | nullable_dto_lookup_matrix: qualified mapped/no-target versus unresolved NULL/ownerless/malformed; wrong/missing generation, disabled/not-qualified; missing still exposes pending/unknown |
| S54_JSON_HISTORY | nullable_dto_historical_matrix: both directional histories, current target versus last recorded target, pending/effect-unknown; failed M01 mutation cannot change DTO history |
| S54_JSON_PAGE | nullable_dto_pagination_matrix: signed/zero IDs, NULL/blob/finite/infinite REAL, duplicates/ownerless, every retained ID and deterministic empty page |
| S54_JSON_READONLY | nullable_dto_readonly_matrix: repeated DTO/consumer/error paths preserve copy/source/endpoint bytes/version; bad schema/operation/null mapped target refuse |

All78 prior required ID/test/tier objects and strict source oracles remain; added5, total83. Initial new literal author expectations were corrected from NULL to the unchanged schema's NOT NULL empty-text default; no old test or runtime behavior was weakened. Targeted DTO4/0/0, read-safe lookup phase1/0/0, authority4/0/0, retained L02 admission1/0/0 and default startup1/0/0 passed. Final complete required qualification and accurately named original/f74/immediate-a934 comparisons are reported from downloaded final-tree CI evidence.

`Canonical::Missing` is absence of proved handled authority, not absence of pending work or permission to retry. Unqualified scope makes nonhandled visibility unavailable. NULL remains explicitly tagged, separate from empty text; target absence is JSON null, not zero/empty SHA. Blob hex and IEEE-754 bit tags preserve unusual raw values. No new authority algorithm, remote commands, migration or operational routes are added by serialization.

## Commands and publication

- `REPOSYNC_OLD_GENERATOR=<pinned original873> cargo test -p reposync-core --features reliability-fixture --locked --test copy_migration m01_conflict_history_preservation -- --exact --nocapture --test-threads=1`
- Same generator: `cargo test -p reposync-core --features reliability-fixture --locked --test copy_migration nullable_dto -- --nocapture --test-threads=1`
- `cargo test -p reposync-core --features reliability-fixture --locked --lib candidate_authority -- --nocapture --test-threads=1`
- `cargo test -p reposync-core --locked --test startup_schema -- --nocapture`
- `scripts/reliability-container.sh --all`: full exact83 plus six sandbox canaries and internal scan.
- `scripts/reliability-compare.sh`: original873, retainedf74, immediate-a934, candidate under the same lock/overlays/method. Feature-gated paths are proved by the exact suite, not default catalogs.
- Complete host/publication and independent downloaded evidence scans; ZIP digest plus key-file hashes in PR72/return. Literal JSON and local before-fix excerpt are downloadable with the CI evidence.

Known integration failure `integration::test_full_svn_to_git_cycle_with_metadata`, ignored `team_mode_e2e::test_team_mode_conflict_detection`, and ordinary CI formatting failure remain visible. Earlier evidence is referenced through the archived handoff rather than repasted here.

## Boundary and next review

Normal startup, operational API, daemon, installer and scheduler remain at reviewed v12. L01/L02, baseline-return and direct historical guards, original readers and copy-only migration remain. Structural later fixture hashes/targets qualify SQL/reader/DTO behavior, not new external effects or arbitrary Git ancestry. No production, resets/reimport recovery, general64 service, force push, merge, release or deployment.

Deployed installations, arbitrary historical topology/policy/ancestry, production identity, concurrent writer fencing/backup ownership, credential variants, live acceptance, down migration and general64 recovery remain unqualified. Smallest proposed next slice after review: additional copy-only historical-topology DTO qualification; operational activation requires its own reviewed boundary. Stop at independent review.
