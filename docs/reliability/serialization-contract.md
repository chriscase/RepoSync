# Copy-only nullable reader DTO — review 9

## Boundary and version

`reposync.copy_read.v1` is a versioned offline DTO over the existing `CopyReaders` authority algorithm. Its only producer entry points are `lookup_dto`, `list_dto`, `status_dto`, `last_emitted_dto` and the display-only `legacy_page_dto`. They reuse the corresponding sealed-copy reads; no SQL, migration, generation inference, endpoint commands, operational router or alternate authority algorithm is added. The module is compiled only with `reliability-fixture`.

Normal initialization, operational API responses, daemon, installer and scheduler remain at the reviewed v12 boundary. This is API-ready DTO qualification on disposable migrated copies, not activation or deployed-install qualification. The copied candidate v14 physical schema gains the M01 guard; superseded prototype shapes refuse at migration and reader admission rather than being silently repaired. Fresh fixture copies still originate from unchanged pinned original v12 imports.

Every response contains `schema`, the complete explicit `request`, and tagged `data`. Requests distinguish lookup/list/status/last-emitted/global-legacy-page. Scoped requests retain repository, optional requested generation, direction and source or signed pagination cursor as applicable. `null` generation is not inferred from a maximum. Status covers both directions; the global page grants no authority.

## JSON decision matrix

| Reader evidence | Tagged JSON contract | Consumer meaning |
| --- | --- | --- |
| Qualified generation and linked handled-chain mapping | `canonical.state=mapped`, typed target and authority | Recorded proved mapping under the named scope |
| Proved no-target | `canonical.state=no_target`, outcome and authority | Handled without a new emitted object; never inferred from NULL |
| Imported outgoing baseline | `handled_baseline_without_emitted_effect` | No invented SVN effect |
| Unlinked NULL, malformed or contradictory legacy row | `unresolved` plus reason; separate typed legacy values | Display evidence, not resolved authority |
| Ownerless or duplicate owned rows | `unresolved` | Ownership/ambiguity stays visible; every row remains representable |
| No proved handled result | `missing` plus separate `nonhandled` | **Not** proof that no pending/unknown record exists, nor permission to retry |
| Wrong/missing generation, disabled/not-qualified/missing repository | Explicit qualification state; `nonhandled.availability=unqualified_scope` | Empty nonhandled records are unavailable evidence, not a claim that none exist |
| Qualified scope | `nonhandled.availability=qualified_generation` and named records | Records outside the handled chain; their state grants no handled authority |
| Applied then no-target | `current_target:null`, separate `last_emitted.target` and authority | Latest handled source differs from last recorded applied target |

`last_emitted` preserves the existing reader contract, including the incoming proved import-baseline fallback and absence of an invented outgoing baseline target. Pending/effect-unknown records remain separate. A canonical `missing` result for a source with effect-unknown evidence still exposes that evidence in `nonhandled`; consumers must not infer retry safety from `missing`.

## Null, unusual values and consumer parsing

Targets use `{ "kind":"git", "value":"..." }` or `{ "kind":"svn", "value":3 }`; missing optional targets/authority and absent generation/cursor are explicit JSON null. Canonical no-target is a tagged state rather than a fabricated target or absent outcome.

Each legacy mapping column is separate and tagged: SQL NULL is `{ "type":"null" }`, integers/text are `integer`/`text` with a value, blobs are exact lowercase `blob_hex`, and REAL is `real_bits` with 16 lowercase hex IEEE-754 binary64 bits. This preserves infinity and unusual values without JSON's nonfinite-to-null coercion or pretending a noninteger SVN value is a valid revision. Empty text remains `{ "type":"text", "value":"" }`, distinct from NULL. The old schema's author defaults are NOT NULL empty text; the initial new fixture expectations were corrected to that unchanged schema fact, with exact equality assertions retained.

Pagination IDs are signed i64 JSON integers; `None` starts at the first retained ID including negative/zero, and an empty page returns `next_after_id:null`. Consumers must preserve i64 integers without floating-point rounding. Deterministic keyset ordering and the existing limit1..200 apply. Duplicate and ownerless rows are preserved, not collapsed or assigned invented ownership.

`Response::decode` rejects unsupported versions, mismatched request/data operations and malformed required target types. Consumer parsing is transport validation, not an external-evidence admission or authorization service. It never grants authority to untrusted incoming JSON. Production HTTP authorization/activation is outside this candidate contract.

Only mapping/read-model business fields are emitted. SQL schema dumps, raw outcome evidence_json, config, credentials, sealed file manifests, endpoint URLs and installation internals are not serialized by this adapter.

## Literal examples and exact cases

[26 complete literal payloads](../../crates/core/tests/fixtures/copy-reader-v1.json) specify success in both directions, no-target, unresolved NULL, ownerless/malformed evidence, missing-with-unknown, missing unrecorded, wrong/absent generation, disabled/missing/not-qualified repository, historical status/emitted targets, signed first page, duplicate pages, blob/REAL values and empty page. Expected fixtures are independently specified; they are not generated from the DTO or reader results. Actual responses must equal the literals, decode into the versioned consumer model, and reserialize to equal payloads.

| Required ID | Test and proof |
| --- | --- |
| M01_HISTORY | 224 SQL rejections: two recursive-trigger settings × four resolved kinds × current/historical victims × twelve replacement and two direct mutation attempts; full row/evidence/frontier/count and DB-byte equality, real reopened lookup/status/emitted, last SVN r3, forward/bookkeeping/rollback usability; superseded schema refusal |
| S54_JSON_LOOKUP | Literal mapping/no-target/unresolved/scope decisions; clean and needs-reconciliation phases each preserve all bytes/manifests |
| S54_JSON_HISTORY | Both directions after applied then no-target; pending/unknown separate; rejected M01 overwrite cannot change serialized historical mapping/status/last target |
| S54_JSON_PAGE | Signed/zero cursor, NULL/ownerless/duplicate rows, exact blobs and finite/infinite REAL bits; all retained IDs survive consumer pagination |
| S54_JSON_READONLY | Repeated full DTO family and consumer/error paths preserve full DB/version, source/config/refs and endpoints; bad version/operation/null mapped target refuse |

Tests use the existing pinned original-code installation and disposable migrated copies. Labeled later history uses structural synthetic hashes/targets: it proves SQL/reader/serialization behavior, not that new external effects or arbitrary Git ancestry were verified. Separate retained cases prove the actual old import lineage and preserve K02/K03, L01/L02 and all original78 identities/oracles.

## M01 enforcement and limits

Candidate-only `pair_outcomes` uses `WITHOUT ROWID`, so the two declared ID/scoped-source identities are its only conflict keys; attempts to address a hidden rowid refuse before mutation. This also closes the locally reproduced residual rowid overwrite on intermediate71c. Legacy commit_map IDs and sequence storage remain unchanged. A BEFORE UPDATE structural guard examines resolved victims of either prospective unique conflict key while they still exist. It prevents implicit replacement through an unresolved row regardless of recursive_triggers or the replacement's resulting state. Existing direct UPDATE/DELETE and BEFORE INSERT guards remain. All four resolved kinds, current/historical victims, INSERT OR REPLACE and UPDATE OR REPLACE, and both recursive settings are tested. Nonconflicting pending UPDATE OR REPLACE bookkeeping and ordinary public-writer forward transitions remain usable; no baseline effect is invented.

Actual local before-fix Rust evidence is [curated separately](review/review-9-reproduction.json): a pending source-key replacement committed r99, and real reopened lookup/last-emitted returned r99 instead of r3. This is not a container before-run or a reviewer Rust execution. The independent review's exact-schema/transcribed-query evidence remains separately archived.

Remaining gaps: deployed versions/installations, arbitrary historical topology/policy/ancestry, production remote identity, concurrent writer fencing/backup ownership, credential variants, operational activation, live acceptance, down migration and general #64 external-effect recovery. No completed recovery search was repeated. No acceptance criteria or issue closures change. Stop at independent review; proposed next slice is additional copy-only historical-topology DTO qualification, with operational activation requiring its own reviewed boundary.
