# #53 SVN→Git path filtering (forward, incremental)

Status: implemented on `claude/rs53-svn-to-git-filtering` from partial52 `dc03b8a`.
Native cases are authored in `crates/core/tests/partial_sync_53.rs`; they need
real `svn`, `svnadmin` and `git` and were **not run** on the authoring host (no
Subversion installed). Qualification of those cases is the exact-head CI run.

## What changes

With non-empty `allowed_paths` or `blocked_patterns`, an SVN revision reaches
Git only through its **selected** file content. Before this change the whole
revision patch was applied and only log metadata was filtered.

Forward and incremental only. There is no retroactive history cleanup, no
reimport, no default-policy change, no #57 path rewriting, no Personal-scope
(#55/#59) behavior, no UI/API/schema/version change and no dependency change.

## Namespace

Policy matching uses the existing canonical branch/Git-root-relative namespace
and the existing component-aware `path_is_projected`. Changed-path log entries
are mapped once by `checked_svn_path_identity` / `svn_log_path_to_branch_relative`
(unchanged `fetch_svn_changes`). `svn diff` section paths are already relative
to the client target URL and are used literally. There is no second trunk strip,
no URL decoding, no substring rewrite and no prefix flattening.

## Preparation (before Git fetch/reset and conflict detection)

`prepare_svn_projection_53` keeps the actual fetch. When rules are active and a
managed repository scope exists, it then:

1. Observes the live SVN identity (`observe_live_svn_identity`). It calls the
   real `OrdinaryWriterWindow::require_engine_scope` before and after one
   read-only snapshot of scope, generation, writer epoch and pinned canonical
   lineage, outside any write reservation. A live/pinned lineage mismatch
   refuses.
2. Plans every fetched revision in order from its actual `svn diff`:
   * The diff is split into node sections. Any text outside a section, or a
     `---`/`+++` header naming another path, holds the revision.
   * Every section must be covered by a changed-path log entry.
   * A log entry that can reach the selection is validated against diff
     evidence:
     - `M` needs a section.
     - `A`/`R`/`D` without an exact content section (a directory, or a file
       missing its evidence) is expanded with a pinned `svn export` into an
       owned temporary directory: `A` and copies at r, `D` at r-1, a plain
       replace at both. Every selected file below it must have a content
       section, or the revision is held.
   * An SVN copy never deletes its source; only an actual delete in the
     revision does. Each endpoint is classified on its own path.
3. Builds the projected view. Every revision is kept with its metadata and
   order. A ready revision's `changed_files` are its selected file endpoints;
   a held or pending revision keeps its complete log view, so the unchanged
   conflict detector stays conservative.

Selected effect:

| Selected evidence | Result |
|---|---|
| A selected content section with a hunk, a binary marker, or a file add/delete | **Content**: only the selected sections feed the unchanged apply/LFS/commit/push/observe/finalizer path |
| No selected content; only plain property, empty-directory or root metadata changes (SVN's header-only `Index:` section for a property change carries no content) | **Zero**: filtered finalization |
| No selected content, but a selected `svn:executable` / `svn:special` change | **Held**: Git meaning without a Git conversion |
| Unparseable diff, uncovered section, missing evidence, read/export failure | **Held** |

A held revision stops the cycle at that revision with `HistoryBlocked`
(`svn_projection_held`) and an audit row. Later revisions are not prepared and
stay pending. A failed selected patch uses the existing stop at the durable
frontier. Neither ever counts as filtering.

Empty allowed **and** blocked rules, an empty fetch, or a call without a
managed repository scope return no batch: the old code path runs unchanged.

## Filtered finalization (atomic)

A zero-effect plan mints a crate-private `VerifiedSvnProjection`. It:

* has private fields;
* has no `Serialize`/`Deserialize`;
* has no Boolean, scalar or JSON constructor;
* borrows the actual window and database.

`Database::record_svn_filtered_mapping` checks the owner outside SQLite. Then,
in ONE journal transaction, it:

1. revalidates epoch, scope, generation, pinned lineage and policy;
2. runs the shared begin core (`SvnFiltered`);
3. runs the shared confirm core, whose filtered tail writes:
   * the scoped v1 `no_git_content` SVN receipt and SVN watermark (the
     projection proved there is no Git content under the active policy);
   * the canonical `intentionally_filtered_no_target` outcome and checkpoint,
     reason `svn_filtered_no_git_content`, policy snapshot, and no Git or
     SVN target;
   * the proven NULL `commit_map` row, through the single transaction-internal
     `insert_proven_no_target_row_tx`;
4. completes the operation and clears it as active.

It never moves the inbound Git cursor, never records a Git SHA, never reports a
remote effect and never increments the applied count. A write failure rolls
back the operation, receipt, watermark, outcome and row together. Recovery is
the next ordinary cycle's fresh observation; there is no blind replay and no
cleared hold.

The generic `begin_team_cycle_mapping` refuses `SvnFiltered`. The generic
`confirm_team_cycle_mapping` (and `finalize_tx`) refuses it without the
verified projection, including for a forged journal document. Legacy outcomes,
their receipts and their finalization are unchanged.

## Diagnostics

`svn_to_git_filtered` audit rows and structured logs carry the revision, the
repository scope, the source/included/excluded file-endpoint counts and a
16-hex policy identity (a SHA-256 prefix of the policy snapshot). They carry
no URL and no credential, and report no applied commit count.

## Test map (`crates/core/tests/partial_sync_53.rs`)

| Case | Covers |
|---|---|
| `sc01_…` | SC01: admitted paths/bytes only; excluded revision accounted in the scoped frontier |
| `sc02_…` | SC02: no Git commit/ref/tree effect; typed no-target, exact NULL row, receipt, cursor; reopen + repeat with no extra rows |
| `sc03_…` | SC03: straddling revision, selected bytes and deletes only, provenance, unrelated trees kept |
| `sc04_…` | SC04: Git mixed commit to SVN, outside paths untouched (existing outbound projection) |
| `sc05_…` | SC05: Git excluded-only commit, no SVN revision, existing receipt, quiet repeat |
| `sc06_…` | SC06: SVN→Git→Git edit→SVN round trip, exact bytes, reopen, quiet repeat, no duplication |
| `empty_policy_…` | Empty-policy legacy behavior |
| `component_matching_…`, `blocked_rules_alone_…` | team vs team-other, spaces/`@`/dash/Unicode/`%41` literal names, blocked-only rules |
| `ancestor_directory_delete_…` | Ancestor directory delete, admitted descendants only, no unrelated loss |
| `copies_never_delete_sources_…` | Copy without delete, moves across the boundary, blocked endpoints |
| `empty_directory_and_plain_property_…` | Empty directory and property evidence, Git-meaning property hold, later revisions pending |
| `failed_selected_patch_…` | Selected patch failure is never filtering; later revision pending; recovery by an ordinary cycle |
| `filtered_finalizer_write_fault_…` | Real file-backed SQLite write fault in the filtered finalizer, drop and reopen, atomic rollback, fresh-cycle finalization once |
| `in_scope_conflict_…` | Pending Git plus in-scope conflict stays held (#73 handling unchanged) |
| `replaced_svn_repository_identity_…` | Changed SVN UUID cannot mint filtered completion |

Unit tests cover the diff splitter, header/property/binary facts, selection
reach and pinned-export walking (`sync_engine::svn_projection_53::tests`); the
generic refusals and forged journal JSON (`team_cycle_mapping_operations::svn_filtered_tests`);
and the NULL-row helper (`nullable_commit_map::insert_proven_no_target_row_tests`).

## Not established here

* Whole #60: Docker Compose, rewriting and Personal scenarios remain open.
* Installed-version compatibility: the company installation's binary/build
  digest, OS/arch, schema/mode and sanitized config/policy/journal shape are
  not established. Historic 873/54 fixtures do not substitute.
* `svn:executable` / `svn:special` changes alongside selected content follow
  the unchanged apply path, which does not convert SVN properties.
