# RepoSync PR #72 — candidate-only scoped read authorization

## Starting point and approval

Continue `chriscase/RepoSync`, `feature/reposync-reliability`, draft PR #72, from independently reviewed:

`ed2f1a4d73b63716c71b360115de321d7fa7f16c`

Read posted PR review **#5334892010** (Review 10) and accompanying `RepoSync-PR72-review-10.md`. **N01 and the requested in-process HTTP/Node milestone are accepted for their tested bounds.** Retain them. Recovery search is finished for its accessible scope; do not repeat it. No additional proposal-only review is needed before the bounded implementation below.

Fixed references:
- Original base: `87379741779a6259f7eeb52a68cc6f061174e5ef`.
- Retained comparison anchor: `f74fce855a1f1d80dd631397f436ba33906272e6`.
- Immediate comparison anchor: `ed2f1a4d73b63716c71b360115de321d7fa7f16c`.
- Original GOAL SHA-256: `16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8`.
- Locked graph SHA-256: `9feb01d8964bc93d67ff94014fcd1c7ff0f86111dc5e0efceaf288ec12cc2d05`.

Preserve original GOAL/prior briefs and issue acceptance criteria. The current 85 required case identities, 26 literal DTO fixtures and earlier correction oracles remain. The existing privileged fixture actor may receive explicit grants so its intended allowed requests continue to work; do not retain implicit all-repository access merely to keep a test green.

## Goal

Qualify a **candidate-only per-repository read authorization boundary** over the existing copied-installation inspection path. A valid session must no longer imply permission to inspect every repository or ownerless record in the copy.

Deliver the policy, adapter enforcement, and in-process request → emitted bytes → actual Node consumer tests in one pass. Add response-driven pagination and populated v12 compatibility controls to this same journey. Stop once this finite matrix and retained cases pass; do not start an arbitrary-topology campaign or production activation.

## A. Minimal explicit authorization model

Use server-supplied trusted candidate context, outside the immutable migrated DB, to identify:

- the enrolled copy/inspection context;
- an authenticated active principal;
- exact repository-read grants for that principal in that context;
- an explicit privileged diagnostic capability for ownerless/global legacy visibility, if supported.

An injected fixture policy/grant store is sufficient. No production permissions migration, administration UI, LDAP integration or new general security framework is required. The copy's business DB must not become the mutable auth/session/grant store.

Inspect existing auth functions instead of assuming session validity returns identity or permission. Reuse current DB/session primitives where they fit. Keep the operational helpers' existing behavior unchanged unless a separately agreed compatibility-preserving refactor is essential. Candidate resolution must verify the named user/session and current enabled state. Fail closed on identity/storage errors. An expired/deleted named session or missing/disabled user cannot be revived as a privileged legacy actor through an unrelated in-memory fallback.

Legacy single-password sessions need an explicit documented candidate policy. Refusing them by default is acceptable, or support a server-enrolled fixture operator with deliberate grants. Do not silently treat any valid legacy token as all-repository access. Never accept a user ID, role, grant list, copy path or permissions assertion from query/header content as authority.

Grants bind exact IDs, not string prefixes, display names, `created_by`, secret ownership, enabled flags or branch/parent relationships. No implicit maximum-generation selection. A grant permits inspection; it does not qualify the requested generation or make unknown history verified. Disabled repository state and disabled user state are different concepts; specify their read behavior independently.

Check authentication and the requested repository permission **before calling CopySession::readers or performing candidate reads for that request**. A request's generation, direction, source, limit or cursor cannot expand its repository grant. Define a stable denial policy that does not disclose unauthorized repository existence or metadata. Do not expose internal SQL/paths/secrets in refusal bodies.

Check authorization on every request. Grant withdrawal, session logout/expiration, account disablement and role/capability removal between requests must take effect. This is not a claim of distributed revocation/fencing against concurrent writers; state the request-time policy boundary clearly.

## B. Raw evidence confidentiality

Current offline diagnostic readers intentionally include ownerless rows through `repo_id IS NULL`. Their unresolved label is about synchronization authority, not permission to disclose the data.

For an ordinary repository-scoped principal:

- Return only data proven within that principal's permitted repository/copy scope.
- Do not reveal another repository's data or ownerless rows through lookup/list/status/last-emitted, errors, counts, pagination, canonical diagnostics or nonhandled records.
- Never assign ownerless records to the requested repository merely to serve them.
- Preserve the original rows on disk. A separate explicitly privileged diagnostic actor may still inspect raw evidence under the existing diagnostic contract.

Use a small explicit visibility selection/profile over the existing readers; do not create an alternate lineage/frontier truth algorithm. Apply visibility correctly before page selection and continuation decisions, not a post-hoc filter that leaks hidden row IDs or falsely ends a permitted listing. Do not turn permission-based omission into evidence that a source was handled, absent, or safe to replay.

Keep the existing offline v1 diagnostic DTO and literal fixtures intact. If the scoped transport needs visibility metadata or a distinct response profile/version, document it explicitly and test compatibility; do not silently reinterpret a legacy diagnostic response as a complete scope-filtered report. All permitted business data remains plain data, never interpreted HTML.

No global legacy route needs to be added. If deliberately adding one, it must require an explicit diagnostic capability and copy enrollment; normal repository access is insufficient.

## C. Finite in-process access matrix

Reuse the pinned old importer, sealed original, two independent repository copies/lineages and disabled/unqualified control. Use real local users/sessions in a **separate disposable authentication store** where feasible, not just unconditional fake middleware. Candidate grant changes are confined to that fixture store/context. No real credentials or network authentication.

Cover, at minimum:

| Actor/state | Required behavior |
| --- | --- |
| Active user A with grant A only | A's permitted lookup/list/status/last-emitted pass; B and ungranted contexts refuse before candidate-reader open |
| Active user B with grant B only | Converse of A; overlapping revisions and similar ID prefixes never combine ownership |
| Active user with no grants | Valid authentication does not reveal candidate data |
| Explicit diagnostic operator | Intended broad fixture reads, including separately permitted ownerless diagnostics, succeed |
| Missing/invalid/expired/revoked session | Refuse without candidate reads/data |
| Missing/disabled principal or policy lookup failure | Refuse without privilege fallback |
| Grant or diagnostic capability revoked between requests | A previously successful request now refuses or receives only its remaining permitted scope |
| Granted repository, wrong/missing generation | Permission is still checked; the qualified reader's explicit missing/unqualified state remains truthful |
| Disabled or unqualified repository | Follow documented inspection permission independently of sync eligibility; no reactivation |
| Modified query/path/ID/generation/direction/cursor | Cannot bypass grants; no filesystem or mutating operation selection |

Instrument the candidate reader boundary so denial proves no request-specific reader open/read, not merely unchanged bytes. An auth-store read or cleanup is separate and should be labeled; do not claim the auth store is immutable if session maintenance writes it.

Use distinctive synthetic canaries for A, B and ownerless rows. Prove they do not appear in unauthorized payloads, metadata, errors or cursors. Verify canonical scope, last-emitted and pending/unknown fields stay correct for allowed requests. Test all four route operations; no live listener is necessary.

## D. Consumer-driven pagination and compatibility refinements

The reviewed fixture labels one page `page_signed` returning cursor 0, then issues the labeled continuation with hard-coded 199. Retain those individual-page assertions, but add a genuine loop using each **returned** next cursor to build the next in-process request. Prefer the Node consumer building the continuation query from emitted response bytes, with the Rust harness dispatching it. Compare the final permitted-ID sequence to an independent expected selection. Include negative/zero IDs, relevant intermediate IDs, empty final page and no skipped/repeated permitted data.

Preserve explicit safe refusal of unsupported i64 values before an inaccurate cursor can be used. Full-range browser transport is still outside scope; do not silently change v1 integer encoding.

The current operational compatibility test covers an empty real `/api/commit-map` response and unauthorized refusal. Add a separate **populated v12** auth/history fixture proving unchanged complete fields, non-NULL SHA, ordering/filter behavior and authenticated/unauthenticated responses at the actual existing handler. Do not alter that route to accept v14 or serialize a new contract.

Keep candidate routes absent from default startup/build. A manually reconstructed router subset is not a full daemon-startup test: identify source/default-feature checks and any actual shared router-construction checks separately. No production listener or installer execution is required in this pass.

## E. Preservation and nonactivation

Every allowed and denied read keeps the migrated DB bytes/version, original DB/config/keys/refs/workdirs and fixture endpoints unchanged. Reuse the existing integrity, storage and seal controls. Auth users/sessions/grants live separately and may change only as explicitly tested.

Normal startup/Database::initialize, operational APIs, daemon, installer and scheduler stay at v12. The candidate registry remains an explicit fixture-only migration; no default active generation, canonical sync writer, or automatic database upgrade is enabled. No new DDL for this authorization slice is needed.

Do not merge, release, deploy, change acceptance criteria, close issues, force-push, access production, inspect a real user's secrets, reset checkpoints or reimport as recovery. No general #64 service or writer-fencing implementation. Do not restart accepted SQL/DTO work or the completed Grok recovery search.

## Evidence and finish line

Separate ordinary commits for candidate identity/grants/enforcement, confidentiality and transport/consumer tests, and concise documentation. Add a small set of stable required case IDs covering the new boundary; keep the 85 existing IDs and their strict assertions. Preserve visible formatting/integration failures and ignored conflict case. Do not weaken them or add unrelated formatting churn.

Iterate with focused tests. Then perform final required suite, original/f74/immediate locked comparisons, broader E2E and full evidence scan at the final meaningful head. Retain raw emitted bytes for an independent Node replay and sanitized per-actor decisions, denial read-access counters, row/copy preservation and populated-v12 proof. No recursive receipt-only commits or repeated unchanged full-suite loops.

Return a compact handoff with:

- exact start/functional/final/remote/test-merge/tree and GOAL/brief/lock hashes;
- principal/grant/diagnostic model and legacy-session policy;
- actor × route matrix and revocation/refusal results;
- ownerless/neighbor canary nondisclosure and denied-reader-open proof;
- response-driven pagination and populated-v12 compatibility;
- retained/new cases, three comparisons, scanners, known failures/unrun work;
- artifact ID, downloaded hash/expiry, emitted-wire/consumer identities;
- remaining deployment, topology, writer-fencing and #64 limitations.

Stop for independent review with PR72 still draft. Passing this goal freezes the scoped inspection milestone; it does not automatically authorize another fixture expansion or operational activation. The original coworker-requested cancellation, lifecycle, late-pairing, snapshot and refresh work remains separately tracked.
