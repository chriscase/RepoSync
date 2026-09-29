# PR74 74-04: import command cleanup certainty

The v12 import operation remains the authority for one repository. A cancellation request is a request to stop; it is not proof that the Git or SVN command, its output producers, or its external effects have stopped. The operation keeps its exact ID and active hold until a reviewed recovery path resolves uncertain cleanup.

| Supervised result | Import decision |
| --- | --- |
| Successful exit and output | Continue under existing import checks. |
| Nonzero finished Git apply | Full SVN export remains eligible when no cancellation is requested. Other known failures use their existing explicit paths. |
| Typed confirmed interruption, including before spawn | Return safe cancellation when the exact operation requested it. |
| Typed confirmed deadline | Stop with a held failure; no next import work. |
| Typed unconfirmed group termination or output completion | Stop with a held failure or reconciliation-required state, regardless of the cancellation flag. Never run fallback export, later revision, commit, push or checkpoint advance. |
| Spawn or pre-execution failure | Stop under the existing held error path. |

`process::run` distinguishes confirmed stop from unconfirmed cleanup with error types, not message text. Git and SVN I/O wrappers retain these types. SVN info, log, diff and export only treat a typed confirmed interruption as safe cancellation; incremental Git fallback requires a finished `ApplyFailed` result. LFS preparation and local commit errors hold the operation, while publication retains its previously journaled intent and reconciliation-required path. Clone and target inspection set an explicit preparation cleanup flag; the preparation guard refuses to infer quiescence from an empty journal when that flag is set. The worker's durable failure detail includes the full error chain, including “cleanup unconfirmed.”

The `reliability-fixture` build has a sealed command fault point. It records only command stages in a disposable fixture directory and returns the typed unconfirmed result at an exact stage before spawning that fixture command. A ready/release barrier lets the actual API record cancellation before the error is delivered. The real importer, SVN/Git wrappers, v12 operation journal, status route and finalizers then execute. This deterministic injection proves result propagation and no-next-work decisions; it does not claim to reproduce an OS kill failure. The process classifier test covers both group-termination failure and output/reap timeout with and without cancellation. Existing Unix tests independently prove ordinary child and same-group descendant cancellation. A descendant that escapes the process group, or an output producer that cannot be confirmed stopped, remains an uncertainty requiring the durable hold; this increment does not add general containment or automated recovery.

The 104 prior exact required cases remain registered. Six new exact cases cover the classifier, Git apply without cancellation, SVN diff with cancellation, SVN info/log/export with cancellation, clone/inspection preparation with cancellation, and healthy finished-patch fallback. The fault cases compare command traces, local refs/tree/index/workdir, mappings, checkpoint, remote refs and reopened operation state as applicable. The existing successful import, ordinary cancellation, reset refusal, LFS and mounted-browser cases remain required controls.

Ordinary startup and storage remain at v12. Candidate v13/v14 migration and inspection routes remain unactivated. The original GOAL and prior briefs remain unchanged. The continuation brief is preserved byte-for-byte at `RS64-A-quiescence-goal.md` (SHA-256 `5de17b77a08fdd58a86f634acbc8fa54f4b3cc9ad1cd65819124a866922d031d`). This correction stops at independent review; it does not merge or activate any runtime path.
