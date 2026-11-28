# Scoped error acknowledgement (#114)

The Dashboard and Repository Detail “Errors (24h)” cards offer **Acknowledge** to administrators. Select the intended repository first. Choosing **All Repositories** and confirming the global prompt explicitly acknowledges across repositories, including legacy unscoped failures. Confirmation names the scope. Repository Detail reports a loading or unavailable status explicitly and disables acknowledgement when its status observation is unavailable, including after a failed refetch with cached data. Successful acknowledgement invalidates the real status queries, including aggregate status. The request is pending until the server responds; failures remain visible with feedback and may be retried.

Acknowledgement means “seen”, not “fixed”. It does not retry synchronization, change a checkpoint, clear a recovery hold, reset cumulative repository statistics, or change the original outcome. Audit entries retain their ID, original timestamp, operation details, author, direction, hashes and success/failure flag. A separate successful `errors_acknowledged` event records the authenticated actor, selected scope, finite observation and number acknowledged. Existing ordinary audit retention remains unchanged; acknowledgement itself performs no deletion. There is no purge permission or purge route in this API, including for administrators.

## API

Read `GET /api/status?repo_id=<id>` for one repository or `GET /api/status` for aggregate status. `total_errors` is the count of **unacknowledged** failures in the rolling 24-hour window; `last_error_at` is the most recent such active failure. `error_observation` contains a decimal-string `through_id` and RFC3339 `observed_at`. The two are read with the count in one SQLite read transaction. IDs remain strings on the wire to preserve full SQLite integer precision in JavaScript.

Send `POST /api/status/acknowledge-errors` with the observation from that displayed status:

```json
{
  "scope": {"kind": "repository", "repo_id": "selected-repository-id"},
  "observation": {"through_id": "123", "observed_at": "2026-11-03T10:00:00Z"}
}
```

Global scope is exactly `{"kind":"all"}`. Missing/unknown scope, extra fields (including `purge`), query scope, malformed/noncanonical/overflowing IDs, future timestamps and unknown repositories are refused. The legacy `reset-errors` POST route accepts only this same explicit body; old empty/query-only requests fail closed. It does not retain deletion behavior.

Success is `{"ok":true,"acknowledged":N}`. An IMMEDIATE transaction appends a versioned `reposync.error-ack.v1` audit event only if it dismisses at least one still-active failure. A retry with the identical observation returns zero for previously acknowledged rows and adds no duplicate receipt. Rows beyond the observed ID/time boundary remain active, even if they arrive during the acknowledgement or retry. Old observations cannot dismiss newly arriving failures. The operation never rewrites failed rows. Invalid or unknown acknowledgement event metadata does not suppress errors.

Unauthenticated, invalid, expired or disabled-user sessions receive **401**. A valid non-administrator receives **403**. Administrator status is rechecked against the current user record, rather than trusted from browser state. Authenticated legacy password-mode administrators are supported. Database failures return **500** with a generic client message, are logged server-side, and roll back the receipt; they do not masquerade as zero errors or successful acknowledgement. A valid retry is safe after a storage failure.

## History and compatibility

The CLI's existing `count_errors()` reports the same unacknowledged rolling count. The sync engine's `last_error_at()` and repository historical timestamp queries continue to read original failed audit rows, including acknowledged failures. Audit listing and failure filters continue to return those rows unchanged. `repositories.total_errors` remains a cumulative diagnostic statistic.

No schema migration, `user_version` update, configuration change, authentication-helper rewrite or execution authority is introduced. The separately reserved #69 schema and initialization seams are unchanged.

## Qualification

The story includes database regressions for two-repository isolation, global scope, byte-for-byte failure preservation, unchanged cumulative counters and historical timestamps, rolling-window date handling, malformed metadata, safe retry/new arrivals, boundary validation and atomic receipt failure. Real HTTP regressions exercise current admin/viewer/expired/disabled sessions, explicit scopes and old-route refusal, current-role revocation, and storage-error feedback. Frontend tests use the same query/view/action abstraction as Repository Detail, with a real TanStack QueryClient/Observer, to cover HTTP/transport failures, stale cached data and selected-status refetch showing a surviving new error. They also exercise exact scoped bodies and integer precision, explicit global confirmation, 401/403 distinction and error propagation. Frontend typecheck, lint and build are additional gates; default Rust CI alone does not run frontend tests.
