# Interaction contract

How controls behave in the three new options. Almost all of it is how the product behaves today, written down so an option cannot quietly change it. Where a row describes something the product does not do yet, it says so and the matrix row is named.

## 1. Rules that hold everywhere

1. **A layout or theme never changes what happens.** Same calls, same order, same confirmations, same refusals, same wording for safety statements, in all six layouts.
2. **Recorded is not the same as done.** A status read reports stored state at a time. The page says when it was observed and what it does not prove. Nothing is shown as synchronized, published or verified unless the server said so.
3. **Stopping is not undoing.** Stop and pause controls say what they leave in place.
4. **Unknown blocks.** A missing, malformed, stale or unrecognised value is shown as such and turns off the actions that depend on it. It is never replaced by a default or shown as empty.
5. **One confirmation per consequential action**, naming the target and the consequence. Routine saves have none.
6. **Failures keep the person's entries** and say what was and was not saved.

## 2. Kinds of action

| Kind | Examples | Control | Before it acts | While it runs | Afterwards |
|---|---|---|---|---|---|
| Go somewhere | navigation, “Open pair”, “Open conflict” | link | nothing | nothing | the page |
| Read again | “Retry read”, “Refresh exact-target review” | default button | nothing | earlier data stays, marked stale | a new observed time |
| Check | “Test connection”, “Preview pair”, “Preview update from parent”, “Verify remote” | default button | nothing; the heading says no repository changes | in place, beside the control | result as a mark, a word and a time |
| Save a setting | profile, repository settings, directory settings | primary button | nothing | button busy, form locked | saved, with a receipt where one exists |
| Ask for work | “Request sync”, “Start full history import”, “Publish reviewed result”, “Resume sync…” | primary button | import states its mode; resume asks once | an operation card | the operation and its state |
| Pause or stop work | “Pause sync…”, “Stop import…”, “Stop operation…”, “Skip selected” | warning-style button | a dialog saying what stays and what is not undone | “Stop requested”, then the settled state | what remains and what can be done next |
| Remove, can be restored | “Remove from RepoSync…”, “Remove pair…” | destructive outline, then a filled button inside the dialog | exact-target review | pending, cannot be dismissed | receipt; “Restore registration (stays disabled)” |
| Delete, cannot be restored | remote branch deletion, “Delete…” user, “Delete…” credential, “Remove…” mapping | destructive outline, then filled | dialog; a typed name for remote branch deletion | pending | receipt or failure |
| Held or unavailable | re-anchor, reset and reimport, refresh execution | no control that acts; the reason in text | not applicable | not applicable | not applicable |

A label that ends in “…” opens a dialog and does nothing by itself. A filled destructive button appears only inside a dialog.

## 3. Flows and their states

### Repository save (rows G03.2, G03.5, G04.2 to G04.5, S18)

Existing behaviour from #115. Two stages in order: configuration, then credentials, tied by one request id.

| State | Shown | Available |
|---|---|---|
| Unchanged | nothing to save | Save is unavailable |
| Edited | “Unsaved changes”; the draft is kept in this browser without secrets | Save, Discard |
| Invalid | the error beside the field | fix the field |
| Saving | “Saving in order: configuration, then credentials.” | wait; closing keeps the recovery record |
| Saved | a server receipt for each stage | the form closes |
| Changed on the server | “This save conflicts with current settings.” | “Load current settings for review”, then a deliberate new save |
| Result unknown | “The save response timed out; its server outcome is unknown.” | resume the same request |
| Second stage not confirmed | configuration saved, credentials not confirmed | re-enter secrets or check the earlier request |
| Session changed | “The login session changed. The save is retained for its original user.” | sign in as that user and reopen the form |
| Retry refused | “No write will be blindly retried.” | review the existing repository first |

Other forms (users, directory, identity mappings, notifications, profile) save in one step and show saved or failed.

### Import (rows G02.2, G04.12 to G04.14, G12.8, S20, S22)

| State | Shown | Available |
|---|---|---|
| Never run | the chosen mode | “Start full history import” or “Start snapshot import” (administrator) |
| Queued, running | measured progress, or “in progress” when the server gives no measure | “Stop import…”, shown to administrators |
| Stop requested | “Stop requested”; published commits are not undone | wait |
| Stopped | how far local history and the remote each reached | “Verify remote” (administrator) |
| Needs reconciliation | held; what differs | “Verify remote” |
| Failed | the server's message | “Verify remote”; start again when allowed |
| Completed | counts and the verification result | none |
| Verification differs | what differs | none from here |
| Status stale or unavailable | last known state with its time | “Retry read” |

Resuming a verified partial import has a server route and no control. It is drawn with a dashed marker as an owner decision (G04.14).

### Paused after error (rows G04.9, G04.10)

Shown only while the scoped status is fresh and reports the pause. “Skip commits…” opens the review of observed pending commits; the request carries the observed tips, and the server refuses if they moved. “Resume sync…” clears the pause. Neither proves the two sides match and neither releases a hold. The confirmation, pending and failure states for resume are a shared change, not part of a layout.

### Add a branch pair (row G04.21)

Preview first; nothing changes. “Publish reviewed result” sends the reviewed pin. States: validation error, preview, refused, blocked by conflicts, published and in progress, held, stopped, completed. “Inspect operation”, “Reconcile / resume” and “Stop operation…” act on the operation the same administrator started.

### Pause sync (row G04.23)

The dialog says no new sync work starts and that registration, mappings, secrets, local files and both remotes stay. Turning sync back on is one click and shows its failure.

### Remove from RepoSync, and remove a branch pair (rows G04.24 to G04.26)

- The dialog shows an exact-target review. Without a current review, preview and dependency evidence, the confirm button is unavailable.
- A repository with branch pairs cannot be removed until they are removed or re-homed.
- For a branch pair, both remote-deletion boxes start empty and a typed name is required. The confirm label says “Remove pair” and gains “and delete Git branch” or “and delete SVN branch” only when a box is ticked.
- An earlier approved request with no retained outcome must be recovered before different work is approved.
- Afterwards: receipt, cleanup result, “Retry owned local cleanup”, “Continue original reviewed operation”, “Restore registration (stays disabled)”.

### Conflict decision (rows G06.1 to G06.7)

Choose Accept SVN, Accept Git or the manual edit, then “Record decision…”. The confirmation states that this marks the conflict resolved in RepoSync and does not change either repository. That is what the server does today; applying content is issue #73. “Defer” records a deferral. States for requested, applying, held and verified are reserved and not claimed.

### Error acknowledgement (rows G02.5, G04.30)

Administrator only. The prompt wording is today's. Acknowledging marks errors as seen; it does not fix them or remove audit entries. Unavailable when there is nothing to acknowledge or no current observation.

### Secrets (rows G04.4, G08.4, G10.2, G11.2, S23)

A saved secret is shown as the word “Set”, never as dots in a field and never as its value. “Replace…” opens an empty field; leaving it empty keeps the saved value. “Delete…” asks once. Where a secret comes from (this repository, inherited, server-wide) is not reported by the server today and is shown as not reported (S24).

### Sign-in and session (rows G01.1 to G01.5, S19, S26, S27)

A wrong password keeps the username and clears the password. A session that ends sends the person to sign-in; work that was mid-save keeps its recovery record for the original user. Being refused for lacking the administrator role is shown in place and does not sign the person out.

### Layout and theme choice (rows S06 to S08, S31, S32)

Choosing a layout or theme applies at once, sends nothing to the server and does not navigate. The page keeps its state, drafts, focus, scroll position and pending requests, as #262 guarantees for layouts. With nothing saved the layout is Classic and the theme is dark. Light is offered in all six layouts. The notices “Using Classic” and “Not saved” and their reasons are #262's text, unchanged. The sign-in page is not affected by either choice.

### Repository sections (row G04.31)

Overview, Activity, Branch pairs, Operations and Settings are views of one page. The address carries `?section=`; an unknown value shows Overview. Choosing a section does not unmount the others: a draft in Settings, an open dialog and a running request survive a visit to another section and a change of layout. Leaving a section with unsaved changes needs no prompt for that reason. Back returns to the previous section.

### The “More” control (row S10)

It follows the last workspace link. Opening it reveals account links, administration links, the signed-in name, the chooser and sign-out, and moves focus to the first of them. Escape closes it and returns focus. Choosing a link closes it.

## 4. Validation

Validation is what the product does today. Fields are checked in the browser when a form is submitted and, where the server validates, again on save. The error is text beside the field and tied to it, using the label and error association added under #121. No option adds, removes or reorders a check. The setup wizard attaches three checks to the wrong step today (row G12.9); correcting that is a shared change.

## 5. Permissions

- This contract describes what the page draws. The stored role decides which controls are drawn, and the design does not change which ones are. What the server enforces is outside this contract, except where issue #268 is cited.
- Administrators see the Administration group, Add repository, the import controls, error acknowledgement, the paused-after-error banner, Add branch pair and the actions on a pair's row, and the Stop or remove section. Users and Directory (LDAP) show other accounts a refusal. Everyone signed in sees the workspace pages, their own account and Help.
- The repository header shows Edit, the sync switch and the sync request to every account today, and the Settings section and the edit view are shown the same way (rows G04.1 to G04.6). The design keeps that and adds no role condition.
- A role cell in the matrix says what the page itself shows to whom. Where the page has no role condition, the cell says so.
- A control that is unavailable is drawn disabled with its reason in text beside it.
- The import controls are shown to administrators.
- Server setup and Identity & notifications are hidden from non-administrators in navigation, and the pages have no role check of their own. Issue #268 records which of their calls need only a session. On Identity & notifications those are reading, and replacing the identity mappings; saving and testing the Teams webhook are checked for the administrator role. The design draws no refusal on those two pages as today's behaviour.
- Administrators see hold details; others see that work is held, without details.

## 6. Receipts and recovery

A receipt is something the server returned and the page kept: a save request id with its stage results, a removal receipt, an operation id. Receipts are shown where the work was started and again on the repository list when the repository itself is gone. A lost reply is never treated as a failure or as a success: the page offers to look the request up.

## 7. Held and unsupported

| Capability | What the options show | Owner |
|---|---|---|
| Update pair from parent, beyond preview | preview only | #69 |
| Re-anchor a pair | a held notice; no control that acts | #69 |
| Reset and reimport | “Unavailable during safe import cancellation.” | existing statement |
| Releasing a history hold | the hold and its evidence; no release control | no route exists |
| Applying a conflict decision to the repositories | a notice that the decision is recorded only | #73 |
| Deleting one side of a pair and archiving the other | not drawn | #199 |

## 8. What changing layout or theme must preserve

The routed page stays mounted. Its state, unsaved drafts, open dialog, focused element, selection, scroll position, pending requests and polling continue. The address does not change. No request is sent. This is the #262 guarantee, and #264 lists it as an acceptance item; every arrangement in this design has to be reachable without remounting page content (see the implementation map, section 5).
