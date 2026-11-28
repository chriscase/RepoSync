# Filtered conflict and audit pages (#119)

Repository, conflict status, audit success and exact audit action predicates run in SQLite before `LIMIT`/`OFFSET`. Each reader computes its matching total and rows inside one read transaction, using the same predicates and bound values. It does not change records, acknowledge errors, migrate a schema or bypass conflict write guards. Row decoding failures fail the request; rows are not silently discarded.

## API contract

- `GET /api/conflicts?page=1&per_page=20&repo_id=<id>&status=detected` returns `{items,total,page,per_page,total_pages}`. Defaults are page 1 and 20 rows; the maximum page size is 100. Ordering is `created_at DESC, id DESC`, including a deterministic tie-breaker. Without `page`, the existing array response remains compatible, but now applies repository/status filtering before its limit. The existing web-layer paginated database reader delegates to the shared raw reader, preserving its public types; list timestamps are not fabricated by the API.
- `GET /api/audit?page=1&limit=50&repo_id=<id>&success=false&action=sync_error` retains `entries` and `total` and adds `page`, `per_page`, `total_pages` and `offset`. The default limit is 50 and maximum is 500. Ordering is `id DESC`. Repository, success and exact action are conjunctive; absence means no predicate. An explicit `page` wins over legacy `offset`. Unaligned legacy offsets retain their exact offset and report `page:null`, instead of claiming an aligned page. Canonical page requests always return their page number.
- Pages/page sizes must be positive. An explicitly empty repository scope is a bad request, rather than an accidental all-repository read. Arithmetic uses checked/widened offsets. Unknown repository/status/action predicates can return a confirmed zero matching count; historic repository IDs remain usable even if not currently registered. Authentication is unchanged.
- Zero total is confirmed empty only after a successful, valid response. An out-of-range page with a positive total means matching records exist elsewhere; it does not mean there are no matching records. Missing tables or unreadable matching rows return an error, with original evidence preserved.

Each page is self-consistent. Offset pagination is not a multi-request snapshot lease: new records or resolutions between requests may shift later pages. The reachability regressions use a fixed dataset. This contract introduces no cursor admission, engine ownership or remote freshness authority.

## UI journey

Conflict and audit list URLs retain repository, status/outcome/action and page. Changing a filter updates that filter and page 1 atomically, so a new filter never requests an old late page first. Next/Previous preserve the filters; an out-of-range page offers return to the first page. Matching totals come from the server, not a filtered page or a global counter. Audit success/failure summaries explicitly describe the current page.

The checked client validates page metadata and the requested row scope/predicates before caching success. A failed or malformed read shows unknown results and a Retry control, not a successful empty view. Conflicts no longer claim that an empty list proves everything synchronized. Existing #114 acknowledgement, #115 save behavior and #116 status/Skip guards remain unchanged.

Dashboard selection is retained in its repository URL context. Audit, conflict and error-summary links carry that selected repository; error links also preserve the outcome filter. Link navigation is separate from the acknowledgement action, with no nested action inside a navigation link. Failed recent-activity reads are explicit errors rather than “no activity”.

## Verification and limits

Synthetic real-router/SQLite fixtures contain 112 mixed A/B/unscoped conflicts and 183 mixed-repository/action/outcome audit events. They traverse the complete matching pages, including older A rows beyond the old global limits, check stable ties and totals against independently seeded IDs, exercise legacy/large/out-of-range requests and verify that reads preserve the records. Malformed rows remain errors. Core regressions cover the shared reader, numeric overflow refusal and existing nonzero-success interpretation.

Frontend tests use the actual pages with QueryClient/router providers, the actual checked API client and shared URL/cache helpers. They check scoped late pages, atomic filter resets, dashboard links, loading/error/empty distinctions, malformed responses and retained-cache failure. They are static page-composition/client/cache tests; no browser event or remote-service execution is claimed. Exact local results and the independent-review/hosted-CI/epic-integration outcome belong to the frozen source receipt. No production deployment or credential/access setting change is part of this story.
