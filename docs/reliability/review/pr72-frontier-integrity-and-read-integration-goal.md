# RepoSync PR #72 — frontier integrity and end-to-end copy-read qualification

## Goal and fixed starting point

Continue `chriscase/RepoSync`, branch `feature/reposync-reliability`, draft PR #72, from reviewed head:

`5e10a89db30dc742b32e43b0e0274f22c651f028`

Read independent review **#5331642648**, `RepoSync-PR72-review-9.md`, and this brief. This is the current continuation. Original `docs/reliability/GOAL.md` and issue acceptance criteria remain authoritative; do not rewrite their bytes or prior briefs.

Original base: `87379741779a6259f7eeb52a68cc6f061174e5ef`.
Retained comparison anchor: `f74fce855a1f1d80dd631397f436ba33906272e6`.
Immediate comparison anchor for this pass: the reviewed head above.
Original GOAL SHA-256: `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`.
Dependency lock SHA-256: `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05`.

**M01 and the demonstrated offline serialization slice are accepted.** Keep the replacement guards, outcome WITHOUT ROWID storage, 26 literal payloads, and all 83 cases. Do not restart the accepted storage/admission/migration/reader work. Recovery search is complete for its accessible scope; do not repeat it without new evidence.

Complete two stages in one bounded run: (A) N01 frontier identity correction; (B) one real copy-only request/response/consumer path. No intervening planning-only review is necessary. The objective is a finite usable read-inspection path, not more disconnected DTO variants.

## Stage A — close N01 at the candidate schema boundary

`pair_frontiers` still has an implicit rowid. With foreign keys ON and recursive triggers OFF, a valid new baseline for B can use INSERT OR REPLACE with A's rowid and delete A's otherwise protected frontier. A valid forward UPDATE OR REPLACE of B can do the same. The new-row/transition guards validate B; the implicit conflicting victim deletion is the unprotected part. Exact-schema probe scripts accompany the review.

Reproduce before modifying the implementation through the existing candidate tests. Then eliminate or protect the implicit frontier identity. WITHOUT ROWID is a reasonable candidate-only option; a different solution must meet the same raw-SQL controls without relying on an unenforced connection pragma. Do not modify legacy commit_map storage or sequence behavior.

Tests must exercise both statements and rowid/oid/_rowid_ aliases, with two distinct repositories and same-repository separate generations as applicable, both directions, and recursive_triggers=0 and1. Each rejection must preserve complete table values, source/copy bytes when applicable, all victim and source frontier rows, existing outcomes, schema/version and subsequent connection usability. Reopen actual CopyReaders and DTOs: both pairs' lookup/status/last-emitted must match before the attempt. FK/integrity alone is insufficient.

Retain legitimate baseline creation and normal forward public-writer transitions. Do not invent an outgoing effect for the imported Git baseline. Unknown Git ancestry remains unknown; structural tests do not become external-effect proof.

Make a short, complete identity matrix for the new authority tables: declared primary/unique keys, implicit identities, supported INSERT/UPDATE/DELETE/REPLACE paths, and enforcement boundary. Focus on avoiding another instance of this same replacement class. Do not expand into adversarial PRAGMA writable_schema, malicious DDL, arbitrary database repair, or general #64 operations.

Fresh v12 copies must build the corrected candidate schema. Superseded prototype physical shapes must refuse rather than silently normalize; do not renumber or deploy a production migration in this slice.

## Stage B — one end-to-end copy-only inspection path

### Producer and fixture

Use the existing pinned-original importer/topology generator. Old writers exit; originals remain sealed; candidate migration runs only on separate disposable copies. One qualified pair, its independent neighbor and disabled/unqualified control are sufficient. Label any later structural history as synthetic, not newly proved remote history.

Use the project's actual web/request stack for an **in-process test-only or feature-gated read adapter** over the existing CopyReaders/DTOs. No public listening service is required. Cover explicit repository/generation/direction/source requests for lookup, list, status and last-emitted. Reuse the current authority algorithm and candidate migration implementation; no duplicate SQL truth source.

The current operational `/api/commit-map` and related v12 responses remain unchanged. Candidate routes must be absent in the default router/build. Avoid merely changing String to Option in the live API and calling it compatible. Document the candidate route names and response version separately; they are unactivated prototypes.

### Access and isolation

Reuse existing request/session validation where practical. Do not invent claims of existing repository permissions: inspect and document the actual model. Exercise unauthenticated/invalid-session refusal and authorized fixture reads. Put any auth/session writes in separate disposable fixture state; the migrated candidate database stays immutable. Any substituted middleware is explicitly unqualified for deployment, not a silently accepted security proof.

No request may select a filesystem path, migrate, repair, initialize, synchronize, cancel, delete or write data. Map admitted fixture IDs to sealed copies internally. Unqualified/wrong/missing generations stay explicit; no maximum-generation selection. An unscoped legacy diagnostic page, if exposed at all in this test adapter, must have deliberate privileged access; it is not a back door for repository-scoped data.

Use in-process transport inside the existing isolated runtime. No production tokens, host service, unrelated loopback access or remote API. Preserve original/candidate/config/refs/endpoint state across success and rejection. Check HTTP status/content type and payload, not just Rust method return values. Errors must not leak SQL, secret values, database paths or raw evidence documents.

### Actual consumer

Add one JavaScript/TypeScript consumer test using serialized response bytes from the real adapter. Prefer the existing frontend test infrastructure. Do not use only the same Rust DTO type as producer and consumer again.

The finite decision matrix must include:

- qualified mapping and proved no-target, both directions;
- applied then no-target, with current target absent and last actual applied target retained;
- missing handled result with effect-unknown/pending records still visible and no retry-safety inference;
- NULL versus empty text, ownerless/unlinked legacy display and duplicate ambiguity;
- missing/wrong generation and disabled/unqualified scope;
- signed/zero pagination IDs, empty final page and exact continuation ordering;
- i64 values at and beyond the browser safe-integer boundary.

The current v1 JSON contract requires consumers to preserve i64 exactly. Use a lossless consumer or explicitly refuse unsupported values before generating a rounded cursor. A test using only small integers is not full-range browser qualification. Do not silently revise v1 fixtures; any wire-format revision must be additive/versioned and explain old-consumer compatibility. Safe explicit refusal is acceptable for an unsupported range in this prototype if reported accurately.

Render/interpret raw legacy data as data, never HTML or canonical authority. Producer response version and operation mismatch tests remain. Do not claim the decoder validates untrusted external history merely because it decodes JSON.

### End-to-end completion bar

A single recorded journey must go from old-code fixture generation to copy migration to authenticated in-process request to emitted JSON to real consumer interpretation. Retain the broader 26-literal matrix rather than duplicating every old case through a new fixture. Add a small set of meaningful exact cases covering the new boundary. Freeze once those and existing controls pass; no speculative extra topology campaign.

## Default behavior and remaining release boundaries

Ordinary startup, `Database::initialize`, installer, daemon, scheduler, API routes and operational consumers remain at v12 and unactivated. Prove default candidate-route absence and retain the startup nonactivation tests. Supported v12 responses should have compatibility assertions at the actual handler boundary where feasible.

Do not merge, deploy, release, close issues, change acceptance criteria, access active synchronized repositories, reset checkpoints, reimport as recovery, or force-push. No production database or credential is needed. General #64 journaling/recovery, canonical sync activation, full writer fencing, arbitrary historical migration, and deployed-version/installer qualification remain later gates.

## Evidence and working discipline

Use separate ordinary commits for (1) schema correction/tests, (2) copy-only adapter and consumer, (3) finite qualification/docs. Preserve all 83 current required identities and correct assertions. Retain three locked comparisons: original base, retained f74 anchor, and immediate reviewed start. Keep formatting/integration failures and ignored controls visible; no unrelated broad formatting rewrite.

Run focused tests while iterating, then final exact suite, three comparisons, broader E2E and full evidence scan on the final meaningful code head. Do not repeat a full suite on an unchanged tree without a concrete flakiness investigation. Do not create self-referential SHA or documentation-only receipt loops. If an environment limitation blocks consumer/transport testing, report NOT RUN rather than replacing it with a mocked pass; still publish completed work and the exact remaining blocker.

Curate fail-before and pass-after evidence, preserving raw log hashes and clear runtime/fixture provenance. No claim that synthetic hashes prove new remote effects. Record functional/final/remote/test merge/tree identities and whether code trees match. Artifact byte hashes must be recomputed after download; compare exact test IDs and outcomes, not aggregate counts alone.

## Return handoff and stop

Return a compact delta report containing N01 actual reproduction/correction and identity matrix; exact new adapter and consumer paths; the recorded end-to-end journey; default route absence/v12 compatibility; read-only preservation; per-case results and retained counts; three comparison outcomes; known unrun/failed work; final SHA/tree/GOAL/brief/lock hashes; downloadable artifact ID/hash/expiry and key extracted hashes.

Use PASS / FAIL / PARTIAL / NOT RUN, separating local proofs from CI and deployment. Reference unchanged earlier evidence rather than repasting all manifests. Keep PR72 draft and stop for independent review. Do not proceed into activation on the strength of these tests alone.
