# Copy-only read transport and consumer (review 10)

## Exact boundary

`crates/web/src/api/copy_inspection.rs` is compiled only with the web crate's
`reliability-fixture` feature. Tests construct its Axum `Router` directly and
make in-process requests; `WebServer::start` does not merge it. Normal startup,
operational `/api/commit-map` and `/api/sync-records`, daemon, installer and
scheduler remain at v12. This adapter neither migrates a request-selected
database nor contains another authority algorithm: every successful response
comes from `CopySession::readers()` and the existing v1 DTO methods.

| Fixture-only GET route | Required input | Existing producer |
| --- | --- | --- |
| `/__reliability/copy-read/lookup` | `repository`, optional `generation`, `direction`, exactly one matching `source_svn_rev` or `source_git_sha` | `lookup_dto` |
| `/__reliability/copy-read/list` | `repository`, optional `generation`, `direction`, optional signed `after_id`, `limit` 1–200 | `list_dto` |
| `/__reliability/copy-read/status` | `repository`, optional `generation` | `status_dto` |
| `/__reliability/copy-read/last-emitted` | `repository`, optional `generation`, `direction` | `last_emitted_dto` |

An omitted generation returns the DTO's explicit `missing_generation`; it is
never inferred. No global legacy page is routed. Unknown parameters, including
`path`, are refused; no request can name a file, initiate a write, migrate,
repair, synchronize or reset. Successful bodies use
`reposync.copy_read.v1` with JSON content type. Copy-open/read failures have
fixed error codes, without SQL, path, config, secret or raw outcome evidence.

The adapter calls the existing `validate_session`. The test gives it a
separate disposable **in-memory** v12 authentication database and session map;
neither is the migrated copy. Missing and invalid sessions refuse. RepoSync's
current authentication model does not establish per-repository read grants;
this prototype is **not deployment authorization proof**. Repository-scoped
legacy display can include ownerless evidence and is still only display data.
Raw legacy text is never rendered as HTML by the test consumer.

## One complete fixture journey

The pinned original `8737974` importer/completion generator produces one old
installation with two independently owned repository lineages and a disabled
repository in a disposable root. It exits before the copies are sealed. The test copies
the old installation, qualifies `pair` and `pair_two` against their disposable
SVN/Git endpoints, and runs the existing copy-only 12→13→14 migration. A
second independent copy remains `needs_reconciliation` for the unqualified
scope response. The old source, config, refs and endpoints remain sealed.

Only after migration, the fixture adds **labeled structural** applied,
no-target, pending and effect-unknown outcomes and display-only legacy rows.
Those synthetic SHA/target values demonstrate readers and transport; they do
not prove new remote effects or arbitrary Git ancestry. An authenticated
in-process Axum request emits actual serialized bytes. The Node consumer at
`scripts/reliability-copy-consumer.mjs` reads those bytes, checks HTTP status
and content type, version and operation, and interprets 30 response decisions.
The required case stores the raw response collection as
`S54_HTTP_COPY-wire.json` in the isolated evidence artifact. Full source and
both candidate-copy byte/mode manifests are equal before and after success
and rejection requests.

| Decision | Consumer check |
| --- | --- |
| Qualified mapping and no-target, both directions | Tagged canonical state and exact emitted target/authority |
| Applied then no-target | Current target `null`; separate last actual applied target retained |
| Missing with pending/effect-unknown | Separate nonhandled records remain; missing does **not** mean retry-safe |
| NULL/empty, ownerless, malformed, duplicate | Raw tagged data preserved; ambiguity stays unresolved; text is data |
| Missing/wrong generation, disabled, unqualified, missing repository | Distinct scope states; no borrowed generation |
| Signed/zero continuation and empty page | Exact ordered IDs and `next_after_id` |
| Beyond browser-safe i64, both signs | Explicit rejection before `JSON.parse` can round a cursor |
| Unauthorized, invalid input, path parameter and POST | Refused without candidate data disclosure |
| Operational history handler | Unchanged v12 shape/status/session behavior; candidate path absent |

The v1 wire format still contains JSON i64 numbers and its 26 literal
payloads remain unchanged. The Node consumer scans number tokens outside
strings with `BigInt` **before** `JSON.parse` and refuses integers outside
`Number.MAX_SAFE_INTEGER`. The adapter can emit such a value; this prototype
does not claim browser support for the entire i64 range. A future lossless
wire format needs an explicit additive version and old-consumer compatibility
review. The parser checks transport shape, not authenticity of remote history.

Deployment, per-repository authorization, arbitrary old topology/policy,
concurrent writer fencing, general #64 recovery, live acceptance and normal
route activation remain separate review gates.
