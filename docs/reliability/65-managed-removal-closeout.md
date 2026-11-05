# Whole #65 lifecycle verification

This change addresses all seven original criteria and four RS-16 requirements in
[#65](https://github.com/chriscase/RepoSync/issues/65). It builds on the existing
managed-removal, restore, dependency, error, session and status implementation;
it does not replace those completed features. The operational schema remains 13.
The protocol and recovery limits are specified in
[the removal contract](contracts/65-managed-remove.md).

## Acceptance and evidence map

| Requirement | Implemented behavior | Direct verification |
| --- | --- | --- |
| O1: truthful lifecycle and UI/API outcomes | Disable, registration removal, remote outcomes and owned cleanup remain distinct. Failed or unknown observations cannot mean healthy, absent or complete. | The 16 retained `candidate_r65_` HTTP tests; four new `rs65_` HTTP tests; reviewed-stage UI vectors and browser confirmation checks. |
| O2: durable ownership, waits, repeat and restart | Native guarded claim disables before effects. Each side records intent before I/O and a sealed, observed result before cleanup. Busy writers queue; uncertain native effects stay held. | Native completion/repeat, cleanup/reopen, stale-review, dangling-pointer and three intent/effect restart-window controls; existing removal writer-blocker units. |
| O3: parent, child and shared dependencies | No cascade. Review binds incarnation, configuration/credential generations and shared registrations. Missing parents, shared targets, new dependencies and stale choices refuse. | Retained parent/child/sibling/credential tests; real child/ancestor/global HTTP Git credentials and revoked-child refusal; stale and missing-parent native/API tests. |
| O4: confined owned data | Cleanup remains limited to the canonical managed root and one validated repository ID. Root/target symlink escapes refuse; unrelated owners and sibling paths remain. | Existing cleanup units, child/sibling HTTP controls, real symlink and missing-parent preflight regression. |
| O5: retained partial failures and safe retry | A completed side never becomes untouched. Local cleanup failures retain remote proof and ordered causes across database reopen. Unknown effects are not blindly repeated. | Real Git success followed by SVN rejection, real pre-receive Git rejection with unrequested SVN unchanged, cleanup failure/reopen and three restart windows. |
| O6: active-list removal, retention and disabled restore | Completed registration disappears; audit, mappings and tombstone remain indefinitely. Explicit restore returns a disabled registration and never recreates remotes. | Native completion/repeat/restore; retained remove/restore/remove and provenance tests; API lost-reply lookup and browser reload/explicit restore. |
| O7: no reset or history rollback as recovery | Removal does not invoke reset, force import, cursor rollback or repository deletion. Exact Git ref deletion uses the reviewed old-tip lease; SVN path deletion retains history. | Native ref/SVN-history and mapping assertions on success, failure and repeat; preserved legacy recovery units. |
| RS16-1: Pause and Remove differ | Root DELETE remains disable-only. Explicit disable preserves registration and both remotes; Remove reports actual durable stages. | Retained disable-versus-legacy-DELETE HTTP test and full legacy rendering comparisons. |
| RS16-2: identity-bound status and retry | Actor, request, pin, operation and repository incarnation are checked. Continue cannot approve different flags. Valid non-admin/foreign actions return 403; missing authentication returns 401. | Native changed-choice/reopen control; HTTP 401/403/foreign actor and unsupported DTO tests; UI account/route send-boundary tests. |
| RS16-3: explicit remote opt-in | Both new choices default false. Read-only exact-target review is required before approval. Legacy requests omitting flags or requesting remote deletion refuse with 409 before effects; explicit-false local removal remains supported. | Five migrated legacy transport controls; actual native Git/SVN positive cases; browser exact-target and false-flag request assertions. |
| RS16-4: receipts, dependency errors and navigation | Pending request is retained before sending. Read-only lookup recovers a lost response; no automatic effect retry occurs on reload. Storage failures preserve pending identity. Receipt links reach removed-row recovery, and only explicit restore mutates it. | New API lost-reply/request lookup/repeat test; UI storage/account/route regressions; actual browser lost-reply/reload/disabled-restore test. |

## Preserved and relocated legacy obligations

The original partial-remote failure was executed before correction. A provider
503 followed by actual SVN deletion returned HTTP 200/`ok:true` and discarded
the registration, scoped mapping and owned local evidence. Its failed assertion,
complete original test source and output are retained in the author evidence
packet; historical failures are not overwritten by the new refusal tests.

These existing transport selectors now exercise a deliberate safe transition:

| Existing selector | Current assertion | Positive obligation retained elsewhere |
| --- | --- | --- |
| `candidate_r65_branch_pair_remote_deletion_explicit_false_defaults` | Omitted flags refuse without effects; explicit false still removes locally on the same row. | Native exact-target deletion and browser false-default confirmation. |
| `candidate_r65_managed_remove_remote_deletion_outcomes` | Unreviewed requested remote actions refuse before provider contact or local cleanup. | `rs65_reviewed_native_completion_is_retained_repeatable_and_restorable` and real authenticated HTTP Git deletion. |
| `candidate_r65_managed_remove_retry_skips_recorded_remote_delete` | Legacy remote retry cannot become a fresh deletion authority. | `rs65_verified_remote_results_survive_cleanup_failure_and_database_reopen` proves cleanup retry preserves native remote proof without another SVN revision. |
| `candidate_r65_managed_remove_retry_ignores_unrequested_remote_failure` | Unsafe legacy request refuses; a separate explicit-false local action leaves remotes untouched. | `rs65_unrequested_svn_stays_untouched_and_failed_git_cannot_change_choices` proves real failed Git work cannot affect unrequested SVN or change choices on reopen. |
| `candidate_r65_managed_remove_missing_parent_fails_operation` | Missing-parent/unreviewed request refuses without fabricated operation or completion. | Native and HTTP reviewed missing-parent controls preserve custody before effects. |

The catalog's two retry IDs must select their actual native positive controls;
legacy refusal alone is not credited as positive remote-retry qualification.

## Qualification boundaries

The author packet records exact commands, source hashes, exit codes, counts and
original logs. Native fixtures use disposable local SVN and bare Git, including
an authenticated smart-HTTP server. API tests use synthetic sessions and owned
loopback listeners. Browser tests run the real UI against the explicitly closed
synthetic fixture in installed headless Chrome with its sandbox enabled; they
prove UI transport, focus and axe behavior, not actual remote effects.

Exact legacy rendered HTML is compared across twelve state/pending vectors.
Both real private foreign-journal readers consume genuine producer-completed v2
and valid v1 records, preserve tombstone guards, and refuse corrupt, duplicate,
unknown, future, partial and orphan evidence without mutation. Their test modules
are EOF-only; every protected production prefix and outside byte is audited.

Local tests are not independent review or hosted CI. Final independent source
review, exact published-head CI, normal epic integration and separate postmerge
verification are still required before issue closeout. The full browser run's
initial stale pre-#67 dialog selector failure is preserved alongside its eventual
correction and rerun; no assertion is removed to hide it.

This does not qualify #62's wider native/platform program, enable #69 refresh or
process recovery, perform #121's deferred actual-reader/usability checks, promote
the epic to main, install a service, deploy, change credentials/access or release
an incident hold. Unknown collection/effects remain held for explicit
investigation; no PID absence, acknowledgement, reset or journal deletion is
accepted as proof.
