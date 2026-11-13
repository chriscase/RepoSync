# #56 — Repository path-policy editor (RS-14)

Parent: #61. Source-only candidate on base `1ddda4750c9c4ea55e471f794a89f3dfa43aa67a`
(tree `044886e72d7396a75f37622e98d6348b5f0a4ab0`). **No test, build, type check, lint,
browser or runtime step has been executed for this candidate.** Everything under
"Evidence" is authored regression source whose results are NOT RUN. This does not close
#56, qualify a release or change engine behaviour.

## Defect in the base

- `GET /api/repos/:id` never returned `allowed_paths` or `blocked_patterns`, but the editor
  initialised from them. Both fields therefore read as empty, and **every** settings save
  sent `null` for both, silently clearing the repository's path policy.
- A stored value that was not JSON would have thrown in the editor. The stored value was
  never validated on write, so a malformed list could be saved and then make the scheduler
  refuse to sync that repository.
- The "Git Hook" download link depended on the same never-returned fields and never appeared.

## Storage and enforcement (unchanged)

The columns stay nullable JSON text. `NULL` and `[]` both mean "no rule of this kind".
The scheduler parses each column as a JSON array of strings, and refuses to sync the
repository if that fails. `reposync_core::path_projection` is the only matching
implementation. There are no schema, engine, scheduler or `path_projection.rs` changes.

## API contract

### Read: `path_policy` on every repository detail response

Every repository detail response includes `path_policy`: `GET`, the `PUT` result and
receipt replays. The raw columns stay write-only and are not returned.

```json
{"version":1,
 "allowed_paths":{"state":"list","entries":["src/","config/"]},
 "blocked_patterns":{"state":"unset"},
 "enforced_direction":"git_to_svn","svn_to_git_filtering":"not_applied","path_rewriting":"none_read_only"}
```

| `state` | Stored value | Extra fields |
| --- | --- | --- |
| `unset` | `NULL` | none |
| `list` | JSON array of strings, including `[]` | `entries` |
| `malformed` | anything else (parsed exactly as the scheduler parses it) | `stored_bytes`, `reason`; no `entries`, and the raw text is not echoed |

### Write: `PUT /api/repos/:id`

The existing semantics are unchanged: an absent field keeps the stored value, `null`
clears it, and a string replaces it. Save receipts, `expected_updated_at` CAS and
same-request replay are all unchanged.

**New:** a field that is present as a string is validated after the existing durable
receipt recovery and before any CAS or write. An exact replay of an already-accepted
request (same actor, request ID and payload bytes) therefore still returns its original
receipt, even if a later rule would refuse its value. A reused request ID with different
content stays `409`. A novel invalid request answers `400` with
`{"error":"<field>[<index>] <code>: <message>; nothing was saved"}`; the index is omitted
for whole-list errors. The whole request is refused, including its unrelated fields.

| Code | Rule (checked in this order per entry) |
| --- | --- |
| `not_array` | The value must be a JSON array of strings; send `null` to clear |
| `too_many_entries` | At most 256 entries |
| `empty_entry` | Must not be empty |
| `entry_too_long` | At most 1024 UTF-8 bytes |
| `control_character` | No Unicode `Cc` characters (U+0000–U+001F, U+007F–U+009F) |
| `surrounding_space` | Must not start or end with U+0020 |
| `names_no_path` | After normalization (`\`→`/`, leading `/` removed, trailing `/` removed), the entry must not be empty |
| `empty_component` | No `//` inside the normalized entry |
| `dot_component` | No `.` or `..` components |
| `wildcard_in_allowed` | Allowed prefixes are literal, so `*` is refused there. Blocked patterns may contain `*` |

Stored lists are not revalidated on read. An unchanged legacy list is reported as a
`list` and keeps working exactly as before.

## UI contract (`RepoDetail` and `PathPolicyEditor56`)

- **Reading.** The policy is read only from `path_policy`. A missing object, a version
  other than 1, or an unrecognised field shape is **unavailable**: shown as such, with no
  controls, never previewed and never submitted. A read `null` in the legacy raw fields is
  ignored.
- **Field states.**
  - `keep` (the default): the field is omitted from the PUT.
  - `edit`: the entries replace the stored value. Removing every entry submits `null`,
    which the summary shows visibly as a clear. An edit that leaves the known list
    unchanged is still omitted.
  - `clear`: explicit `null`, with Undo.
  - A malformed field offers only "Replace with a new list" or "Clear". Until one is
    chosen it is omitted, so it is never cleared automatically.
- **Recovered drafts.** A recovered string restores as an explicit replacement. A
  recovered `null` is **not** restored as a clear, and a visible notice says so: drafts
  written before #56 recorded `null` because of the defect above. An unreadable recovered
  value raises the existing "recovery draft could not be read" error.
- **Inline validation.** The same codes and order as the server. Errors are bound to their
  labelled input through `aria-invalid` and `aria-describedby`, with a `role="alert"`
  message, and the list stays open until they are fixed. Only fields that would be
  submitted block Save. A blocked Save shows "Nothing was saved…" and sends no request.
  Duplicates, `*` alone, and a `*` in the middle of a pattern are non-blocking warnings.
- **Collapsed lists.** Each field is a labelled group with a summary. A disclosure button
  (`aria-expanded`/`aria-controls`) shows or hides the entries. Entries are individually
  labelled ("Allowed path prefix 3"). Add moves focus to the new entry; Remove moves focus
  to the previous entry, or to Add. Edit, Clear, Add and Undo carry screen-reader context
  in their names.
- **Unchanged.** Unrelated settings, credentials (never part of this body), receipts,
  journal recovery and `RepositoryEditorOwnership` are not changed. The shared save
  allowlist already carries both fields as `string | null`.
- **Hook link.** The "Git Hook" link appears only for a known, non-empty list.

## Preview semantics

The browser preview is a local port of `path_projection` and is never sent to the server.
It uses the lists as they will be after saving.

- **Normalization:** `\` becomes `/`, and leading `/` characters are removed.
- **Allowed prefix:** the trailing `/` is ignored. `team` matches `team` and `team/…`,
  not `team-other`. Matching is case-sensitive, and an empty prefix never matches.
- **Blocked pattern:**
  - A leading `*` is a suffix match (`*` alone matches everything).
  - Otherwise, a trailing `*` is a string-prefix match on the full path, so `secret-*`
    matches `secret-x/child`.
  - Otherwise the pattern is a component prefix.
- **Decision:** if both lists are empty, the path is included. Otherwise a path not under
  any allowed prefix is excluded first, then the first matching blocked pattern excludes
  it. This is the same order as `path_is_projected` and `exclusion_message`.
- **No preview:** a kept malformed field means the scheduler refuses to sync, so the
  preview shows that instead of results.
- **Sample paths are verbatim.** Each line of the sample textarea is one path exactly as
  typed; only empty lines are ignored. Leading and trailing whitespace is part of a
  filename (the engine never trims it), so ` src/a.rs` is not under `src` and
  `tool.exe ` does not match `*.exe`. Such paths are rendered with whitespace preserved and
  marked "Includes leading or trailing whitespace".

`PREVIEW_VECTORS` is kept identical in `crates/web/tests/repo_path_policy_56.rs` and
`web-ui/src/pathPolicy56.test.ts`. The Rust side evaluates the table with
`path_is_projected` and `exclusion_message`, and the UI side with the port. Equal tables
with both sides passing would be the parity evidence; neither side has been run.

## Disclosed limitations

1. **SVN→Git isolation.** This base does not contain qualified #53. The policy filters
   Git→SVN only; SVN→Git imports every path on the SVN branch. In that direction a
   non-empty policy only disables convergence and settling shortcuts. The UI states this
   in a note, in every preview summary and in the read DTO (`svn_to_git_filtering:
   "not_applied"`).
2. **Rewriting is read-only.** Included paths keep their branch-relative path. No rewrite
   mapping is offered.
3. **Other readers are unchanged.** `GET /hooks/pre-commit` and SVN-commit reconciliation
   still treat a malformed stored value as empty. They are outside this allocation, and
   new malformed values can no longer be written through the API.
4. **PR #216 risk.** Open PR #216 modifies `crates/core/src/path_projection.rs`. If it
   lands, re-verify `PREVIEW_VECTORS` and the port.
5. **Whitespace.** Only U+0020 counts as surrounding whitespace. Other Unicode spaces are
   accepted on both sides.
6. **Harness wiring.** The component test is `PathPolicyEditor56.test.ts` (no JSX), so
   the existing vitest `include` (`src/**/*.test.ts`) collects it without any
   `vite.config.ts` change. `tests/path-policy-56.e2e.mjs` is not wired into
   `package.json` scripts. The `contracts/README.md` index is not updated.

## Evidence (authored, NOT RUN)

| Source | What it would prove |
| --- | --- |
| `crates/web/src/api/repo_path_policy_56.rs` unit tests | Entry rule table (shared with the UI); absent/null/string forms; error text; unset vs `[]` vs malformed reads |
| `crates/web/tests/repo_path_policy_56.rs` (real router, SQLite file, loopback HTTP) | Read states with no raw columns. GET→edit→PUT→GET preserves, replaces one or both, and explicitly clears. A malformed value survives unrelated saves. All 13 invalid forms give 400 under a `BEFORE UPDATE` abort trigger, with no receipt and the unrelated name unapplied, plus a positive lookup control. Credentials, runtime progress and unrelated settings are preserved. Receipt replay is stable, a reused ID with a different payload gives 409, and a stale editor gives 409. Validation order: an exact replay of a durably accepted legacy request whose value is now invalid returns its original receipt after a later edit without re-applying it; the same ID with a different invalid payload gives 409; a novel invalid request gives 400 with no write or receipt. Preview vectors, including leading and trailing whitespace, run against the engine functions |
| `web-ui/src/pathPolicy56.test.ts` (vitest, node) | Read classification, including unavailable for absent or legacy `null`. PUT body only for explicit changes. Malformed fields kept until Replace or Clear. Shared allowlist passthrough. Recovered-draft rules. Server-parity entry rules and UTF-8 length. Submit-only blocking. Warnings. Preview vectors, verbatim sample lines and the whitespace-preserving textarea-to-preview pipeline, the after-save effective lists, and the hook-link rule |
| `web-ui/src/components/PathPolicyEditor56.test.ts` (static render, collected by the existing vitest include) | Collapsed disclosure wiring; read-only mode; error/label/describedby binding with a disabled hide button while invalid; explicit actions for malformed; no controls when unavailable; preview table and status; notices; whitespace-preserving preview cells |
| `web-ui/tests/path-policy-56.e2e.mjs` (Vite plus an existing admitted Chromium, loopback fixtures only) | Exact PUT bodies: unrelated save omits both fields; an inline invalid entry sends no PUT; replacement; explicit clear; malformed kept, then cleared; no `path_policy` means no controls and fields omitted. Keyboard Remove moves focus to the previous entry (or to Add for the last one), Undo restores keep, and Save sends only that field, with unrelated form fields unchanged and an actual API reread that preserves them. Keyboard disclosure and a preview that keeps typed whitespace. Axe (WCAG 2.x A/AA) on the policy section |

Commands a later admitted executor would run:

- `cargo test -p reposync-web repo_path_policy_56`
- `cargo test -p reposync-web --test repo_path_policy_56`
- `cd web-ui && npm run typecheck && npm run lint && npx vitest run src/pathPolicy56.test.ts src/components/PathPolicyEditor56.test.ts`
- `REPOSYNC_A11Y_BROWSER=<admitted chromium> node --test --test-concurrency=1 tests/path-policy-56.e2e.mjs`

## Scope

Existing files are changed only in these regions:

- `crates/web/src/api/repos.rs`: helper registration, the `path_policy` DTO field and its
  initializer, and present-field validation in `update_repo`.
- `web-ui/src/api.ts`: repository policy read types; the raw fields become optional and
  write-only.
- `web-ui/src/pages/RepoDetail.tsx`: policy parsing and serialization, the edit-error gate,
  and editor/preview rendering.

New files: the helper module, the HTTP/SQLite regression, the logic module and its test,
the component and its test, the browser regression, and this contract.

Not changed: the schema, engine, `path_projection.rs`, Personal mode, #69, PR195, #73,
shared save/auth machinery (`repositorySave.ts`, `useRepositorySave.ts`), dependencies,
workflows and `vite.config.ts`.
