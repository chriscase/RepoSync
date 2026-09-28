# RepoSync PR #72 — review 10

Posted as PR review **#5334892010**, anchored to the reviewed final head.

## Decision

**ACCEPTED FOR THE TESTED BOUNDS: N01 and the in-process copy-read/JavaScript milestone.** No new blocking defect was found in that bounded delta. Freeze and retain it. This is not whole-PR approval or an assertion that the original reliability epic is complete.

Continue with one finite candidate-only repository-read authorization gate, plus the two small evidence refinements below. Default startup, operational routes, daemon, installer and scheduler remain at v12. No merge, release, deployment, issue closure, production access, checkpoint reset/reimport recovery, or general #64 service is authorized.

Reviewed identities:

| Role | SHA |
| --- | --- |
| Original base | `87379741779a6259f7eeb52a68cc6f061174e5ef` |
| Immediate reviewed start | `5e10a89db30dc742b32e43b0e0274f22c651f028` |
| Functional adapter/consumer commit | `41edf4be3b1d1b43ea4bb1726e2f7f61e0db0c77` |
| Final reviewed head | `ed2f1a4d73b63716c71b360115de321d7fa7f16c` |
| Tested PR merge | `f325b33c5c63e332a225717a561345fc01f9cb7c` |
| Matching feature/merge tree | `0daf3cbec27699fbae3be13c8a797b87670f263d` |

## Independent checks

Downloaded artifact **10946696049**, run **36362630765**, independently hashes to:

`cc2a80f6329a8675cb54b682eca48b8f9bfa02cd79e3029ca82f8f3060c653fb`

GitHub's artifact metadata reports the same digest and expiration `2026-10-12T00:54:09Z`. The archive has 247 files / 1,243,736 bytes. Its publication scan precedes its own 95-byte receipt: 246 / 1,243,641. The summary, N01 and HTTP case files, wire payloads and comparison hashes match the handoff. Three curated compressed/decompressed log hashes also match. Review-side exact-canary scans of archive and decompressed logs found no match; this is not a comprehensive secret audit.

Required and executed identities match **85 passed / 0 failed / 0 ignored**: 2 baseline-defect observations, 82 candidate regression subcases, 1 admission control. All previous 83 case ID/test/tier identities remain. The downloaded original, retained f74, immediate-start and candidate catalogs independently recount to 324/5/1, 362/1/1, 375/1/1 and 375/1/1. No formerly passing identity becomes missing or nonpassing in any comparison. Candidate-feature coverage comes from the exact suite, not the ordinary test totals.

The exact artifact consumer has Git blob `de7e125844b575292807be37109fdcc934baacef`, matching the committed script. **I ran that unmodified Node consumer on the downloaded Axum wire bytes:** all 30 responses passed, including two unsafe-i64 refusals before numeric rounding. This was replay of emitted HTTP bytes, not a new server request or a browser UI test.

I also ran the exact current v14 SQL, Git blob `7935d3d5e3081267d1bca8e20fe56329db46edf4`, on synthetic in-memory SQLite **3.46.1**. All 48 frontier alias attempts rejected with complete table/SQLite-byte preservation; the corresponding 48 valid baseline/forward controls succeeded. The five candidate authority tables are WITHOUT ROWID. This independently checks the SQL boundary; it is not Rust CopySession/CopyReaders execution.

Cargo, SVN and Docker are unavailable here. Their published runs were inspected, not independently rerun. Final GitHub job metadata reports isolated qualification/comparisons/scanning/publication and broader E2E/S7/LFS success. General CI still fails formatting and skips later stages. The known metadata integration failure and ignored team-conflict case remain visible. Implementing-agent fail-before Rust logs remain implementing-agent evidence, albeit hash-verified here.

See `artifact-verification.json`, `node-consumer-replay.json`, `n01-independent-probes.json`, and the included reproducible scripts.

## Accepted implementation

### N01

`crates/core/src/db/candidate/v14.sql` removes implicit identities from all five candidate authority tables. Existing baseline/forward/evidence guards remain; legacy `commit_map` storage is unchanged. The actual candidate test covers aliases, directions, separate repositories/generations, recursive-trigger settings, reopened readers/DTOs, positive controls and superseded-schema refusal. The uploaded `frontier-identity.md` is an appropriately bounded identity checklist, not authorization for arbitrary SQL/DDL clients.

### End-to-end read milestone

`crates/web/src/api/copy_inspection.rs` is a feature-gated in-process route family. It uses the existing CopySession/readers/DTOs and separate disposable authentication state. It does not expose a global legacy page, filesystem-path selection or writes. The old importer produces the source installation before copy migration; later applied/no-target/unknown fixtures are correctly labeled structural, not newly verified remote effects.

The Node consumer checks typed decisions, scope, raw data, signed values, current versus last-emitted targets, and explicit refusal of unsupported integer ranges. Full-range browser i64 support is not claimed. This meets the finite prototype milestone; do not restart it or create another open-ended topology/DTO campaign.

## Remaining boundary to implement next: authorization

The adapter's `authenticate` returns only session validity. It does not establish an actor's permission to read a particular repository. This limitation is acknowledged in the current contract, so it is **an unimplemented next gate, not a newly discovered regression in N01**.

Two separations are essential:

1. **Authentication versus authorization.** Resolve an actual active principal, bind grants to the enrolled copy and exact repository ID, and check permission before opening/reading the candidate for that request. Do not infer grants from names, `created_by`, credentials, branch ancestry, repository enabled state, or a successful legacy session check. Existing role/session helpers are not by themselves the new permission model.
2. **Canonical authority versus data visibility.** Current candidate lookup/list deliberately include `(repo_id = requested OR repo_id IS NULL)`. Marking an ownerless row unresolved does not make disclosure harmless. A repository-only user must not receive unrelated or ownerless diagnostic rows, source IDs, author text, counts or cursors merely because a requested repository is allowed. Preserve raw records on disk and for an explicitly privileged copy-diagnostic reader; do not invent ownership or weaken canonical interpretation.

Use a small explicit fixture authorization context outside the migrated DB. Do not build a new production permission schema, live admin UI, LDAP workflow or general security platform in this pass. Existing v12 responses/auth behavior stay unchanged. Candidate identity resolution must nevertheless fail closed for expired/revoked sessions and missing/disabled principals; no stale memory fallback may turn a failed named-user lookup into a privileged candidate actor.

## Two limited evidence refinements, not new runtime findings

**Response-driven pagination:** the current `page_signed` request returns `next_after_id=0`, but the labeled `page_continue` request uses a separately hard-coded `after_id=199`. The checks prove individual page shapes, not a consumer following that response cursor. Add a real consumer-built next request using the returned value, and verify no permitted IDs are skipped/repeated through completion. Keep safe refusal before using an unsupported i64 cursor. There is no independently reproduced pagination implementation bug here.

**Populated v12 compatibility:** the current comparison exercises the real existing history handler with an empty authentication DB and `{entries:[],total:0}`. Retain that control, but add a separate populated v12 fixture with complete old fields and authenticated/unauthenticated results. The candidate-absence request currently runs against a manually assembled history-router subset; keep source/default-feature checks separate from claims of executing the full daemon/installer router.

These refinements belong in the next authorization journey, not another review-only detour.

## Next completion bar

The next handoff should show user A reading repository A but not B, user B the converse, an explicitly privileged diagnostic operator, revoked/expired/disabled identity refusal, ownerless-data confidentiality, response-driven paging, and populated v12 compatibility. Authorized reads and denials must preserve candidate/source/config/ref/endpoint bytes; only the separate fixture auth/grant state may change. A denied request must not open the copy reader.

Retain all 85 identities, 26 literal DTOs, sequence/crash/storage checks and the three named comparisons. Use targeted iteration, then one final full qualification on the meaningful final head. Keep PR72 draft and stop. Deployed versions, arbitrary lineage/policy, production identities, writer fencing, installer/scheduler activation, general #64 recovery and live acceptance remain unqualified.
