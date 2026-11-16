"""Surface/state matrix rows for #263.

Each row: id, route or scope, surface, source file (under web-ui/src unless it starts with another root),
anchor (a literal string found in that file; build resolves it to a line at each epic revision),
current label and path, role and mode conditions, action and API boundary, states, prototype slugs,
status, kind, note. `epic` marks rows whose source differs between the two epics.
"""
E = "existing"
D = "existing, known defect"
H = "held or unsupported"
P = "design proposal"
N = "needs separately owned feature work"

ROWS = []


def R(id, route, surface, file, anchor, label, role, api, states, slugs, status="covered", kind=E, note="none", epic="both", anchor_r=None):
    ROWS.append(dict(id=id, route=route, surface=surface, file=file, anchor=anchor, anchor_r=anchor_r or anchor, label=label, role=role, api=api,
                     states=states, slugs=slugs if isinstance(slugs, list) else [slugs], status=status, kind=kind, note=note, epic=epic))


ANY = "Any signed-in account"
ADM = "Shown to administrators; the page hides it from other accounts"
ADM_PAGE = "Administrators. The page shows other accounts a refusal, and the navigation hides it"
EVERY_HDR = "Every signed-in account, as for the header (row G04.1)"
SAVE_FORM = "Follows the form that saves: the Add Repository dialog (row G03.2) or the repository edit view (row G04.1)"
PAIR_PAGE = "Every signed-in account, on a branch pair's page. The row action in the Branch pairs table is shown to administrators (row G04.20)"
PUB = "Not signed in"
SES_CFG = "Any signed-in account by direct address: the navigation hides the page from other accounts, and issue #268 lists the setup and config calls that need only a session"
SES_SETUP = SES_CFG + ". Some setup routes check nothing until the first account or admin password exists (issue #268)"

# ---- G01 /login
R("G01.1", "/login", "Sign-in form", "pages/Login.tsx", "export default function Login()", "“Sign in to the sync dashboard”; Username, Password, Sign in", PUB,
  "POST /api/auth/login", "ready; pending (“Signing in...”); invalid credentials (server message shown, password cleared, username kept)", ["g01-login", "g01-login-states"],
  note="Sign-in keeps one dark appearance for everyone (decision D5). The page is the same file under each option and ignores the theme")
R("G01.2", "/login", "Sign-in help (directory hint)", "pages/Login.tsx", "api.getAuthInfo()", "“Corporate login enabled (domain)”; password placeholder “Network password”", PUB,
  "GET /api/auth/info (no session)", "loaded with directory; loaded without; read failed (ignored, hint absent)", ["g01-login", "g01-login-states"], kind=D,
  note="A failed read is swallowed; the design shows the form without the hint and says so in a prototype note. Sign-in keeps the existing wording, “Corporate login enabled”, because it has one appearance (decision D5)")
R("G01.3", "/login", "Already signed in", "pages/Login.tsx", "if (clientSession().snapshot().token) return <Navigate", "Redirect to /", "Signed-in account", "none (client redirect)",
  "redirect", ["g01-login-states"])
R("G01.4", "/login", "Session ended, arriving at sign-in", "session.ts", "Session expired — please log in again", "No text on the sign-in page; the reason is thrown, then the page reloads at /login", ANY,
  "Any authenticated request returning 401", "session ended", ["g01-login-states", "s07-exceptional"], status="explicit gap", kind=N,
  note="The sign-in page cannot know why it was opened; showing the reason needs it carried across the redirect")
R("G01.5", "/login", "Server not set up", "crates/web/src/api/auth.rs", "initial setup required", "Server refusal “initial setup required — please complete the setup wizard”", PUB,
  "Any session check before a first administrator exists (403)", "refused", ["g01-login-states"], status="explicit gap", kind=N,
  note="No page handles this refusal specifically; the design shows the intended message and link")

# ---- G02 /
R("G02.1", "/", "Repository scope", "pages/Dashboard.tsx", 'aria-label="Repository scope"', "Select “All Repositories” or one repository; kept in ?repo_id=", ANY, "GET /api/repos; URL parameter only",
  "all; one repository; repository in the address no longer registered", ["g02-dashboard", "g02-dashboard-single"])
R("G02.2", "/", "Import progress cards", "pages/Dashboard.tsx", "<ImportProgressCard", "“SVN Import — name” card per root repository", ANY, "GET /api/repos/:id/import/status (2 s poll)",
  "hidden when idle; active; completed within the hour; failed; stopped; status unavailable", ["g02-dashboard", "g02-dashboard-states", "g04-operations"])
R("G02.3", "/", "Repository cards and branch tree", "pages/Dashboard.tsx", "Repositories Overview", "“Repositories”, “Manage Repositories →”, “Configured enabled/disabled”", ANY, "GET /api/repos; GET /api/status?repo_id= per pair (15 s)",
  "ready; none; per-pair status unavailable or stale", ["g02-dashboard", "g02-dashboard-single", "g02-dashboard-states"])
R("G02.4", "/", "Aggregate health and counts", "pages/Dashboard.tsx", "<StatusObservationPanel view={statusView}/>", "“Effective sync health”, “Total Syncs”, “Active Conflicts”, “Errors (24h)”, “Uptime”, “Last Sync”", ANY, "GET /api/status (5 s poll)",
  "observed; stale (older than 45 s or refetch failed); unavailable; malformed; loading; uptime unavailable", ["g02-dashboard", "g02-dashboard-states"],
  note="Keeps its place after Repositories, with the acknowledgement. The aggregate read returns the worst health and one evidence flag; it returns no counts")
R("G02.5", "/", "Error acknowledgement", "components/ErrorAcknowledgement.tsx", "window.confirm(acknowledgementPrompt", "“Acknowledge” on the Errors card; browser prompt", ADM, "POST /api/status/acknowledge-errors {scope, observation}",
  "unavailable (no errors or no observation); confirming; pending; acknowledged (count); failed", ["g02-dashboard", "g02-dashboard-states"], kind=P,
  note="Prompt wording kept; shown in the shared dialog instead of the browser prompt")
R("G02.6", "/", "Sync position (one repository)", "pages/Dashboard.tsx", "Sync Position", "“Last SVN Revision”, “Last Git Hash”, “Last Sync”, “Total Conflicts”", ANY, "GET /api/status?repo_id=",
  "recorded; “Not recorded”; “Unknown”", ["g02-dashboard-single"])
R("G02.7", "/", "Recent sync records", "pages/Dashboard.tsx", "Recent Sync Records", "“Recent Sync Records”, grouped stream", ANY, "GET /api/sync-records?limit=20", "ready; empty; read failed",
  ["g02-dashboard", "g02-dashboard-states"], kind=D, note="A failed read renders “No sync records yet” today; the design shows a failed state")
R("G02.8", "/", "Commit map", "pages/Dashboard.tsx", "Commit Map (SVN", "“Commit Map (SVN ↔ Git)” table", ANY, "GET /api/commit-map?limit=15", "ready; empty; read failed; no-target rows (partial-52 only)",
  ["g02-dashboard", "g04-activity"], epic="differs: partial-52 renders CommitMapTargetCell; reliability-61 prints the SHA", kind=D, note="Failed read renders as empty today")
R("G02.9", "/", "Recent activity", "pages/Dashboard.tsx", "Recent Activity", "“Recent Activity”", ANY, "GET /api/audit?limit=20", "loading; ready; empty (“No matching activity”); read failed with Retry",
  ["g02-dashboard", "g02-dashboard-states"])
R("G02.10", "/", "Server monitor", "components/ServerMonitor.tsx", "refetchInterval: 15000", "Collapsible server panel: disk, memory, CPU, network, Git push, SVN", ANY, "GET /api/status/system (15 s)",
  "collapsed; expanded; no data yet", ["g02-dashboard", "g02-dashboard-states"])
R("G02.11", "/", "Needs attention", "pages/Dashboard.tsx", "Repositories Overview", "none: what needs a person is spread across the repository cards and the count cards", ANY,
  "none of its own: derived from GET /api/repos and the status reads the page already makes", "nothing needs you; one or more pairs held, paused, in conflict or in error; status unavailable",
  ["g02-dashboard", "g02-dashboard-single"], kind=P,
  note="New, read-only and first in the document. It holds links only. The count and the per-state counts come from the status read the page already makes for each pair; a pair whose read fails or goes stale is counted as Unknown. Present in every layout and hidden in Classic, Individual and Team (decision D12)")

# ---- G03 /repos
R("G03.1", "/repos", "Repository list", "pages/Repositories.tsx", "function RepositoriesEditor()", "“Repositories” with count; cards with branch tree; “Add Repository”", ANY + "; “Add Repository” is shown to administrators", "GET /api/repos; per-pair status reads",
  "loading; ready; empty; read failed", ["g03-repositories", "g03-states"])
R("G03.2", "/repos", "Add Repository dialog", "pages/Repositories.tsx", "Add Repository Modal", "“Add Repository”: name; SVN URL, branch, username, password; provider, API URL, repository, default branch, token; sync mode, poll interval, LFS threshold, Auto Merge, Enabled", ADM,
  "POST /api/repos (save_request_id); POST /api/repos/:id/credentials", "pristine; edited; saving; saved; closed with draft kept", ["g03-add-repository", "g03-save-recovery"])
R("G03.3", "/repos", "Connection tests in the dialog", "pages/Repositories.tsx", "async function handleTestSvn()", "“Test SVN Connection”, “Test Git Connection”", "The dialog is offered to administrators. The two test calls check only for a session (issue #268)", "POST /api/setup/test-svn; POST /api/setup/test-git",
  "idle; testing; passed; failed", ["g03-add-repository"], kind=D, note="The result gains a mark, a time and a live region (display only). The two test calls are the setup tests: the server does not limit them to administrators (issue #268)")
R("G03.4", "/repos", "Initial import choice", "components/ImportModeFields.tsx", "export default function ImportModeFields", "“Initial SVN import”: Full SVN history; Snapshot at pinned SVN revision (HEAD or number)", ADM,
  "Kept as a preference; sent later with POST /api/repos/:id/import", "full; snapshot; invalid revision", ["g03-add-repository", "g04-operations"])
R("G03.5", "/repos", "Save and recovery notice", "components/RepositorySaveNotice.tsx", "export default function RepositorySaveNotice", "“Recorded configuration stage … Credential stage …”; “Load current settings for review”", SAVE_FORM,
  "GET /api/repository-save-requests/:id; GET /api/repos/:id/save-requests/:id", "pending; saved; unknown; failed; conflict; protocol failure; reviewed settings; prior attempts retained",
  ["g03-save-recovery", "s04-recoverable-saves"])
R("G03.6", "/repos", "Removal notices on the list", "components/ManagedRemovalReceiptNotice.tsx", "export default function ManagedRemovalReceiptNotice", "Receipt banners; “View removal status and recovery”", "Any signed-in account whose browser holds a stored receipt",
  "Browser-stored receipts; GET /api/repos/:id/removal", "completed; not complete; cleanup warnings; partial cleanup receipt", ["g03-repositories", "g03-states", "g04-removal-recovery"])

# ---- G04 /repos/:id  (RepoDetail.tsx differs between the epics; lines are resolved per epic)
RD = "pages/RepoDetail.tsx"
R("G04.1", "/repos/:id", "Header and actions", RD, "function RepositoryDetailEditor()", "Name; “Sync configured enabled/disabled”; “Turn sync off/on”; “Edit”; “Git Hook”; “Trigger Sync”", "Every signed-in account sees the header controls",
  "PUT /api/repos/:id {enabled}; POST /api/repos/:id/sync", "ready; sync request recorded; action unavailable (status forbids acting); request failed", ["g04-overview", "g04-states"], kind=P,
  note="“Trigger Sync” reads “Request sync” in every layout once the sync owner confirms what the call does (decision D11). “Sync configured enabled/disabled” is kept word for word. The header shows Edit, the sync switch and Trigger Sync to every account today; the design keeps that. The Settings section and the edit view follow the header (rows G04.2 to G04.6)")
R("G04.2", "/repos/:id", "SVN settings, read and edit", RD, "SVN Configuration", "“SVN Configuration”: Name, URL, Branch, Username, Password; “Test SVN Connection” in the edit view", EVERY_HDR, "PUT /api/repos/:id (save_request_id, expected_updated_at); POST /api/repos/:id/test-svn",
  "read; editing; invalid; saving; saved; conflict; test passed or failed (edit view)", ["g04-settings", "g04-settings-edit"],
  note="The test is in the edit view only, as today. The label differs in the plain vocabulary (E3); Classic, Individual and Team keep it")
R("G04.3", "/repos/:id", "Git settings, read and edit", RD, "Git Configuration", "“Git Configuration”: Provider, API URL, Repository, Default Branch, Token; “Test Git Connection” in the edit view", EVERY_HDR, "PUT /api/repos/:id; POST /api/repos/:id/test-git",
  "read; editing; saving; saved; conflict; test passed or failed (edit view)", ["g04-settings", "g04-settings-edit"],
  note="The test is in the edit view only, as today. The label differs in the plain vocabulary (E3); Classic, Individual and Team keep it")
R("G04.4", "/repos/:id", "Repository credentials", RD, "credStatus?.svn_password_set", "“●●●●●●● (saved)” or “Not set”; password and token inputs in edit mode", EVERY_HDR, "GET and POST /api/repos/:id/credentials",
  "set; not set; replacing; credential stage unknown", ["g04-settings", "g04-settings-edit", "s06-credentials-identity"], status="explicit gap", kind=N,
  note="The read returns two booleans; own, inherited, server-wide and revoked cannot be told apart, and the form cannot send the documented empty-string revocation")
R("G04.5", "/repos/:id", "Sync settings", RD, "Sync Settings", "“Sync Mode”, “Poll Interval”, “LFS Threshold”, “Auto Merge”", EVERY_HDR, "PUT /api/repos/:id", "read; editing; saved", ["g04-settings", "g04-settings-edit"],
  note="The label differs in the plain vocabulary (E3); Classic, Individual and Team keep it")
R("G04.6", "/repos/:id", "Path policy", RD, "<PathPolicyEditor56 draft={form.path_policy}", "partial-52: “Path policy” lists with Git→SVN preview. reliability-61: “Allowed SVN Paths”, “Blocked Patterns” text areas", EVERY_HDR,
  "PUT /api/repos/:id (allowed_paths, blocked_patterns)", "partial-52: keep, edit, clear, malformed, unavailable, per-entry validation, preview. reliability-61: free text", ["g04-policy"],
  epic="differs: #56 editor on partial-52; text areas on reliability-61", anchor_r="Allowed SVN Paths", note="Both variants are drawn; behaviour of each editor is unchanged")
R("G04.7", "/repos/:id", "Hook download", RD, "hooks/pre-commit", "“Git Hook” link, shown when a path policy has rules", ANY, "GET /api/repos/:id/hooks/pre-commit", "available; not offered (no rules); download failed",
  ["g04-settings", "g04-states"], kind=D, note="A plain link cannot send the session header; the design uses a button that fetches with the session")
R("G04.8", "/repos/:id", "Repository facts", RD, "Quick Stats", "“LFS Threshold”, “Created”, “Last Updated”, “Created By”", ANY, "GET /api/repos/:id", "ready", ["g04-settings"],
  note="The label differs in the plain vocabulary (E3); Classic, Individual and Team keep it")
R("G04.9", "/repos/:id", "Paused after error", RD, "Circuit Breaker: Error Paused Banner", "“Sync Paused — Permanent Error”; “Skip Commit”; “Retry”", ADM + "; only while scoped status is fresh and paused",
  "GET /api/repos/:id/skip-commit/context; POST /api/repos/:id/retry", "shown; hidden when status is held, stale or unknown; resume pending; resume failed", ["g04-skip", "g04-states"], kind=P,
  note="“Retry” is drawn as “Resume sync…” with a confirmation, a pending state and an error state")
R("G04.10", "/repos/:id", "Skip selected pending commits", RD, 'title="Skip selected pending commits"', "Dialog: “Skip selected pending commits”; checkboxes; “Skip selected”", ADM,
  "POST /api/repos/:id/skip-commit {pinned_cursor, selected_commits, expected_remote_tip, expected_bridge_tip, reason}", "none selected; selected prefix; pending; refused (status changed); refused by server; closed when status becomes held or unknown",
  ["g04-skip"], kind=P, note="Prefix choice drawn as radios; the request is unchanged")
R("G04.11", "/repos/:id", "Status and evidence", RD, "<StatusObservationPanel view={statusView}/>", "Evidence paragraphs; status cards", ANY, "GET /api/status?repo_id= (5 s)",
  "observed; stale; unavailable; malformed; held; operations listed; details restricted", ["g04-overview", "g04-states", "s05-operations"])
R("G04.12", "/repos/:id", "Import baseline notice", RD, "<ImportHistoryNotice fields={baselineNoticeFields}", "“Snapshot at pinned SVN revision rN”; “Snapshot import refused”", ANY, "GET /api/repos/:id; GET /api/repos/:id/import/status",
  "verified snapshot boundary; refused or failed snapshot; none", ["g04-operations"])
R("G04.13", "/repos/:id", "Import start, progress, stop, verify", "components/ImportProgressCard.tsx", "function ImportProgressSubject", "“SVN Import”; “Start full history import” / “Start snapshot import”; “Stop import”; “Verify remote”",
  "Start, stop and verify are shown to administrators", "POST /api/repos/:id/import; POST …/import/:op/cancel; POST …/import/:op/reconcile",
  "never run; queued; running (measured, unmeasured); stop requested; cancelled; reconciliation required; failed; completed; verification differs; status stale", ["g04-operations", "s05-operations"], kind=P,
  note="“Stop import” acts in one click today. The design adds a confirmation that says what stopping does not undo (a shared change, its own change)")
R("G04.14", "/repos/:id", "Resume a verified partial import", "crates/web/src/api/repos.rs", "/api/repos/:id/import/:operation_id/resume", "No control exists", "No control exists", "POST /api/repos/:id/import/:operation_id/resume (registered; no UI caller)",
  "resume authorised", ["g04-operations"], status="explicit gap", kind=N, note="Drawn with a dashed marker as an owner decision; not part of parity claims")
R("G04.15", "/repos/:id", "Sync records", RD, "function SyncRecordRow", "“Sync Records”, expandable rows", ANY, "GET /api/sync-records?repo_id=", "ready; expanded; empty; read failed", ["g04-activity", "g04-overview"], kind=D,
  note="Failed read renders as empty today")
R("G04.16", "/repos/:id", "Commit map", RD, "Commit Map (SVN", "“Commit Map (SVN ↔ Git)”", ANY, "GET /api/commit-map?repo_id=", "ready; empty; no-target rows (partial-52)", ["g04-activity"],
  epic="differs: CommitMapTargetCell on partial-52 only")
R("G04.17", "/repos/:id", "Recent audit log", RD, "Recent Audit Log", "“Recent Audit Log”, grouped rows, “Show group”, expandable details", ANY, "GET /api/audit?repo_id=&limit=10", "ready; grouped; expanded; empty", ["g04-activity"])
R("G04.18", "/repos/:id", "Update pair from parent (preview)", RD, "Update pair from parent", "“Update pair from parent”; “Preview update from parent”", PAIR_PAGE, "POST /api/repos/:id/refresh {operation: update_pair_from_parent, execute: false}",
  "not requested; previewing; preview shown; preview failed; rewritten lineage; conflicts reported", ["g04-refresh-preview"], kind=H, note="Read-only preview only. Owned by #69; relocated and labelled, not redesigned")
R("G04.19", "/repos/:id", "Re-anchor boundary", RD, "reanchor-not-implemented", "“Re-anchor pair (not implemented)”; row action “Re-anchor”", PAIR_PAGE, "POST /api/repos/:id/refresh {operation: reanchor}",
  "held", ["g04-refresh-preview", "s07-exceptional"], status="covered", kind=H, note="Shown as held with no execute control. The page copy and the server's reported status disagree at the pin; that is for the #69 owner")
R("G04.20", "/repos/:id", "Branch pairs table", RD, "Branch Pairs", "“Branch Pairs”; “Add Branch Pair”; row actions “Preview update”, “Re-anchor”, delete icon", ANY + "; “Add Branch Pair” and the row actions are shown to administrators", "GET /api/repos/:id/branches",
  "ready; empty (“No branch pairs configured”)", ["g04-branches", "g04-overview"])
R("G04.21", "/repos/:id", "Add branch pair (late pairing)", RD, 'title="Review Branch Pair"', "Dialog “Review Branch Pair”: Branch Name, SVN Branch Path, include parent, pairing mode; “Preview Pair”, “Publish reviewed result”, “Inspect operation”, “Reconcile / resume”, “Cancel operation”", ADM,
  "POST /api/repos/:id/branches {action: preview|publish|status|resume|cancel, review_pin}", "validation; preview; refused; blocked by conflicts; published and in progress; held; cancelled; completed", ["g04-pairing"],
  note="Both pairing modes are drawn as today, including “Start from now (unsafe — refused)”. “Cancel operation” reads “Stop operation…” in every layout (decision D11)")
R("G04.22", "/repos/:id", "Stop or remove section", RD, "Danger Zone - admin only", "“Danger Zone”: “Reset & Reimport” (text only); “Pause / disable sync (explicit API)”; “Remove from RepoSync” or “Remove branch pair”", ADM, "see the three rows below",
  "listed; reset and reimport unavailable", ["g04-settings"],
  note="The explicit pause is offered for a repository only (RepoDetail.tsx, `!repo?.parent_id`); a branch pair has the one-click switch. The section label differs in the plain vocabulary (E3)")
R("G04.23", "/repos/:id", "Pause sync", RD, 'title="Pause / disable sync"', "Dialog “Pause / disable sync” (repositories only); header “Turn sync off/on” (repositories and branch pairs)", "The dialog: shown to administrators, in the Stop or remove section (row G04.22). The header switch: every signed-in account (row G04.1)", "POST /api/repos/:id/disable; PUT /api/repos/:id {enabled}",
  "confirming; pending; paused; failed; turned on; turn-on failed", ["g04-pause"], kind=P, note="For a repository: one pause control using the explicit call and its dialog. For a branch pair nothing changes: it keeps the one-click switch, and its failure is shown. The explicit call is not extended to pairs")
R("G04.24", "/repos/:id", "Remove from RepoSync (root)", RD, 'testId="managed-remove-modal"', "Dialog “Remove from RepoSync”; exact-target review; dependency preview", ADM,
  "POST /api/repos/:id/removal/review; GET …/removal/preview; POST …/removal/approve {review_pin, request_id}", "reviewing; review unavailable; blocked by child pairs; preview unavailable; earlier request unresolved; confirming; pending", ["g04-remove-root"])
R("G04.25", "/repos/:id", "Remove branch pair, optional remote deletion", RD, 'testId="delete-branch-modal"', "Dialog “Delete branch pair”; “Delete Git branch”, “Delete SVN branch” (both off); typed confirmation; “Delete Branch Pair”", ADM,
  "POST /api/repos/:id/removal/approve {delete_git, delete_svn, review_pin}", "review; options off; an option on; typed name missing; pending; failed", ["g04-remove-pair"], kind=P,
  note="Typed confirmation and defaults unchanged; the title and confirm label say “Remove” unless a remote deletion is ticked")
R("G04.26", "/repos/:id", "Removal progress, receipt, retry, restore", "components/ManagedRemovalPanel.tsx", "export default function ManagedRemovalPanel", "Panel headings from managedRemoval.ts; “Retry …”; “Restore registration (stays disabled)”", ADM,
  "GET /api/repos/:id/removal; POST …/removal/continue; POST …/restore; GET …/removal/request", "queued; cancelling; running; failed; reconciliation required; completed; evidence unavailable; lost reply; restore failed",
  ["g04-removal-recovery", "s05-operations"])
R("G04.27", "/repos/:id", "Repository not found or removed", RD, "repo-not-found", "“Repository not found”; “Back to repositories”; retained removal evidence for administrators", ANY, "GET /api/repos/:id (404)", "not found; removed with receipt; verification unavailable",
  ["g04-removal-recovery", "s07-exceptional"])
R("G04.28", "/repos/:id", "Loading and read failure", RD, "Loading repository...", "“Loading repository...”; “Error loading repository: …”", ANY, "GET /api/repos/:id", "loading; failed", ["g04-states"])
R("G04.29", "/repos/:id", "Server monitor on the repository page", RD, "<ServerMonitor />", "Server panel repeated on every repository page", ANY, "GET /api/status/system", "as G02.10", ["g04-overview", "g02-dashboard"], kind=P,
  note="Stays on the repository page as the last part of Overview, collapsed. Nothing is removed from any layout")
R("G04.30", "/repos/:id", "Errors card and acknowledgement (scoped)", RD, 'title="Errors (24h)"', "“Errors (24h)” with “Acknowledge”", "Every signed-in account sees the card; “Acknowledge” is shown to administrators (row G02.5)", "POST /api/status/acknowledge-errors {scope: repository}", "none; some; acknowledged; failed", ["g04-overview", "g02-dashboard-states"])

# ---- G05, G06
R("G05.1", "/conflicts", "Filters", "pages/Conflicts.tsx", 'aria-label="Repository filter"', "Repository select; “All”, “Detected”, “Queued”, “Deferred”, “Resolved”", ANY, "GET /api/conflicts?page=&per_page=&status=&repo_id=",
  "all; filtered; repository in the address no longer registered; invalid page", ["g05-conflicts", "g05-conflicts-states"])
R("G05.2", "/conflicts", "Conflict table and paging", "pages/Conflicts.tsx", "function ConflictRow", "Repository, File, Type, Status, Created, “Resolve” or “View”; “N matching conflicts · Page x / y”", ANY, "same read",
  "loading; ready; no matches; nothing on this page; read failed with Retry; malformed page", ["g05-conflicts", "g05-conflicts-states", "s07-exceptional"])
R("G06.1", "/conflicts/:id", "Header and facts", "pages/ConflictDetail.tsx", "function ConflictEditor()", "File path; status badge; Type, SVN Revision, Git SHA, Created", ANY, "GET /api/conflicts/:id", "ready; loading", ["g06-conflict-diff"], kind=P,
  note="Repository shown (the payload carries repo_id); status uses the real value")
R("G06.2", "/conflicts/:id", "Side-by-side diff", "components/DiffViewer.tsx", "export default function DiffViewer", "“Side-by-Side Diff”: “SVN Version”, “Git Version”", ANY, "none (content in the read)", "text diff, for every file type", ["g06-conflict-diff", "g06-conflict-states"], kind=P,
  note="The diff compares SVN with Git, as today, and is offered for every file type, as today. The common base is added as a third panel (already in the read). The diff follows the theme; today it renders light")
R("G06.3", "/conflicts/:id", "Manual edit", "pages/ConflictDetail.tsx", "Edit merged content:", "“Manual Edit”: “Edit merged content:”", ANY, "none until a decision is sent", "editing", ["g06-conflict-edit"])
R("G06.4", "/conflicts/:id", "Resolution and defer", "pages/ConflictDetail.tsx", "resolution: 'accept_svn'", "“Accept SVN”, “Accept Git”, “Apply Manual Edit”, “Defer”", ANY, "POST /api/conflicts/:id/resolve {resolution, merged_content}; POST /api/conflicts/:id/defer",
  "choosing; confirming; pending; failed; deferred; resolved", ["g06-conflict-diff", "g06-conflict-edit", "g06-conflict-states"], kind=P,
  note="Labels kept; one confirmation added; a notice states that the server records the decision and does not write content (issue #73). Nothing is preselected, and “Record decision…” is unavailable until a choice is made")
R("G06.5", "/conflicts/:id", "Applied and verified states", "crates/web/src/api/conflicts.rs", "pub struct ResolveConflictRequest", "none", ANY, "none: the request carries only `resolution`", "requested; applying; held; verified", ["g06-conflict-states"],
  status="explicit gap", kind=H, note="Open under #73. The design reserves the place and claims nothing")
R("G06.6", "/conflicts/:id", "Resolved record", "pages/ConflictDetail.tsx", "Resolved as", "“Resolved as <resolution> by <resolved_by> on <date>”", ANY, "GET /api/conflicts/:id", "resolved", ["g06-conflict-states"])
R("G06.7", "/conflicts/:id", "Not found and read failure", "pages/ConflictDetail.tsx", "if (isLoading || !conflict)", "“Loading...” in every non-ready case", ANY, "GET /api/conflicts/:id", "loading; not found; failed", ["g06-conflict-states"], kind=D,
  note="No error branch today; the design shows not-found and failed states")

# ---- G07
R("G07.1", "/audit", "Filters, counts and view mode", "pages/AuditLog.tsx", "const ACTION_TYPES", "Repository, action and success filters; “Stream” / “Table”; “N successes / failures on this page”; “Showing x of y matching entries”", ANY,
  "GET /api/audit?limit=&page=&success=&repo_id=&action=", "all; filtered; action not in the list; invalid filter", ["g07-audit", "g07-audit-table", "g07-audit-states"], kind=P,
  note="Action list drawn from the actions production writes; the fixed list mostly names seed-only actions")
R("G07.2", "/audit", "Stream view", "pages/AuditLog.tsx", "function AuditStreamView", "Grouped activity stream with date separators", ANY, "same read", "ready; grouped", ["g07-audit"])
R("G07.3", "/audit", "Table view and details", "pages/AuditLog.tsx", "function AuditRow", "Time, Action, Direction, Details, Author, SVN Rev, Git SHA; “Show audit details”", ANY, "same read", "ready; row expanded", ["g07-audit-table"], kind=P,
  note="Outcome as a mark and a word; repository column added")
R("G07.4", "/audit", "Paging and list states", "pages/AuditLog.tsx", "Error loading audit log", "“← Previous”, “Page x of y”, “Next →”; “Return to first page”", ANY, "same read (15 s refresh)",
  "loading; no matches; nothing on this page; read failed with Retry; stale after a failed refresh; malformed page", ["g07-audit-states"])

# ---- G08
R("G08.1", "/settings", "My profile", "pages/UserSettings.tsx", "function ProfileSection", "“My Profile”: Username, Role (read-only), Display Name, Email; “Save Profile”", ANY, "PUT /api/users/:id {display_name, email}", "ready; saving; saved; failed",
  ["g08-account", "g08-account-states"])
R("G08.2", "/settings", "Directory-controlled profile", "pages/UserSettings.tsx", "const isLdap = user.role === 'ldap'", "Display name and email disabled; no Save; no password section", "An account whose role is `ldap`", "none", "read-only; not reached by directory accounts today", ["g08-account-states", "s06-credentials-identity"], kind=D,
  note="The page draws this branch only for the role `ldap` (UserSettings.tsx). The server gives accounts it creates from the directory the role `user`, so they do not reach it today")
R("G08.3", "/settings", "Change password", "pages/UserSettings.tsx", "Change Password", "“Change Password”: Current, New, Confirm", "Every signed-in account except the role `ldap` (row G08.2)", "PUT /api/users/:id {password, current_password}",
  "empty; current missing; mismatch; too short; changing; changed; failed", ["g08-account", "g08-account-states"])
R("G08.4", "/settings", "Personal credential cards", "pages/UserSettings.tsx", "function CredentialCard", "“My Credentials”: SVN Password, SVN Token, Git Token; “Configured” / “Not configured”; Save, Test Connection, Delete", ANY,
  "GET, POST, DELETE /api/users/:id/credentials. “Test Connection” calls POST …/credentials/:id/test, which is not registered", "not configured; configured; saving; save failed; test shows the client's error; deleting",
  ["g08-account", "g08-account-states", "s06-credentials-identity"], kind=D,
  note="Token cards send an empty username the server rejects, and a blank value replaces the stored secret; drawn with explicit Replace and Delete steps. “Test Connection” calls a route that is registered at neither revision, "
       "so the test can only show the client's error text, and it is drawn that way. What reads these credentials is unconfirmed")
R("G08.5", "/settings", "Account unavailable", "pages/UserSettings.tsx", "Unable to load user information", "“Unable to load user information. Please log in again.”", ANY, "GET /api/auth/me", "unavailable", ["g08-account-states"])
R("G08.6", "/settings", "A personal credential test that works", "pages/UserSettings.tsx", "api.testCredential(userId, existing.id)", "none: “Test Connection” has no route to call", ANY,
  "none registered", "passed; failed with a reason", ["g08-account-states"], status="explicit gap", kind=N,
  note="Needs a server route and an owner. Until then no passing or reasoned result is drawn (row G08.4)")

# ---- G09
R("G09.1", "/users", "User list", "pages/AdminUsers.tsx", "queryKey: ['admin-users']", "Users table: role, “Active” / “Disabled”; Edit; Delete", ADM_PAGE, "GET /api/users", "loading; ready; read failed; not an administrator", ["g09-users", "g09-users-states"],
  note="No sign-in source is shown, because the server reports none (row S34). The section label differs in the plain vocabulary (E3)")
R("G09.2", "/users", "Create user", "pages/AdminUsers.tsx", 'title="Create User"', "Dialog “Create User”: Username, Display Name, Email, Password, Role", ADM_PAGE, "POST /api/users", "editing; saving; failed", ["g09-user-dialogs"])
R("G09.3", "/users", "Edit user", "pages/AdminUsers.tsx", "title={`Edit User: ${user.username}`}", "Dialog “Edit User: <username>”: Display Name, Email, Role, “Account enabled”", ADM_PAGE, "PUT /api/users/:id", "editing; saving; failed", ["g09-user-dialogs"])
R("G09.4", "/users", "Delete user and own-account guard", "pages/AdminUsers.tsx", 'title="Delete User"', "Confirm “Delete user ‘name’? This cannot be undone.”; Delete disabled on your own row", ADM_PAGE, "DELETE /api/users/:id", "confirming; pending; failed; unavailable for own account",
  ["g09-user-dialogs", "g09-users-states"])

# ---- G10
R("G10.1", "/ldap", "Directory settings", "pages/AdminLdap.tsx", 'id="ldap-enabled"', "Enable switch; LDAP URL, Base DN, Search filter; display name, email and group attributes", ADM_PAGE, "GET and PUT /api/admin/ldap", "loading; ready; disabled; saving; saved; save failed",
  ["g10-directory", "g10-directory-states"])
R("G10.2", "/ldap", "Service account and secret", "pages/AdminLdap.tsx", "bind_password: bindPassword || undefined", "Bind DN; Bind password (“Leave blank to keep current”)", ADM_PAGE, "PUT /api/admin/ldap (empty keeps the saved password)", "saved; not saved; replacing",
  ["g10-directory", "g10-directory-states", "s06-credentials-identity"], note="There is no way to clear the saved password through the API; stated in the prototype")
R("G10.3", "/ldap", "Test connection", "pages/AdminLdap.tsx", "const handleTest = async", "“Test Connection”", ADM_PAGE, "POST /api/admin/ldap/test", "testing; passed; failed (entries kept)", ["g10-directory-states"])
R("G10.4", "/ldap", "Not an administrator", "pages/AdminLdap.tsx", "You need admin privileges to view this page.", "“You need admin privileges to view this page.”", "Non-administrator", "none", "denied", ["g10-directory-states", "s07-exceptional"],
  note="The label differs in the plain vocabulary (E3); Classic, Individual and Team keep it")

# ---- G11
R("G11.1", "/config", "Author identity mappings", "pages/Config.tsx", "Author Identity Mappings", "“Author Identity Mappings”: SVN Username, Git Name, Git Email, GitHub; Remove; “Add New Mapping”", "Any signed-in account by direct address: the server checks only for a session, for reading and for replacing (issue #268). The navigation hides the page from non-administrators",
  "GET and PUT /api/config/identity (whole list)", "loading; ready; empty; adding; removing; save failed; read failed", ["g11-identity-notifications", "g11-states"], kind=D,
  note="A failed read renders an empty table and the next edit replaces the stored list; drawn with editing turned off and a confirmation on Remove. Reading and replacing the mappings is not limited to administrators by the server (issue #268)")
R("G11.2", "/config", "Notifications: Microsoft Teams", "pages/Config.tsx", "function NotificationsConfig()", "“Notifications — Microsoft Teams”: Setup Instructions; Teams Webhook URL; Save; Test", "Reading: any signed-in account; the server checks only for a session (issue #268). Saving and testing: administrator, checked by the server", "GET and POST /api/config/notifications; POST /api/config/notifications/test",
  "loading; set; not set; saving; saved; save failed; test sent; test failed; read failed", ["g11-identity-notifications", "g11-states", "s06-credentials-identity"], kind=D,
  note="A failed read followed by Save clears the webhook. Drawn as Set with Replace and Remove, and with editing off after a failed read. Hiding the address on read is row G11.3")
R("G11.3", "/config", "Webhook address returned on read", "pages/Config.tsx", "function NotificationsConfig()", "The Teams webhook URL is shown in full in the field", SES_CFG,
  "GET /api/config/notifications", "set; not set", ["g11-identity-notifications", "s06-credentials-identity"], status="explicit gap", kind=N,
  note="The read returns the address in full to any signed-in account (issue #268). Showing only Set or Not set needs the server to stop returning it, which is its own change and not part of that issue's suggested direction")

# ---- G12
W = "pages/SetupWizard.tsx"
R("G12.0", "/setup", "Step indicator and navigation", W, "function StepIndicator", "Eight steps; Back, Next, “Skip to Login” / “Exit to Dashboard”", SES_SETUP, "GET /api/setup/config; GET /api/setup/import/status", "step n of 8; earlier steps reachable; jumps to Import when one is running",
  ["g12-setup-1", "g12-setup-8"], kind=D,
  note="Setup is not limited to administrators by the page or the server (issue #268)")
for i, (fn, name, what, extra) in enumerate([
        ("function WelcomeStep()", "Welcome", "Three introduction cards", "none"),
        ("function SvnStep(", "SVN Repository", "Repository URL, Username, Password; Repository Layout (standard or custom); Trunk, Branches and Tags paths; Test Connection", "custom layout enables the Branches and Tags paths"),
        ("function GitStep(", "Git Provider", "Provider (GitHub / GitHub Enterprise, Gitea); API URL, Repository, Default Branch, Personal Access Token; Test Connection", "none"),
        ("function SyncStep(", "Sync Settings", "Sync Mode; Auto Merge; Sync Tags; LFS Threshold, Max File Size, LFS Patterns, Ignore Patterns; pull-request title prefix, labels, reviewers, auto-merge", "pull-request fields are collected and not sent"),
        ("function IdentityStep(", "Identity Mapping", "Fallback Email Domain; Author Mapping File; inline mappings add and remove", "none"),
        ("function ServerAuthStep(", "Server & Auth", "Listen Address, Authentication Mode, Admin Password Env Var, OAuth fields; Poll Interval, Log Level, Data Directory; Slack and email notification fields", "authentication mode, OAuth and notification fields are collected and not sent"),
        ("function ReviewStep(", "Review", "Summary cards; “Save Configuration to Server”; warnings; error", "none"),
        ("function ImportStep()", "Import", "Phase graphic; “Start Full Import”; progress; counters; “Cancel Import”; Operation Log; complete, failed and cancelled panels", "polling and cancel failures are not shown today (RS-19)")]):
    R(f"G12.{i + 1}", "/setup", f"Step {i + 1}: {name}", W, fn, what, SES_SETUP, "POST /api/setup/apply (Review); POST /api/setup/import, /import/cancel (Import); POST /api/setup/test-svn, /test-git",
      "ready; validation errors where the step has required fields" + ("; saved, saved with warnings, save failed" if i == 6 else "") + ("; idle, running, stop requested, stopped, failed, complete, verification differs, stale" if i == 7 else ""),
      [f"g12-setup-{i + 1}", "g12-setup-states"], kind=D,
      note="Setup is not limited to administrators by the page or the server (issue #268)" + ("" if extra == "none" else ". Also: " + extra))
R("G12.9", "/setup", "Validation", W, "function validateStep", "Per-step field errors", SES_SETUP, "client only", "URL scheme, owner/repo format, host:port, required fields", ["g12-setup-states"], kind=D,
  note="Three checks are attached to the wrong step today; the design validates each field on its own step. Setup is not limited to administrators by the page or the server (issue #268)")
R("G12.10", "/setup", "Loading the saved configuration", W, "const cfg: WizardSetupConfig = await api.getSetupConfig();", "No visible state; defaults on failure", SES_SETUP, "GET /api/setup/config", "loaded; read failed", ["g12-setup-states"], kind=D,
  note="A failed read shows defaults and offers Save; drawn with the form turned off. Setup is not limited to administrators by the page or the server (issue #268)")

# ---- G13
R("G13.0", "/docs", "Section navigation", "pages/Documentation.tsx", "export default function Documentation()", "Left list of ten sections; no link to this route exists in the shell", ANY, "none (static)", "ready; narrow (list scrolls sideways)",
  ["g13-docs-overview"], kind=P, note="Themed with the page tokens; today it is a white panel with dark-shell sidebar text")
for slug, title, anchor in [("overview", "Overview", "function SectionOverview()"), ("getting-started", "Getting Started", "function SectionGettingStarted()"), ("dashboard", "Dashboard", "function SectionDashboard()"),
                            ("repositories", "Managing Repositories", "function SectionRepositories()"), ("identity-mapping", "Identity Mapping", "function SectionIdentityMapping()"),
                            ("conflict-resolution", "Conflict Resolution", "function SectionConflictResolution()"), ("configuration", "Configuration", "function SectionConfiguration()"),
                            ("personal-mode", "Personal Branch Mode", "function SectionPersonalMode()"), ("deployment", "Deployment", "function SectionDeployment()"), ("troubleshooting", "Troubleshooting", "function SectionTroubleshooting()")]:
    R(f"G13.{slug}", "/docs", f"Section: {title}", "pages/Documentation.tsx", anchor, f"“{title}”: headings, nested topics, code blocks, links, placeholder figures", ANY, "none (static)", "ready",
      [f"g13-docs-{slug}"], note="Section text is abbreviated in the prototype; layout, navigation and content types are what is designed")

# ---- non-route surfaces
L = "components/Layout.tsx"
R("S01", "shell", "Primary navigation and groups", "layouts/layoutCatalog.ts", "export const PRIMARY_NAVIGATION", "Dashboard, Repositories, Conflicts, Audit Log, Settings, Users, LDAP, Config, Setup; groups Workspace, Account, Administration", ANY,
  "none (links)", "active link; grouped or inline", ["s01-shell-roles", "g02-dashboard"], kind=P, note="Per-layout labels for the new options; Classic, Individual and Team keep theirs")
R("S02", "shell", "Role visibility", L, "PRIMARY_NAVIGATION.filter(item => !item.adminOnly || isAdmin)", "Administration entries hidden for non-administrators", "By stored role: the page hides the entries", "none", "administrator; non-administrator",
  ["s01-shell-roles", "s01-shell-member"])
R("S03", "shell", "Status in the shell", "components/SyncStatus.tsx", "export default function SyncStatus", "Dot and “<health label>. <availability>.”", ANY, "GET /api/status (15 s)", "idle; needs attention; stale; unavailable; loading",
  ["s01-shell-roles"], kind=P,
  note="A mark and the health word replace the colour dot, in every layout (display only). The shell shows what the aggregate read returns, which is the worst health. It shows no counts and makes no request of its own")
R("S04", "shell", "Conflict count", L, "item.conflictCount && activeConflicts > 0", "Count badge on Conflicts", ANY, "GET /api/status", "zero (hidden); non-zero", ["s01-shell-roles", "g05-conflicts"])
R("S05", "shell", "Account name and sign-out", L, "export default function Layout()", "Display name or username; “Logout”", ANY, "POST /api/auth/logout (local sign-out is immediate)", "signed in; signing out", ["s01-shell-roles"],
  note="The label differs in the plain vocabulary (E3); Classic, Individual and Team keep it")
R("S06", "shell", "Layout chooser", "layouts/LayoutChooser.tsx", "export default function LayoutChooser", "Select labelled “Layout”: Classic, Individual, Team", ANY, "none (browser storage per account and origin)", "chosen; unknown value falls back to Classic",
  ["s02-chooser"], kind=P, note="Three options added to the list; existing entries unchanged")
R("S07", "shell", "Preference notices", "layouts/layoutPreference.ts", "export function readNotice", "“Using Classic”; “Not saved” with a reason", ANY, "none", "saved; unusable; unreadable; not saved (five reasons)", ["s02-chooser"])
R("S08", "shell", "Theme choice", "layouts/layoutCatalog.ts", "export const THEME_IDS", "none: only `dark` exists", ANY, "none (browser storage per account and site)",
  "dark (default); light (chosen); saved; not saved", ["s02-chooser"], status="explicit gap", kind=P,
  note="Decided 2026-11-16: dark until chosen, light offered in all six layouts, the device setting is not followed. Needs the preference record change (E2)")
R("S09", "shell", "Help access", "App.tsx", '<Route path="docs"', "Route exists; nothing links to it", ANY, "none", "linked", ["s01-shell-roles", "g13-docs-overview"], status="explicit gap", kind=P,
  note="Decided: a Help entry in Ledger, Console and Guide only (E4). Classic, Individual and Team keep their bar exactly as it is")
R("S10", "shell", "“More” control for account and administration links", L, "export function LayoutFrame(", "none: links scroll sideways (Individual, Team) or overflow (Classic)", ANY, "none",
  "closed; open; focus moves into the opened links; Escape closes and returns focus", ["s03-dialogs-transient", "s01-shell-roles"], status="explicit gap", kind=P,
  note="Decided: one control after the last workspace link, present in every layout and hidden where unused (E1). Tab order equals reading order at every width")
R("S11", "dialogs", "Dialog primitive", "components/Dialog.tsx", "export default function Dialog(", "Native modal: title, description, busy line", ANY, "none", "open; busy (Escape refused); closed with focus returned", ["s03-dialogs-transient", "g04-remove-root"])
R("S12", "dialogs", "Confirmation dialog", "components/ConfirmModal.tsx", "export function ConfirmModal(", "Cancel and a named action; Cancel focused first", ANY, "none", "default; danger; busy; error", ["s03-dialogs-transient", "g09-user-dialogs"])
R("S13", "transient", "Status and alert messages", "components/BranchPairRemovalNotice.tsx", "export default function BranchPairRemovalNotice", "Inline banners with role status or alert", ANY, "none", "status; alert", ["s03-dialogs-transient"])
R("S14", "transient", "Toasts", "main.tsx", "ReactDOM.createRoot", "none", ANY, "none", "none", [], status="not applicable", note="The product has no toast component; outcomes are inline. The design adds none")
R("S15", "transient", "Disclosures", "pages/AuditLog.tsx", "aria-expanded={expanded}", "“Show/Hide audit details”, “Show group”, expandable sync records, Setup Instructions, server panel", ANY, "none", "collapsed; expanded", ["s03-dialogs-transient", "g07-audit-table"])
R("S16", "transient", "Tooltips", "pages/RepoDetail.tsx", "title=\"Quick sync on/off via registration update", "Explanations in `title` attributes", ANY, "none", "none", ["s03-dialogs-transient"], kind=P, note="Explanations move into visible text; no tooltip component is added")
R("S17", "transient", "Table controls", "listPagination.ts", "export function changeListPage", "Previous, Next, Return to first page; URL-backed filters", ANY, "none", "first page; middle; last; out of range", ["s03-dialogs-transient", "g05-conflicts-states"])
R("S18", "saves", "Recoverable repository saves", "repositorySave.ts", "export async function runRepositorySave", "Stage words: pending, saved, unknown, failed, conflict", SAVE_FORM, "Request-id receipts", "unchanged; edited; invalid; saving; saved; conflict; unknown; stage two unconfirmed; session changed; navigated away; retry refused",
  ["s04-recoverable-saves", "g03-save-recovery", "g04-settings-edit"])
R("S19", "saves", "Session ended during work", "session.ts", "export class RetiredSessionError", "“The login session changed. Earlier requests cannot update this session; a submitted mutation may still need reconciliation.”", ANY, "none", "session ended; signed in as someone else", ["s07-exceptional", "g03-save-recovery"])
R("S20", "operations", "Operation lifecycle words", "statusObservation.ts", "const states:Record<string,readonly string[]>", "Raw `kind — state` lines", ANY, "GET /api/status", "every lifecycle value per kind; unknown value", ["s05-operations", "g04-operations"], kind=P,
  note="Plain words for each value; the raw value stays in details")
R("S21", "operations", "History hold", "components/StatusObservation.tsx", "operation.history_evidence", "Checkpoint and tip values in the evidence text", "Administrator sees details; others see held without details", "GET /api/status", "held; details restricted", ["s05-operations"], kind=H,
  note="No release route exists; none is drawn")
R("S22", "operations", "Wizard import phase graphic", "components/ImportPhaseGraphic.tsx", "label: 'Import & Push'", "Connect, Import & Push, Verify, Final Push, Complete", SES_SETUP, "none", "each phase; failed; cancelled", ["g12-setup-8"], kind=D,
  note="Setup is not limited to administrators by the page or the server (issue #268)")
R("S23", "credentials", "Secret presentation", "pages/AdminLdap.tsx", "Leave blank to keep current", "Placeholder strings: “●●●●●●● (saved)”, “Leave blank to keep current”, “Current password is set”", ANY, "`*_set` flags on reads", "set; not set; replacing", ["s06-credentials-identity"], kind=P,
  note="State as a word and a mark; never a placeholder")
R("S24", "credentials", "Credential provenance", "crates/core/src/db/queries.rs", "pub fn insert_user_credential", "none", ANY, "none exposed", "own; inherited; server-wide; revoked; unreadable", ["s06-credentials-identity", "g04-settings"], status="explicit gap", kind=N,
  note="Source does not expose origin to the interface; documented as a gap, not invented")
R("S25", "identity", "Roles and the directory-controlled profile", "utils/auth.ts", "export function getStoredUser()", "Role text; disabled fields for the role `ldap`", ANY, "GET /api/auth/me", "administrator; user; role `ldap` (not assigned by the server today); legacy single-administrator sign-in", ["s06-credentials-identity", "g08-account-states"], kind=D,
  note="Local and directory accounts cannot be told apart from the data the interface has (row S34)")
R("S26", "exceptional", "Permission denial", "session.ts", "admin access required", "Role denial kept distinct from session expiry", "Non-administrator", "Any administrator route", "denied without sign-out", ["s07-exceptional", "g09-users-states"])
R("S27", "exceptional", "Signed-out redirect", "App.tsx", "function ProtectedRoute", "Redirect to /login", PUB, "none", "redirect", ["g01-login"])
R("S28", "exceptional", "Unknown address", "App.tsx", "<Routes>", "none: no catch-all route", ANY, "none", "not found", ["s07-exceptional"], status="explicit gap", kind=N, note="A catch-all route is a route change and is outside this design; drawn only to show the intended look")
R("S29", "exceptional", "Malformed or missing read data", "statusObservation.ts", "export function decodeSyncStatus", "“Malformed status response; health is unknown.”; “Malformed conflict page; matching results are unknown.”; “Not recorded”; “Unavailable”", ANY, "strict decode of reads",
  "malformed; missing value named", ["s07-exceptional", "g04-states"])
R("S30", "exceptional", "Unsupported or held capability", RD, "Reset & Reimport", "“Unavailable during safe import cancellation.”", ADM, "none offered", "unavailable", ["s07-exceptional", "g04-settings"], kind=H)
R("G04.31", "/repos/:id", "Section in the address", RD, "function RepositoryDetailEditor()", "none: the parts of the page have no address", ANY, "`?section=` on the existing route; no new route",
  "overview (default); activity; branches; operations; settings; an unknown value falls back to overview", ["g04-overview", "g04-operations"], kind=P,
  note="Decided: links such as “Open operation” land on the right section and Back works. In Classic, Individual and Team the same parameter scrolls to that part of the page")
R("G04.32", "/repos/:id", "Status line on every section", RD, "<StatusObservationPanel view={statusView}/>", "none: the status panel sits in the middle of the page", ANY,
  "none of its own: derived from GET /api/status?repo_id=", "observed; stale; unavailable; held", ["g04-overview", "g04-settings"], kind=P,
  note="New and read-only: state, observed time, last sync, position and a link to the evidence. The status panel itself, with the acknowledgement, stays where it is in the page and shows on Overview (decision D12)")
R("S31", "shell", "Wording per layout family", "layouts/layoutCatalog.ts", "export const PRIMARY_NAVIGATION", "One set of labels, written inside each page", ANY, "none",
  "existing wording (Classic, Individual, Team); plain wording (Ledger, Console, Guide)", ["s01-shell-roles", "g04-settings"], kind=P,
  note="Decided: one text catalogue with two vocabularies (E3). The existing three layouts keep every string exactly, apart from the three corrections in decision D11; the crosswalk lists the pairs")
R("S32", "shell", "Light theme for Classic, Individual and Team", "layouts/layoutTokens.css", ".rs-layout-shell[data-rs-theme='dark']", "none: dark only", ANY, "none", "dark (default); light (chosen)",
  ["s08-existing-layouts-light"], status="explicit gap", kind=P,
  note="Colours, the colour bridge and a comparison panel are specified. Whole screens in the existing three layouts are not drawn in light")
R("S33", "/setup, /config", "Administrator pages with no role check", "layouts/layoutCatalog.ts", "adminOnly", "Server setup and Identity & notifications are hidden from non-administrators in navigation; the pages have no role check of their own",
  SES_CFG, "the setup and config routes listed in issue #268", "administrator; any other signed-in account", ["s01-shell-roles", "s01-shell-member"], status="explicit gap", kind=N,
  note="Source-discovered. Issue #268 records it: reading, and replacing the identity mappings, need only a session; saving and testing the Teams webhook are checked for the administrator role. "
  "No refusal is drawn for these two pages as today's behaviour; a page-level refusal is a proposal that depends on that issue")
R("S34", "/users, /settings", "Where an account signs in", "pages/AdminUsers.tsx", "queryKey: ['admin-users']", "none: the user record carries no sign-in source", ADM_PAGE, "GET /api/users returns id, username, display name, email, role, enabled, created",
  "not reported", ["g09-users", "s06-credentials-identity"], status="explicit gap", kind=N,
  note="Source-discovered. Local and directory accounts cannot be told apart, so no Sign-in column or badge is drawn")
