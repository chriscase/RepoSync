"""Documents added in v1.1: the decisions record, the change list, the colour bridge and the page regions."""
import csv
import io
import json

import bridge
import kit
import resolve as Z
import tokens

SETS = [("existing layouts", "light"), ("ledger", "light"), ("ledger", "dark"), ("console", "light"), ("console", "dark"), ("guide", "light"), ("guide", "dark")]
WITH_LAYOUTS = ["G01.2", "G02.5", "G02.7", "G02.8", "G02.11", "G03.3", "G04.32", "S03", "G04.1", "G04.10", "G04.15", "G04.25", "G06.1", "G06.2", "G06.7", "G07.1", "G07.3", "G12.4", "G12.6", "S16", "S20", "S23"]
OWN_CHANGE = ["G04.7", "G04.9", "G04.13", "G04.23", "G04.31", "G06.4", "G08.4", "G11.1", "G11.2", "G12.9", "G12.10"]
WAITS = ["G12.8"]


def decisions_md():
    return f"""# Decisions

Recorded 2026-11-16. Chris answered the nine questions that v1 left open. This file gives his words, how they are read, and what the designer decided where he delegated. The contracts in this version are written to these decisions.

**These are decisions about the design. Nothing is implemented.** #263 cannot close #264.

## Chris's answers

| # | Question in v1 | Answer, in his words | How it is read |
|---|---|---|---|
| 1 | Which options go to #264 | “all 3” | Ledger, Console and Guide are all accepted for #264. |
| 2 | Five changes to the #262 shell | “i'm not sure what you're asking” | Explained in plain words and decided under the delegation in answer 6. See D1 to D5. |
| 3 | A light theme for Classic, Individual and Team, or dark only | “dark until chosen” | Those three stay dark until a person chooses light. So light is offered in them too. |
| 4 | Theme when nothing is saved | “default to dark” | Dark, in every layout. The device setting is not followed. |
| 5 | How far #264 goes | “go FAR, make it extensive, be bold and corageous” | #264 targets everything the prototypes show: shell, both themes, page arrangement, wording and the shared changes. No reduced first version. |
| 6 | The shared changes the prototypes draw | “use your best judgement for this and the rest” | Decided by the designer: D6. The same delegation covers questions 7, 8 and 9 and the unanswered question 2. |

One reading is the designer's and could be wrong. Answer 3 is taken to mean that light becomes available in Classic, Individual and Team. If the intent was that those three stay dark only, drop row S32 and the section “Existing layouts in light” of the design-system contract; nothing else depends on it.

## Decided by the designer

| Id | Decision | Why |
|---|---|---|
| D1 | **A “More” control** is added to the shell (E1). It follows the last workspace link, exists in every layout and is hidden where a layout does not use it. Opening it moves focus to the first link it reveals; Escape closes it and returns focus. | Account and administration links must be reachable on a phone. Placed there, tab order equals reading order at every width in all three options, which v1 left unsolved. |
| D2 | **The theme is saved with the layout** (E2): per account and site, dark when absent. An existing saved layout keeps working. | Answers 3 and 4. |
| D3 | **Wording comes from one text catalogue with two vocabularies** (E3). Classic, Individual and Team use the existing vocabulary and keep every string exactly, apart from the three corrections in D11. Ledger, Console and Guide use the plain vocabulary in the crosswalk. | The new options need accurate labels inside pages, not only in navigation. A catalogue changes text without forking behaviour, leaves the acceptance lab's locators for the existing layouts untouched, and takes text out of page components. |
| D4 | **A Help link** to the existing documentation route (E4), in Ledger, Console and Guide only. | The route exists and nothing links to it. Classic's bar already wraps at common widths, so it is left alone. |
| D5 | **The sign-in page keeps one appearance: dark, as today.** E5 is withdrawn. | The saved choice belongs to an account, and nobody is signed in there. Remembering the last person's choice in the browser would leak it to the next person, which #262 and #264 both rule out. |
| D6 | **Every shared change the prototypes draw is adopted**, in three groups (below). They apply in every layout, because behaviour must not differ between layouts. | Answer 5. Each one makes the interface say what is true or stops it destroying data after a failed read. |
| D7 | **Repository sections are addressable** with `?section=` on the existing route. Unknown values fall back to Overview. In the existing three layouts the same parameter scrolls to that part of the page. | A link such as “Open operation” has to land on the operation, and Back has to work. It is a parameter, not a new route. |
| D8 | **Page arrangement uses one fixed tree of regions per page**, the same in every layout and in today's order (D12). See [contracts/page-regions.md](contracts/page-regions.md). | #264 requires that switching layout keeps drafts, focus and pending requests. That only holds if a layout rearranges regions and never renders a different tree. |
| D9 | **Held and missing items stay as they are**: pair refresh beyond preview and re-anchor (#69), applying conflict decisions (#73), credential origin (#63 RS-11), resuming a stopped import, deleting one side of a pair (#199). | They have owners or holds. The design shows them as held or missing and adds nothing. |

### D6 in detail: the shared changes

| Group | Rows | What it means for #264 |
|---|---|---|
| Built with the layouts | {", ".join(WITH_LAYOUTS)} | Display only: no call is added, changed or made at a different time, and no step is added. Failed reads shown as failed, states as a mark and a word, secrets as “Set”, explanations as text, the confirm label saying what will happen. |
| Each its own small change, with its own tests | {", ".join(OWN_CHANGE)} | It changes when a call is made, adds a confirmation, or stops a write after a failed read. Same behaviour in every layout. Each gets its own review so a regression can be traced to it. |
| Waits for its owner | {", ".join(WAITS)} | The setup wizard's import step belongs to #64 (RS-19). The options style what exists until that owner changes it. |

Two limits on D6:

- **Labels in Classic, Individual and Team do not change**, apart from three that state something the control does not do. Those are corrected in every layout (D11, below).
- **“Request sync” depends on one fact.** At the revisions read, the call behind “Trigger Sync” records a request and nothing in the tree was found to act on it. The sync owner should confirm that before the label ships.

## Second round, the same day

After v1.1 Chris was asked three things: whether light should really reach Classic, Individual and Team; whether three misleading labels should be corrected in those layouts too; and whether pages should take a new order of regions. His answer was “Use your best judgement”. So:

| Id | Decision | Why |
|---|---|---|
| D10 | **Light stays offered in Classic, Individual and Team.** The reading of answer 3 stands. | Once the colour bridge exists it costs one palette. Dark remains the default, so nobody sees it without choosing it. |
| D11 | **Three labels are corrected in every layout**, in both vocabularies: “Retry” becomes “Resume sync…”, “Cancel operation” becomes “Stop operation…”, and “Trigger Sync” becomes “Request sync”. Every other string in Classic, Individual and Team stays exactly as it is. | The shared changes already alter these controls everywhere. A button called “Retry” that opens a dialog titled “Resume sync?” would be worse than either word. Each of the three states something the control does not do. |
| D12 | **Pages keep today's order of regions in the document, in every layout.** Ledger, Console and Guide fit that order. Where they need something first, it is a new read-only region: “Needs attention” on the dashboard, a status line on the repository page, a credentials summary in repository settings. | This reverses the recommendation in v1.1. A new document order would have made what is seen and what the keyboard and a screen reader reach differ in Classic, Individual and Team, on two pages, in the default layout. Keeping today's order leaves those layouts as they are and gives the new options one order for eye, keyboard and screen reader. |

Conditions on D11:

- “Request sync” ships only after the sync owner confirms that the call behind “Trigger Sync” records a request and nothing acts on it at once. If it does start a sync, the label stays as it is in both vocabularies.
- The setup wizard's “Cancel Import” is not one of the three. It belongs to #64 (RS-19) and changes with that owner's work.
- The acceptance lab (PR #195) is updated for the three labels in the same change that makes them.

## Third round: the independent review of v1

The publisher had v1 rendered and reviewed independently. The verdict was to accept it with fourteen required changes. All fourteen are made in v1.3; [CHANGES-FROM-v1.md](CHANGES-FROM-v1.md) maps each one. The decisions that came out of it:

| Id | Decision | Why |
|---|---|---|
| D13 | **The markup inside every page is the same in all three options.** Guide uses the same tables and forms as the other two; its cards and the step header on the add form are dropped. The build checks it. | A layout must never render a different tree (D8). v1 broke that on four screens. |
| D14 | **The list of shell changes is E1, E2, E3, E4 and E6.** E3, the text catalogue, covers navigation, the shell and page labels. E6 is a navigation icon per entry. E5 stays withdrawn. | v1 used icons, a renamed sign-out and renamed section labels without listing how they would be delivered. |
| D15 | **In Classic, Individual and Team, the “More” button, the icons, the three derived regions and the four group headings on the directory form are in the tree and hidden. The theme select is visible.** | Hidden elements leave those layouts looking and tabbing as they do today. The theme select is the one visible addition, and it follows from D10. |
| D16 | **Server setup and Identity & notifications are described as they are today**: hidden in navigation, with no role check in the page. Issue #268 records which of their calls need only a session; saving and testing the Teams webhook are checked for the administrator role. No refusal is drawn for the two pages. | Issue #268. v1 claimed a server check for the whole of both pages. |
| D17 | **Safety and truthful-status sentences are the source's own, and identical in both vocabularies.** | v1 reworded four of them while calling them unchanged. |
| D18 | **The shell shows the health word the current read returns.** Counts appear only on the dashboard, from the reads that page already makes for each pair. | The aggregate read returns no counts, and switching layout must issue no request. |
| D19 | **On the conflict page nothing is preselected**, the diff compares SVN with Git as today, and “Record decision…” is unavailable until a choice is made. | A preselected resolution invites a wrong decision. |
| D20 | **The explicit pause is for repositories. A branch pair keeps its one-click switch.** | The explicit call is offered only for repositories today, and the design does not extend it. |

## Fourth round: the follow-up review of v1.3

A second independent reviewer checked v1.3 against the source and the publisher's captures. Most of the first review's findings were confirmed fixed. [CHANGES-FROM-v1.3.md](CHANGES-FROM-v1.3.md) lists what remained and what v1.4 did.

| Id | Decision | Why |
|---|---|---|
| D21 | **The package makes no statement about access control beyond issue #268. It describes a functional refusal only where the page shows its result.** | A design package describes the interface. Access control belongs to the server's own review, and issue #268 is the public record of it. |
| D22 | **On a phone, wide tables stay in scroll frames with a shadow at the hidden edge.** Stacking rows into blocks is left for #264 to decide against the real application. | The publisher's captures show the shadow works. Stacking changes every table in every option at once, and no version of it has been rendered. |
| D23 | **The public repository carries this version, v1.6, with its captures, and nothing earlier.** v1, the 2026-11-12 packet inside it, and v1.1 to v1.5 stay private Artifacts, recorded by checksum. | The owner's decision, passed on by the publisher. It replaces the earlier decision to commit v1 as history. |

## Fifth round: the closing check of v1.4

A third independent reviewer checked v1.4 against the source and the publisher's captures. Every item of the second review was confirmed fixed. [CHANGES-FROM-v1.4.md](CHANGES-FROM-v1.4.md) lists the eight new items and what v1.5 did.

| Id | Decision | Why |
|---|---|---|
| D24 | **A role cell says what the page itself shows to whom.** Where the page has no role condition, the cell says so, and the design adds none. | #264 must not gain a role condition the page does not have today, and a test must not assert one. |
| D25 | **The repository connection tests stay in the edit view**, as today. | A test started from the read view would be a request the page does not make today. |
| D26 | **A result is drawn only if the source can produce it.** The personal credential test is drawn with the error it shows today, and test results use the source's own messages. | #263 asks for the states the source supports. |
| D27 | **The designer checks each version in a sandboxed browser before delivery**: errors, requests outside the package, a page wider than the screen, and selected elements inside the page measured against the box that holds them. The publisher's captures remain the visual record. | Every earlier version that was captured showed a layout fault the designer had not seen. The browser starts with its sandbox when run as a non-root user. |

## Sixth round: the final check of v1.5

A fourth independent reviewer checked v1.5. It asked for one required change, the remainder of the role cells, and listed five small items. [CHANGES-FROM-v1.5.md](CHANGES-FROM-v1.5.md) lists them and what v1.6 did.

| Id | Decision | Why |
|---|---|---|
| D28 | **Who sees a setting is said once, in the matrix.** The crosswalk points to the matrix rows and names no role beside a field. | Two places that state the same thing drift apart: the crosswalk still gave a role the matrix had already corrected. |

## For Chris

1. **A Dark and Light select appears in Classic, Individual and Team** because light is offered there (D10). It adds one control to a bar that #262 records as already wrapping between 1280 and 1490 px. The alternative is to offer light in Ledger, Console and Guide only, which removes the control and row S32.
2. **Issue #268** is yours to schedule. The design does not wait for it. Row S33 stays an explicit gap until it is fixed. Row G11.3, hiding the webhook address on read, needs its own change: the server would have to stop returning the address, which #268's suggested direction does not cover.
3. **Releasing the #69 holds.** Nothing in the design depends on it.
4. **A different page structure for Guide**, such as cards for repositories, is possible later but needs its own regions in the fixed tree. It is left out until the implementation has shown that the fixed tree works.
5. **“Test Connection” on a personal credential cannot work today.** The page calls a route that is not registered (row G08.4). A test that works is feature work with no owner yet (row G08.6).

## Still open, and whose it is

| Item | Owner |
|---|---|
| Whether the foundation changes are technically acceptable as specified, in particular the saved record's new version | the #262 foundation owner |
| Whether the colour bridge or component classes come first, and the order of routes | the implementation owner |
| The setup and config routes | issue #268 |
| Confirming what “Trigger Sync” does | the sync owner |
| A personal credential test that works (row G08.6) | no owner yet |
| Updating the acceptance lab for the three labels and the added confirmation steps | the PR #195 owner |
"""


def changes_md():
    return f"""# What changed

Earlier versions are unchanged and none of them is in the repository. They are private Artifacts: the links open only for their owner, and the SHA-256 of each `SHA256SUMS` is what identifies them.

- v1: SHA-256 `{kit.V1_SUMS}` (private Artifact: {kit.V1_ARTIFACT})
- v1.1: SHA-256 `{kit.V11_SUMS}` (private Artifact: {kit.V11_ARTIFACT})
- v1.2: SHA-256 `{kit.V12_SUMS}` (private Artifact: {kit.V12_ARTIFACT})
- v1.3: SHA-256 `{kit.V13_SUMS}` (private Artifact: {kit.V13_ARTIFACT}). An earlier issue of v1.3, SHA-256 `{kit.V13_WITHDRAWN_SUMS}`, was withdrawn before use.
- v1.4: SHA-256 `{kit.V14_SUMS}` (private Artifact: {kit.V14_ARTIFACT})
- v1.5: SHA-256 `{kit.V15_SUMS}` (private Artifact: {kit.V15_ARTIFACT})

By review: [CHANGES-FROM-v1.5.md](CHANGES-FROM-v1.5.md) answers the final check of v1.5, [CHANGES-FROM-v1.4.md](CHANGES-FROM-v1.4.md) the closing check of v1.4, [CHANGES-FROM-v1.3.md](CHANGES-FROM-v1.3.md) the follow-up review of v1.3, and [CHANGES-FROM-v1.md](CHANGES-FROM-v1.md) the first review of v1.

The sections below were each written for their own version. A statement in them about what had or had not been rendered is true as of that version; the package index says how things stand now.

## From v1.5 to v1.6

This version answers the final check of v1.5.

- **The last role statements.** The Errors card row says the card is shown to everyone and only “Acknowledge” to administrators. The crosswalk no longer names a role beside a setting; it points to the matrix rows (decision D28).
- **One sentence removed.** The places that describe stopping an import say only who the page shows the control to.
- **Specimens.** The credential save failure shows the client's text as it is. The stopped import was stopped by an administrator. The Git token field in the edit view shows whether a token is saved. The test result note says only that a message appears.
- **Layout.** Tables in Ledger's dialogs no longer show a shadow when nothing is hidden. A row of buttons keeps its distance from the field above. A long host name breaks after a full stop, not before its slash.
- **Records.** The render check's scope is stated exactly. The publisher's v1.5 capture run is recorded. The repository carries v1.6 (decision D23).

The matrix still has 140 rows.

## From v1.4 to v1.5

This version answers the closing check of v1.4.

- **Role cells.** Each was checked again against the page's own role conditions. Where the page has none, the cell says so in page terms and the design adds none (decision D24).
- **Connection tests.** The repository tests are in the edit view only, as today. Test results use the source's own messages.
- **Personal credentials.** “Test Connection” is drawn with the error it shows today, because the route it calls is not registered. A test that works is a new feature-work row, G08.6.
- **Truthful status.** The held status line shows the recorded position alone.
- **Wording.** Two sentences about the server are put in page terms, and decision D21 says what the package does state.
- **Layout.** An identifier stays whole while its container can hold it and wraps inside it when it cannot. Three tables that were cut off at 1440 px now fit. A label column is only as wide as its longest label.
- **Records.** The publisher's v1.4 capture run is recorded. The public repository carries only this version and its captures (decision D23), so every path into v1 is gone.
- **Checking.** The designer now runs a sandboxed render check before delivery (decision D27).

The matrix has 140 rows, up from 139.

## From v1.3 to v1.4

This version answers the follow-up review of v1.3.

- **Statements about the server.** The package describes what the page does and leaves server enforcement out, except where issue #268 states it (decision D21).
- **Setup.** The secrets table and two setup specimens are corrected to match the page and the source.
- **Conflict page.** The diff now compares SVN with Git, as the page does today, and the confirmation says “Not chosen”.
- **Truthful status.** A non-administrator's shell shows the same health as an administrator's. The unknown evidence specimen reports the configuration as unknown. The held repository has one set of positions everywhere.
- **Skipping commits.** The dialog opens with nothing selected.
- **No resume control** is listed anywhere, and Stop is described as shown to administrators.
- **Layout.** Review notes wrap, long addresses in documents wrap, a long label no longer squeezes its value, names in tables stay whole, and long paths break after a slash.
- **Records.** Contradictions between documents are removed, and the links to earlier versions are labelled private.

The matrix still has 139 rows.

## From v1.2 to v1.3

This version answers the independent review of v1 (findings F1 to F14 and the optional items).

Corrections of fact:

- Server setup and Identity & notifications are no longer described as checked by the server for the administrator role. The rows link issue #268, and two explicit-gap rows are added.
- The Sign-in column and the Local and Directory account distinction are removed. The server reports no sign-in source.
- Status evidence is written for each health. A held, paused or unknown page no longer shows idle evidence, and the build checks that.
- The shell shows the health word only. v1 showed a count the read does not return.

Wording put back as the source has it: the save-recovery sentences and buttons, “Cancellation after intent retains the hold…”, “Sync configured enabled”, and “Corporate login enabled” on sign-in.

Behaviour kept as today where v1 changed it without saying so: binary conflicts, the second pairing mode, no preselected conflict resolution, the diff comparing SVN with Git, no control for resuming an import, the one-click sync switch on branch pairs.

Structure:

- The markup inside every page is now identical in all three options. Guide's cards and step header are gone. The build checks it.
- The chooser specimens are visible in every option and width.
- Adding `-menu` to the address fragment opens the “More” control for captures.
- The dashboard names the repository in the commit map and the sync records.

Layout faults reported from the captures are corrected in the stylesheets: long buttons wrap, identifiers do not break inside tables, credentials are a list that wraps, State is the second column of the repositories table, Help has spacing and its section links wrap.

The matrix has 139 rows, up from 136.

## From v1.1 to v1.2

Decisions:

- D10 to D12 are added to [DECISIONS.md](DECISIONS.md): light stays offered in the existing three layouts; three misleading labels are corrected in every layout; pages keep today's order of regions.
- D12 reverses a recommendation made in v1.1. That version proposed a new document order for the dashboard and the repository page. It would have changed keyboard and screen-reader order in Classic.

Prototypes:

- **Dashboard.** “Needs attention” is still first, now as a read-only region with the count and the observed time. After it the regions follow today's order: in flight, repositories, sync health with the acknowledgement, recent sync records, commit map, recent activity, server. In v1.1 the health counts and the acknowledgement were in the first block.
- **Repository page.** The status line at the top of every section is read-only and links to the evidence. “What this status proves” and “Errors” are the first parts of Overview, followed by operations, recent sync records, branch pairs and the server monitor.
- **Branch pairs section.** The update preview and the held re-anchor notice come before the table, as in the page today.
- **Console.** From 1101 px the dashboard's left column is “Needs attention” and the right column is everything else in order.

Documents:

- `contracts/page-regions.md` is rewritten for D12.
- The matrix has 136 rows, up from 134: “Needs attention” (G02.11) and the status line (G04.32).
- The implementation map, the crosswalk and the index follow the three decisions.

Not changed from v1.1: the tokens, the shell and the “More” control, the colour bridge, the plain vocabulary, the existing layouts' light palette.

## From v1 to v1.1

- The nine open decisions were settled.
- Dark became the default; the “Device” option was removed.
- The “More” control moved to follow the last workspace link, and opening it moves focus.
- Sign-in was given one appearance, dark, in every option.
- The existing layouts' light palette, the colour bridge and the page regions were added.
- The repository overview kept the server monitor, collapsed.
- 69 screens per option, up from 68.

## Still not done

As of that version: no page had been opened in a browser by the designer, on the belief that no sandboxed browser could start in the container. Two versions had by then changed arrangement without a rendered check. The belief was wrong for a non-root user; see the package index.
"""


def changes_from_v1_md():
    D = "[DECISIONS.md](DECISIONS.md)"
    rows = [
        ("F1", "P1", "Not touched.",
         "The role text on G12.0 to G12.10, S22, G03.3, G11.1 and G11.2 says what the server checks today and links issue #268; those rows are *existing, known defect*. "
         "New explicit-gap rows S33 and G11.3. The crosswalk's scope cells are corrected. “Administrator only” is removed from the two screens, and no refusal is drawn for them. Nothing is said beyond what #268 says.",
         f"Fixed. Recorded as D16 in {D}.", "`matrix/*`, `crosswalk/*`, `contracts/interaction.md` §5, `options/*/screens/g11-*`, `g12-*`, `s01-*`"),
        ("F2", "P1", "v1.2 made the status line at the top read-only and left the evidence in its own region on Overview (D12). That took the idle evidence off the held and paused pages, but no page had evidence for those states.",
         "Evidence is generated for each health from the page's own sentence pattern. The paused page shows paused evidence; Overview shows held, paused and unknown specimens. "
         "The operation on the held page is the held import. `tools/build.py` fails if evidence and badge disagree.",
         "Fixed.", "`tools/screens_a.py` (`evidence`), `tools/build.py`, `options/*/screens/g04-overview`, `g04-skip`, `g04-operations`"),
        ("F3", "P1", "Not touched.",
         "The Sign-in column and the Local and Directory rows are removed. G08.2 and S25 are *existing, known defect*. New explicit-gap row S34.",
         "Fixed.", "`options/*/screens/g09-users`, `s06-credentials-identity`; rows G08.2, G09.1, S25, S34"),
        ("F4", "P2", "v1.2 moved the health counts to the read-only “Needs attention” region and put the acknowledgement back after Repositories. The shell still showed a count.",
         "The shell shows the aggregate health word and nothing else, in the live shell and in its specimens. S03 is a shared change. The dashboard says its counts come from the per-pair reads it already makes and that a failed or stale read counts as Unknown.",
         f"Fixed. Recorded as D18 in {D}.", "`tools/build.py` (`shell`), `options/*/screens/g02-dashboard`, `s01-*`; rows S03, G02.4, G02.11"),
        ("F5", "P2", "Not touched.",
         "Source wording restored on save recovery (sentences and both buttons), pairing and the header chips. The footnote and the crosswalk's “word for word” claim are corrected.",
         f"Fixed. Recorded as D17 in {D}.", "`options/*/screens/g03-save-recovery`, `g04-pairing`, every `g04-*` header; `crosswalk/*`"),
        ("F6", "P2", "Not touched.",
         "Binary conflicts and the second pairing mode are drawn as today. The confirmation on “Stop import…” is a marked proposal and its own change. Interaction contract §5 states what is shown to whom today.",
         "Fixed.", "`options/*/screens/g06-conflict-states`, `g04-pairing`; rows G04.1, G04.13, G04.21, G06.2; `contracts/interaction.md` §5"),
        ("F7", "P2", "v1.1 chose the mechanism: one text catalogue with two vocabularies (D3). v1.2 corrected three labels in every layout (D11). The rows were still tagged layout only.",
         "The catalogue covers navigation, the shell and page labels (E3). The rows are retagged as shell extension E3 and the labels are listed in the crosswalk. Sign-in keeps the existing wording.",
         f"Fixed. Settled by D3, D11 and D14 in {D}.", "rows G04.2, G04.3, G04.5, G04.8, G04.22, G09.1, G10.4, S05, G01.2; `crosswalk/*`; `contracts/implementation-map.md` §4"),
        ("F8", "P2", "Not touched.",
         "Nothing is preselected. “Record decision…” is unavailable until a choice is made. The choices are worded as recorded decisions. The diff compares SVN with Git as today.",
         f"Partly fixed in v1.3: the drawn diff and the confirmation's wording were completed in v1.4. Recorded as D19 in {D}.", "`options/*/screens/g06-conflict-diff`, `g06-conflict-edit`; rows G06.2, G06.4"),
        ("F9", "P2", "Not touched.", "No control is drawn for resuming an import.", "Fixed.", "`options/*/screens/g04-operations`; row G04.14"),
        ("F10", "P2", "Not touched.", "The explicit pause is for repositories; a branch pair keeps its one-click switch, and the explicit call is not extended to pairs.",
         f"Fixed. Recorded as D20 in {D}.", "rows G04.22, G04.23; `options/*/screens/g04-pause`; `matrix/capability-parity.md`"),
        ("F11", "P2", "v1.1 withdrew E5 and moved the “More” control. v1.2 fixed the order of regions as today's, in one tree (D8, D12). Guide still had its own cards and step header, and the effect of E1 and E2 on the existing layouts was not stated.",
         "The markup inside every page is identical in all three options, and the build checks all 69 screens. The effect of E1 and E2 on the existing layouts is stated and the oracle changed. E6 (navigation icons) is added and E3 widened. "
         "Colour that does not come from utility classes is listed in the colour bridge, and the sentence about the diff viewer is corrected.",
         f"Fixed. Settled by D8, D12 and D13 to D15 in {D}.", "`tools/screens_a.py`, `tools/build.py`; `contracts/implementation-map.md` §4, §9; `contracts/colour-bridge.md`; `contracts/design-system.md` §10"),
        ("F12", "P2", "v1.1 moved the “More” control but gave no way to capture it open. The publisher's render of v1.2 opened it by hand and found all three options' menus complete.",
         "The chooser specimens use their own class. `-menu` in the fragment opens the control on load. The dashboard's commit map and sync records name the repository.",
         "Fixed in the files; the fragment is not yet captured.", "`options/*/screens/s02-chooser`, `g02-dashboard`; `prototypes/assets/proto.js`, `base.css`"),
        ("F13", "P3", "Not touched. The publisher's render of v1.2 found the same four over-wide captures, identifiers breaking inside two tables, and a table cut at its frame's edge on a phone with nothing to show that it scrolls.",
         "Buttons wrap. Identifiers inside tables, and short ones anywhere, do not break. A table frame shows a shadow at an edge where more is hidden. Credentials are a list that wraps. State is the second column of the repositories table. "
         "Help has heading spacing, wrapping section links and the placeholder figure.",
         "Fixed in the stylesheets; not rendered.", "`prototypes/assets/base.css`; `options/*/screens/g03-save-recovery`, `g04-settings`, `g02-dashboard`, `g13-*`, `s01-*`"),
        ("F14", "P3", "v1.1 replaced the history copy with a pointer and added the `delivery_for_264` values. v1.2 gave the attention list its own row.",
         "G11.1's role text; G11.2 split, with G11.3 as an explicit gap; S05 a shell extension; the reference statement with the capture naming; breadcrumbs; the setup marker; the selector wording; the tools' paths; the index and cover citing the publisher's captures.",
         "Fixed.", "`matrix/*`; `options/*/screens/g04-*`, `g12-setup-6`; `contracts/design-system.md` §1; `tools/*`; `README.md`"),
        ("O1", "optional", "Not touched.", "The index and the option pages say that no option offers a different page structure. It is true by construction now.", "Taken.", "`README.md`, `options/*/README.md`"),
        ("O2", "optional", "Not touched.", "The chooser page says the existing three stay selectable in the product.", "Taken.", "`options/*/screens/s02-chooser`"),
        ("O3", "optional", "Not touched.", "Ledger dark's masthead rule is `#7d7b76`, down from `#eceae4`.", "Taken.", "`options/ledger/tokens.css`"),
        ("O4", "optional", "Not touched.", "In Guide, “Account and admin” names the current page when it is one of the pages behind it, and is marked as current in every option.", "Taken.", "`tools/build.py` (`shell`), `base.css`"),
        ("O5", "optional", "Not touched.", "The sandbox path is gone from `tools/`. The history stays untouched in v1.", "Taken.", "`tools/resolve.py`"),
    ]
    md = ["# Changes from v1, by finding", "",
          "The independent review of v1 (2026-11-16) asked for fourteen changes and offered five optional ones. For each, this says what v1.1 or v1.2 had already changed, what v1.3 changes, the result as of v1.3, and where. "
          "Nothing is disputed: every source claim in the review was re-checked against both epic tips and found correct.", "",
          "The review asked for a version called v1.1. That number and v1.2 were already used for the versions that applied Chris's decisions, so the answer was v1.3. v1 is unchanged.", "",
          "This file was written for v1.3 and its “As of v1.3” column is kept as it was, with one correction: the follow-up review found F8 only partly fixed. It also found residuals in F2, F6 and F14. "
          "[CHANGES-FROM-v1.3.md](CHANGES-FROM-v1.3.md) records what v1.4 did about them.", "",
          "| Finding | Priority | Already changed by v1.1 or v1.2 | Changed in v1.3 | As of v1.3 | Where |", "|---|---|---|---|---|---|"]
    for r in rows:
        md.append("| " + " | ".join(r) + " |")
    md += ["", "## Changes between v1 and this version that do not come from the review", "",
           "They come from Chris's decisions and are listed in [CHANGES.md](CHANGES.md) and [DECISIONS.md](DECISIONS.md): dark by default, light in all six layouts, the “More” control after the last workspace link, "
           "one appearance for sign-in, the text catalogue, pages keeping today's order of regions, the colour bridge, and `?section=` on the repository page.", "",
           "## Not done, as of v1.3", "",
           "As of v1.3 the designer had rendered nothing, on the belief that no sandboxed browser could start in the container; that was wrong for a non-root user, and the package index describes the check that now runs. "
           "When v1.3 was written the publisher had rendered v1 and v1.2. It has since captured every later version, including the `-menu` fragment under F12 and the layout corrections under F13; the package index records those runs."]
    return "\n".join(md) + "\n"


def changes_from_v13_md():
    rows = [
        ("N1", "Role cells describe what the page does: “Shown to administrators; the page hides it from other accounts”. The sentence about the server is removed from every row that carried it, and from S02, G04.1 and interaction contract §5. "
         "The package makes no statement about what the server enforces outside issue #268 (decision D21).",
         "`matrix/*`; `contracts/interaction.md` §5; `tools/matrix_data.py`"),
        ("N2", "The three items are changed as the review lists them. The setup row of the secrets table names only what the wizard has fields for. One sentence on the sixth setup step is deleted. "
         "The setup warning shown is one the source returns, the Git LFS preflight warning, and the failure reads “API error 500.”. "
         "The tree was then searched for other statements of the same kind. One more was found and removed: the note on empty fields in the same row.",
         "`options/*/screens/s06-credentials-identity`, `g12-setup-6`, `g12-setup-states`; `tools/screens_b.py`"),
        ("F8 residual", "The two panes show SVN's lines on the left and Git's on the right, compared with each other as the page does today. The base appears only in its own panel. The confirmation's row reads “Not chosen: Git version …”.",
         "`options/*/screens/g06-conflict-diff`, `g06-conflict-states`; `tools/screens_b.py`"),
        ("N3 (and the F6 and F9 residuals)", "“Resume” is removed from the operations vocabulary. Stop is described the same way there, in interaction contract §3 and §5 and in row G04.13: shown to administrators. (v1.4 added a second half to that sentence; v1.6 removed it.)",
         "`options/*/screens/s05-operations`; `contracts/interaction.md` §3; row G04.13"),
        ("N4", "S33, D16, interaction contract §5 and the note on the shell pages say what issue #268 says: on Identity & notifications, reading, and replacing the identity mappings, need only a session; saving and testing the Teams webhook are checked for the administrator role. "
         "“For Chris” item 2 says that G11.3 needs its own change. The shell pages' table reads “Administrators, in navigation only (#268)” for the two pages.",
         "row S33, row G11.3; `DECISIONS.md`; `contracts/interaction.md` §5; `options/*/screens/s01-*`"),
        ("N5", "The non-administrator's shell shows “Held”, the same aggregate health as every other page.", "`options/*/screens/s01-shell-member`; `tools/build.py` (`shell`)"),
        ("N6 (F2 residual)", "The unknown evidence specimen reads “Sync configuration: unknown. No supported observation is available. Health, freshness and operation evidence remain unknown.”", "`options/*/screens/g04-overview`; `tools/screens_a.py` (`evidence`)"),
        ("N7 (F2 residual)", "The held repository has one story everywhere: a full history import stopped at r768 of r1871, written locally through r768, confirmed on the Git remote through r750. "
         "The dashboard's attention list, its table, the operations page's status line and its operation card agree. The snapshot chip is gone, the snapshot notice is labelled as another repository's, and the commit map no longer lists the held repository.",
         "`options/*/screens/g02-dashboard`, `g04-operations`, `g04-states`, `s08-existing-layouts-light`; `tools/screens_a.py`"),
        ("N8", "The label column of a label and value list is capped at 40% of its container, so a long label cannot squeeze its value.", "`prototypes/assets/base.css` (`dl.kv`)"),
        ("N9", "Review notes wrap and cannot be wider than their container. Paragraphs, list items and headings in the document pages break long words, which covers plain addresses.", "`prototypes/assets/base.css` (`.tag-needs`); every `*.html` rendering of a document"),
        ("N10", "Names that are links inside a table stay whole. Identifiers of up to 24 characters never break. Longer ones, such as file paths, break after a slash before they break inside a word.", "`prototypes/assets/base.css`; `tools/kit.py` (`mono`, `svn`, `git`)"),
        ("N11", "The Skip dialog opens with nothing selected and “Skip selected” unavailable, as the page does today. The state after a choice is a labelled specimen on the same page.", "`options/*/screens/g04-skip`"),
        ("N12", "Corrected: five shell changes, E1 to E4 and E6, wherever four or “E1 to E5” was written; the cover no longer says the existing layouts are untouched; implementation map §5, §9 and §10; the matrix file's reference text; S31's note; "
         "the index says the markup is identical on 68 of 69 screens. For `/ldap`, the existing layouts' form is not changed: the four group headings are elements hidden in those layouts, and D15 and the implementation map now list them with the other hidden elements.",
         "`README.md`; `DECISIONS.md` (D15); `contracts/implementation-map.md`; `contracts/page-regions.md`; `matrix/surface-state-matrix.json`; row S31; the Artifact cover"),
        ("N13", "Links to earlier versions are labelled as private Artifacts and each is given with the SHA-256 that identifies it.", "`README.md`, `CHANGES.md`, `history/README.md`, `tools/kit.py`"),
        ("F14 residual", "On the sixth setup step the marker “Collected but not sent today” sits directly under Authentication mode and names it and the OAuth fields. The matrix file no longer says captures are not part of v1.",
         "`options/*/screens/g12-setup-6`; `matrix/surface-state-matrix.json`"),
        ("Optional items", "All three taken: the two “Who sees it” cells; the two opened groups in Guide start at the same height; the shell specimen pages mark no destination as current.", "`options/*/screens/s01-*`, `s02-chooser`, `s03-*`, `s07-*`; `options/guide/option.css`"),
        ("Phone tables", "Kept as scroll frames with the shadow, as the publisher asked. Stacked rows is recorded as an improvement for #264 to decide (implementation map §10, decision D22).", "`contracts/implementation-map.md` §10; `DECISIONS.md`"),
    ]
    md = ["# Changes from v1.3, by item", "",
          "This file was written for v1.4 and is kept as it was, apart from the lines it marks as corrected. [CHANGES-FROM-v1.4.md](CHANGES-FROM-v1.4.md) records what the closing check of v1.4 found afterwards.", "",
          "The follow-up review of v1.3 (2026-11-16) confirmed most of the first review's findings as fixed and listed what remained: the rest of F8, residuals in F2, F6 and F14, and new items N1 to N13. "
          "Every one is accepted and changed here. Nothing is disputed.", "",
          "| Item | What changed in v1.4 | Where |", "|---|---|---|"]
    for r in rows:
        md.append("| " + " | ".join(r) + " |")
    md += ["", "## Checked after the changes", "",
           "- `tools/build.py` and its checks pass: structure, links, tokens, evidence against badge, and markup inside `main` that is identical on 68 of the 69 screens (wording corrected in v1.6).",
           "- All 278 source anchors still resolve at both epic tips.",
           "- The tree was searched for statements about access control. What remains is what issue #268 says, cited to it. Functional refusals are described where the page shows their result. "
           "(This line was corrected in v1.5: as first written it claimed less than the tree says.)", "",
           "## Not done, as of v1.4", "",
           "The designer rendered nothing. The publisher captured v1.4 after delivery; the package index records that run."]
    return "\n".join(md) + "\n"


def changes_from_v14_md():
    rows = [
        ("Publication", "The public repository carries only v1.5 and its captures. D23, the index, `history/README.md` and `CHANGES.md` say so. Every path into v1 is removed. "
         "The 2026-11-12 packet is described as inside v1, a private Artifact, with v1's checksum and the archive's. v1.4 is added to the list of earlier versions. The repository path is `docs/design/263/v1.5/`.",
         "`DECISIONS.md` (D23); `README.md`; `history/README.md`; `CHANGES.md`; the Artifact cover"),
        ("M1", "Every role cell was checked again against the page's own role conditions at both revisions. Where the page has none, the cell says only what the page shows: “Every signed-in account, as for the header (row G04.1)”. "
         "The pages that refuse other accounts say that. A row with no control says “No control exists”. Interaction contract §5 and row G04.1 agree with the cells. "
         "The settings edit page no longer draws a role refusal, since that page has no role condition. Nothing is said about what the server checks (decisions D21 and D24).",
         "the role column of `matrix/*`; `contracts/interaction.md` §5; row G04.1; `options/*/screens/g04-settings-edit`; `tools/matrix_data.py`"),
        ("M2", "The personal credential test is drawn as it behaves today: the button, and the client's error text. Both invented messages are gone. "
         "Row G08.4, *existing, known defect*, records that the route the button calls is not registered. A test that works is the new row G08.6, *needs separately owned feature work*.",
         "`options/*/screens/g08-account-states`; rows G08.4 and G08.6"),
        ("M3", "The repository connection tests are in the edit view only, as today. The read view has no test. The edit view now shows the Git target with its test as well as the SVN source.",
         "`options/*/screens/g04-settings`, `g04-settings-edit`; rows G04.2, G04.3; `crosswalk/legacy-navigation-settings.md`"),
        ("M4", "An identifier of up to 24 characters stays whole while its container can hold it; when it cannot, it wraps inside the container, after an underscore first. "
         "A longer one is cut into its path segments, so a line breaks after a slash before it breaks inside a segment. Inside a table a short identifier does not break, and a path may break after a slash. Table headers wrap, which gives the overview's pairs table its room. (The sentence about tables was corrected in v1.6.)",
         "`prototypes/assets/base.css`; `tools/kit.py` (`_ident`)"),
        ("M5", "The held status line shows “Position recorded r750 8a02f5d” alone. “Confirmed on the Git remote” stays on the operation card.", "`options/*/screens/g04-operations`"),
        ("M6", "(a) The held states say “Settings and credentials cannot be saved while an import is held; the save stops at its first stage”. (b) Interaction contract §4 says fields are checked “where the server validates, again on save”. "
         "D21 now reads: the package makes no statement about access control beyond issue #268, and describes a functional refusal only where the page shows its result. The line in `CHANGES-FROM-v1.3.md` is corrected the same way.",
         "`options/*/screens/g04-settings-edit`, `g04-states`; `contracts/interaction.md` §4; `DECISIONS.md` (D21); `CHANGES-FROM-v1.3.md`"),
        ("M7", "Test results use the source's own messages: “Repository Root: …”, “Authentication required (401 Unauthorized). The API URL is reachable but the repo may be private.” and “Failed to run svn command: …”. "
         "The directory test's two results were of the same kind and are changed too: “LDAP connection successful” and “LDAP connection test failed: …”.",
         "`options/*/screens/g03-add-repository`, `g12-setup-states`, `g10-directory-states`"),
        ("M8", "The index's list of checks says 68 of 69. The note about the live shell is true on both shell pages. The publisher's v1.4 capture run is recorded with its two checksums. The index says the publisher captures v1.5 after delivery.",
         "`README.md`; `options/*/screens/s01-*`"),
        ("Found by the designer's render check", "Three tables were cut off at 1440 px in at least one option: the commit map (its two variants are now one above the other), the removal review (its dialog is the wide one), "
         "and the audit table (its long links wrap, its cells are tighter, and its opened detail row wraps). The sync records table has tighter cells so it fits beside the status evidence. "
         "A label column is as wide as its longest label, at most 40%, which leaves values more room. One overflow introduced by M7 on a phone was caught and removed before delivery.",
         "`prototypes/assets/base.css`; `options/*/screens/g04-activity`, `g04-remove-pair`, `g07-audit-table`, `g04-overview`, `g03-add-repository`"),
        ("Also corrected", "Row G08.3's role cell said “Local account”; the page offers the password change to every account except the role `ldap`. "
         "The operations vocabulary listed a “Verify” control for writes to SVN and pushes to Git; the interface offers none, and the page now says so. Its other cells name the controls as the page has them.",
         "row G08.3; `options/*/screens/s05-operations`"),
    ]
    md = ["# Changes from v1.4, by item", "",
          "This file was written for v1.5 and is kept as it was, apart from the lines it marks as corrected. Where it says the repository carries v1.5, that is now v1.6; "
          "[CHANGES-FROM-v1.5.md](CHANGES-FROM-v1.5.md) records what the final check of v1.5 found afterwards.", "",
          "The closing check of v1.4 (2026-11-16) confirmed every item of the review before it as fixed and listed eight new items, M1 to M8. The owner also changed what the public repository carries. "
          "Every item is accepted and changed here. Nothing is disputed.", "",
          "| Item | What changed in v1.5 | Where |", "|---|---|---|"]
    for r in rows:
        md.append("| " + " | ".join(r) + " |")
    md += ["", "## Checked after the changes", "",
           "- `tools/build.py` and its checks pass: structure, links, tokens, evidence against badge, and markup inside `main` that is identical on 68 of the 69 screens (wording corrected in v1.6).",
           "- Every source anchor resolves at both epic tips, including the new row's.",
           "- The tree was searched again for statements about access control. What remains is what issue #268 says, cited to it.",
           "- The render check in the package index: no errors, no requests outside the package, no page wider than the screen, none of the elements it measures past its box, and no table frame in a prototype page that scrolls at 1440 px. "
           "(Reworded in v1.6 to say what the check measures.)", "",
           "## Not done", "",
           "No visual review of every page, and no keyboard, screen-reader, zoom or accessibility check. The publisher captures this version after delivery."]
    return "\n".join(md) + "\n"


def changes_from_v15_md():
    rows = [
        ("Publication", "The public repository carries v1.6 and its captures. D23, the index, `history/README.md`, `CHANGES.md` and the cover name it. v1.5 joins the private earlier versions with its checksum. "
         "The repository path is `docs/design/263/v1.6/`, with captures at `docs/design/263/captures/v1.6/`. The publisher's v1.5 capture run is recorded beside the earlier runs.",
         "`DECISIONS.md` (D23); `README.md`; `history/README.md`; `CHANGES.md`; the Artifact cover"),
        ("K1", "(a) Row G04.30 says what the page does: every signed-in account sees the Errors card, and “Acknowledge” is shown to administrators (row G02.5). "
         "(b) The crosswalk's settings table no longer names a role beside a setting. Its column now gives the scope and the matrix rows that say who sees the setting, for every line of the table (decision D28). "
         "The whole column was checked against the rows it points to. Nothing is said about what the server checks.",
         "row G04.30; `crosswalk/legacy-navigation-settings.md`; `tools/docs.py`; `DECISIONS.md` (D28)"),
        ("K2", "The render check's scope is stated exactly in the index: which elements, against which boxes, and what it does not measure. D27 and the option pages use the same wording. "
         "The script now measures against dialogs, includes third-level headings, and reads only its own user's browser processes, once, right after launch. "
         "The three older lines that said no browser could start are dated to their version. The screenshots the designer looked at are called a sample.",
         "`README.md`; `DECISIONS.md` (D27); `tools/render_check.js`; `tools/README.md`; `CHANGES.md`; `CHANGES-FROM-v1.md`; `CHANGES-FROM-v1.4.md`"),
        ("K3", "The sentence about who else may stop an import is removed. The operations vocabulary, interaction contract §3 and §5 and row G04.13 say that the import controls are shown to administrators. D21 is unchanged and now literally true.",
         "`options/*/screens/s05-operations`; `contracts/interaction.md` §3, §5; row G04.13; `CHANGES-FROM-v1.3.md`"),
        ("K4", "(a) The note beside the two tests in the edit view says only that the result appears as a message, as today. (b) “Credential save failed” shows the client's text as it renders it, with the reply's own body. "
         "(c) The stopped import was stopped by an administrator, on the operations page and on the dashboard. (d) The Git token field in the edit view says that no token is saved, matching the read view.",
         "`options/*/screens/g04-settings-edit`, `g08-account-states`, `g04-operations`, `g02-dashboard`"),
        ("K5", "Tables inside a dialog cover their edge shadow with the dialog's own colour, so Ledger's dialogs show no shadow when nothing is hidden. A row of buttons that follows a form keeps a gap from the last field. "
         "An identifier may also break after a full stop, so a long host name wraps there and no slash is left alone on a line. The wording about tables is corrected in `CHANGES-FROM-v1.4.md`: "
         "inside a table a short identifier does not break, and a path may break after a slash.",
         "`prototypes/assets/base.css`; `options/ledger/option.css`; `tools/kit.py` (`_ident`); `CHANGES-FROM-v1.4.md`"),
        ("K6", "Row G08.6 follows G08.5. The unused history code is removed from `tools/finish.py` and `tools/docs_text.py`. `CHANGES-FROM-v1.4.md` and `CHANGES-FROM-v1.3.md` say the markup is identical on 68 of 69 screens.",
         "`matrix/*`; `tools/finish.py`, `tools/docs_text.py`; `CHANGES-FROM-v1.4.md`, `CHANGES-FROM-v1.3.md`"),
        ("The 405 specimen", "Kept. Its note adds that a proxy in front of the server may answer differently.", "`options/*/screens/g08-account-states`"),
    ]
    md = ["# Changes from v1.5, by item", "",
          "The final check of v1.5 (2026-11-16) verified M1 to M8 as fixed apart from two role statements, which are its one required change, K1. It listed five smaller items, K2 to K6. "
          "Every item is accepted and changed here. Nothing is disputed.", "",
          "| Item | What changed in v1.6 | Where |", "|---|---|---|"]
    for r in rows:
        md.append("| " + " | ".join(r) + " |")
    md += ["", "## Checked after the changes", "",
           "- `tools/build.py` and its checks pass: structure, links, tokens, evidence against badge, and markup inside `main` that is identical in all three options on 68 of the 69 screens.",
           "- Every source anchor resolves at both epic tips.",
           "- Every role statement in the matrix, the crosswalk and the interaction contract was read again against the page's own role conditions.",
           "- The tree was searched again for statements about access control. What remains is what issue #268 says, cited to it.",
           "- The render check was run again on this tree. Its results are in the package index.", "",
           "## Not done", "",
           "No visual review of every page, and no keyboard, screen-reader, zoom or accessibility check. The publisher captures this version after delivery."]
    return "\n".join(md) + "\n"


def bridge_docs(write, write_md):
    classes, rows, prow, strings = bridge.analyse()
    uses = sum(classes.values())
    undecided = [r for r in rows if r[7] == "per-use decision"]
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\n")
    w.writerow(["class", "uses", "variant", "utility", "colour", "step", "opacity", "page_token"])
    for r in rows:
        w.writerow(r)
    write("contracts/colour-bridge-classes.csv", buf.getvalue())
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\n")
    w.writerow(["background_class", "text_class", "uses", "background_token", "text_token"] + [f"{o.replace(' ', '_')}_{t}" for o, t in SETS])
    lows = {k: [] for k in SETS}
    mins = {k: (99, None) for k in SETS}
    for bw, tw, n, bt, tt, res in prow:
        d = dict(res)
        w.writerow([bw, tw, n, bt, tt] + [d[k] if d[k] is not None else "not computed" for k in SETS])
        for k in SETS:
            if d[k] is None:
                continue
            if d[k] < 4.5:
                lows[k].append((bw, tw, d[k]))
            if d[k] < mins[k][0]:
                mins[k] = (d[k], f"`{bw}` with `{tw}`")
    write("contracts/colour-bridge-pairs.csv", buf.getvalue())
    by_token = {}
    for r in rows:
        by_token.setdefault(r[7], [0, 0])
        by_token[r[7]][0] += 1
        by_token[r[7]][1] += r[1]
    md = ["# Colour bridge", "",
          "How existing pages get their colour from the page tokens without every page being rewritten first. It serves the light theme in all six layouts and both themes of the three new ones.", "",
          "This is analysis of source text and arithmetic on token values. Nothing was built or rendered.", "",
          "## What was counted", "",
          f"Every non-test `.tsx` file under `web-ui/src` at `{Z.P}` (epic/reposync-partial-52), read with `git show`: {len(bridge.files())} files. "
          f"{strings:,} class strings set a colour. They use **{len(rows)} distinct colour classes, {uses:,} times**. "
          "The list, with the token each resolves to, is [colour-bridge-classes.csv](colour-bridge-classes.csv).", "",
          "## How the bridge works", "",
          "- `tailwind.config.js` has an empty `theme.extend` today. The bridge points each colour utility that is in use at a CSS variable, and the variable's default is the value Tailwind gives now. "
          "With no theme attribute set, every page renders exactly as it does today.",
          "- Tailwind lets background, text, border, ring, divide and placeholder colours be set separately, so `bg-gray-800` and `text-gray-800` can resolve to different tokens.",
          "- Each layout and theme other than today's dark sets those variables to its page tokens, by the rules below.", "",
          "## Rules", "",
          "| Utility | Colour and step | Resolves to |", "|---|---|---|",
          "| background | grey 900 and darker | `--rs-bg` |", "| background | grey 800 | `--rs-surface` |", "| background | grey 700, and grey 200 to 400 | `--rs-surface-2` |",
          "| background | grey 500 and 600 | `--rs-surface-3` |", "| background | a colour at 800 or darker, at 200 or lighter, or at any step with opacity 50 or less | that colour's tint (`--rs-danger-soft`, `--rs-ok-soft`, `--rs-warn-soft`, `--rs-held-soft`, `--rs-accent-soft`, `--rs-run-soft`) |",
          "| background | blue or indigo at 300 to 700, opaque | `--rs-accent` |", "| background | red at 300 to 700, opaque | `--rs-danger-solid` |",
          "| background | green, yellow, orange, purple, cyan at 300 to 700, opaque | that state's colour (`--rs-ok`, `--rs-warn`, `--rs-held`, `--rs-git`, `--rs-run`) |",
          "| background | black with opacity | `--rs-scrim` |",
          "| text | grey 100 and 200 | `--rs-text` |", "| text | grey 300 and 400 | `--rs-text-2` |", "| text | grey 500 and darker | `--rs-text-3` |",
          "| text | a colour at 300 or darker | that state's colour; blue resolves to `--rs-link` |", "| text | a colour at 100 or 200 | `--rs-text` (it sits on its own tint) |",
          "| border, ring, divide | grey 700 and darker | `--rs-line` |", "| border, ring, divide | grey 600 and lighter | `--rs-line-strong` |",
          "| ring, outline | blue | `--rs-focus` |", "| border | a colour | that state's colour |", "",
          "Green and emerald are both “OK”; yellow and amber are both “warning”; purple marks the Git side and directory accounts. Each of these should be confirmed against its uses when the bridge is built; the rules are a starting map.", "",
          "## What the bridge cannot do", "",
          f"**{len(undecided)} classes, {sum(r[1] for r in undecided)} uses, need an edit where they are used.**", "",
          "| Class | Uses | Why a rule cannot decide it |", "|---|---|---|"]
    why = {"text-white": "White text sits on a filled control in 38 of its 39 uses. The right colour depends on the fill: text on the accent, on the destructive fill and on a state fill are three different tokens in the dark themes.",
           "hover:text-white": "A hover colour for links and quiet buttons. It should become the strong text colour, which white is not in a light theme.",
           "bg-white": "Used for the documentation panel and for switch knobs. One is a surface and the other sits on a fill."}
    for r in undecided:
        md.append(f"| `{r[0]}` | {r[1]} | {why.get(r[0], 'Depends on what it sits on.')} |")
    md += ["", "These become classes that name their role, for example text on the accent fill. That is a change to those lines and to nothing else, and Classic renders the same because the new classes default to white.", "",
           "## Contrast of the pairs that appear together", "",
           f"Where one class string sets both a background and a text colour, the pair can be checked: **{len(prow)} distinct pairs, {sum(p[2] for p in prow)} uses**. "
           "Each was resolved by the rules, with white text resolved by the fill it sits on, and its contrast computed in every token set. The full table is [colour-bridge-pairs.csv](colour-bridge-pairs.csv).", "",
           "| Token set | Pairs below 4.5:1 | Lowest ratio | Pair |", "|---|---|---|---|"]
    for k in SETS:
        md.append(f"| {k[0]}, {k[1]} | {len(lows[k])} | {mins[k][0]}:1 | {mins[k][1]} |")
    md += ["", "Limits of this check:", "",
           "- It covers pairs written in the same class string. Most text takes its background from an ancestor element, and those pairs are covered only by the token pairs in the design-system contract.",
           "- A class string split across a condition is read as separate strings, so some pairs are missed.",
           "- Translucent backgrounds are treated as their tint token, not blended.",
           "- It is arithmetic on declared values. It is not a rendered measurement and not an accessibility result.", "",
           "## Tokens the classes resolve to", "", "| Page token | Classes | Uses |", "|---|---|---|"]
    for t, (c, n) in sorted(by_token.items(), key=lambda x: -x[1][1]):
        md.append(f"| `{'--rs-' + t if t != 'per-use decision' else t}` | {c} | {n} |")
    md += ["", "## Colour that does not come from utility classes", "",
           "The bridge cannot reach these. Each needs a change in its own file, which is why the bridge is not “no page changes” for every page.", "",
           "| Where | Today | Treatment |", "|---|---|---|",
           "| `UIForgeActivityStream` on the dashboard and the audit log (`Dashboard.tsx:313`, `:392`; `AuditLog.tsx:197`) and `uiforge-overrides.css` | `theme=\"dark\"` is fixed | Pass the current theme, and move the overrides onto the page tokens |",
           "| `react-diff-viewer` (`DiffViewer.tsx:24`) | `useDarkTheme={false}`: the diff renders light inside a dark page | Set it from the current theme, and take the diff colours from `--rs-code-bg`, `--rs-add-bg` and `--rs-del-bg` |",
           "| Inline SVG colours (`Dashboard.tsx:162` to `165`, `Repositories.tsx:31` to `45`) | fixed hex values for the SVN and Git marks | Use `--rs-svn` and `--rs-git` through `currentColor` |",
           "| `Documentation.tsx` | light utility classes on a white panel inside the dark shell | Covered by the bridge once its classes are mapped like any other page; today it is the one page that is already light |", "",
           "These line numbers are at the partial-52 tip."]
    md += ["", "## What it means for #264", "",
           "- The bridge is one configuration change and one stylesheet, plus the per-use edits above. It makes every page legible in every layout and theme before any page is restructured.",
           "- It does not give Ledger its rules, Console its density or Guide its cards. Those come from the shared presentational classes and the page regions.",
           "- The existing layouts' light theme is the bridge with the existing-layouts light palette, plus the four items in the section above."]
    write_md("contracts/colour-bridge.md", "\n".join(md) + "\n", "Colour bridge")
    return dict(classes=len(rows), uses=uses, undecided=len(undecided), undecided_uses=sum(r[1] for r in undecided), pairs=len(prow), pair_uses=sum(p[2] for p in prow),
                low=sum(len(v) for v in lows.values()))


# region id, what it is today, text found in the source, matrix rows, where it sits in Ledger, Console and Guide, note
DASH = [
    ("scope", "Repository scope select", 'aria-label="Repository scope"', "G02.1", "Page head", "none"),
    ("imports", "Import progress cards", "<ImportProgressCard", "G02.2", "In flight", "Running operations from the status read are listed with them"),
    ("repositories", "“Repositories” overview", "Repositories Overview", "G02.3", "Repositories", "A table in every option"),
    ("health", "Sync health panel and the count cards", "<StatusObservationPanel view={statusView}/>", "G02.4, G02.5", "Sync health", "Keeps the error acknowledgement"),
    ("position", "“Sync Position” (one repository in scope)", "Sync Position", "G02.6", "Sync health and position", "none"),
    ("records", "“Recent Sync Records”", "Recent Sync Records", "G02.7", "Recent sync records", "none"),
    ("commit-map", "“Commit Map (SVN ↔ Git)”", "Commit Map (SVN", "G02.8", "Commit map", "none"),
    ("activity", "“Recent Activity”", "Recent Activity", "G02.9", "Recent activity", "none"),
    ("server", "Server panel", "<ServerMonitor", "G02.10", "Server", "none"),
    ("attention", "none: new, derived from the same status reads", None, "G02.11", "Needs attention: first, under the page head", "Read-only; links only. In the tree of every layout and hidden in Classic, Individual and Team"),
]
REPO = [
    ("head", "Name, sync switch, Edit, Git Hook, Trigger Sync", "Trigger Sync", "G04.1, G04.7", "Page head, on every section", "none"),
    ("svn", "“SVN Configuration”", "SVN Configuration", "G04.2, G04.4", "Settings: SVN source", "The password field stays inside this region"),
    ("git", "“Git Configuration”", "Git Configuration", "G04.3, G04.4", "Settings: Git target", "The token field stays inside this region"),
    ("sync", "“Sync Settings”", "Sync Settings", "G04.5", "Settings: Sync policy", "none"),
    ("policy", "Path policy (inside the edit form)", None, "G04.6", "Settings: Path policy", "The #56 editor on partial-52; two text areas on reliability-61"),
    ("about", "“Quick Stats”", "Quick Stats", "G04.8", "Settings: About", "none"),
    ("paused", "Paused-after-error banner", "Circuit Breaker: Error Paused Banner", "G04.9, G04.10", "Overview, first, while paused", "none"),
    ("status", "Status panel and cards", "<StatusObservationPanel view={statusView}/>", "G04.11, G04.30", "Overview: “What this status proves” and “Errors”", "Keeps the error acknowledgement"),
    ("import", "Import notice and import card", "<ImportHistoryNotice fields={baselineNoticeFields}", "G04.12, G04.13, G04.14", "Operations; the current operation also shows on Overview", "One region shown in two sections"),
    ("records", "“Sync Records”", "{/* Sync Records", "G04.15", "Activity; also shown short on Overview", "One region shown in two sections, not two copies"),
    ("commit-map", "“Commit Map (SVN ↔ Git)”", "Commit Map (SVN", "G04.16", "Activity", "none"),
    ("audit", "“Recent Audit Log”", "Recent Audit Log", "G04.17", "Activity", "none"),
    ("refresh", "“Update pair from parent”", "Update pair from parent", "G04.18", "Branch pairs, first", "Held under #69; preview only; not moved out of that owner's code"),
    ("reanchor", "“Re-anchor pair (not implemented)”", "reanchor-not-implemented", "G04.19", "Branch pairs, second", "Held"),
    ("pairs", "“Branch Pairs”", "{/* Branch Pairs", "G04.20, G04.21, G04.25", "Branch pairs, third; also shown short on Overview", "One region shown in two sections"),
    ("server", "Server panel", "<ServerMonitor />", "G04.29", "Overview, last, collapsed", "none"),
    ("stop-remove", "“Danger Zone”", "Danger Zone - admin only", "G04.22, G04.23, G04.24, G04.26", "Settings: Stop or remove, last", "none"),
    ("status-line", "none: new, derived from the status read", None, "G04.32", "Under the page head, on every section", "Read-only: state, observed time, last sync, position and a link to the evidence. Hidden in the existing three layouts"),
    ("credentials", "none: new, derived from the credential read", None, "G04.4, S23, S24", "Settings: Credentials, after Git target", "A summary. “Replace…” opens the field in its own region and sends nothing itself. Hidden in the existing three layouts"),
]


def _order(path, rows):
    out = []
    for rid, today, anchor, mrows, where, note in rows:
        lp = Z.find(Z.P, path, anchor)[0] if anchor else None
        lr = Z.find(Z.R, path, anchor)[0] if anchor else None
        out.append(dict(id=rid, today=today, text=anchor or "none", line_partial52=lp, line_reliability61=lr, matrix_rows=mrows, new_options=where, note=note))
    known = sorted([o for o in out if o["line_partial52"]], key=lambda o: o["line_partial52"])
    for i, o in enumerate(known):
        o["order_today"] = i + 1
    for o in out:
        o.setdefault("order_today", "not in the page today" if o["text"] == "none" and o["today"].startswith("none") else "inside the edit form")
    return sorted(out, key=lambda o: (not isinstance(o["order_today"], int), o["order_today"] if isinstance(o["order_today"], int) else 0))


def regions_docs(write, write_md):
    dash, repo = _order("web-ui/src/pages/Dashboard.tsx", DASH), _order("web-ui/src/pages/RepoDetail.tsx", REPO)
    write("contracts/page-regions.json", json.dumps({"revisions": {"epic/reposync-partial-52": Z.P, "epic/reposync-reliability-61": Z.R},
                                                     "pages": {"/": dash, "/repos/:id": repo}}, indent=1, ensure_ascii=False) + "\n")

    def table(path, rows):
        md = ["| Region | Order today | Today | Source line (partial-52, reliability-61) | Matrix rows | In Ledger, Console and Guide | Note |", "|---|---|---|---|---|---|---|"]
        for o in rows:
            src = f"`{path}:{o['line_partial52']}`, `:{o['line_reliability61']}`" if o["line_partial52"] else "none"
            md.append(f"| `{o['id']}` | {o['order_today']} | {o['today']} | {src} | {o['matrix_rows']} | {o['new_options']} | {o['note']} |")
        return md
    md = ["# Page regions", "",
          "The rule that lets a page be arranged for a layout without losing a person's work when they switch, and without changing what the keyboard reaches first. It applies decisions D8 and D12.", "",
          "## The rule", "",
          "- **One tree per page.** A page renders the same named regions, with the same parents and keys, in every layout. `LayoutFrame` already does this for the shell.",
          "- **Document order is today's order.** Regions stay in the order they have in the source now, in every layout. Classic, Individual and Team are unchanged. "
          "Ledger, Console and Guide show regions in that same order within each view, so what is seen, what the keyboard reaches and what a screen reader reads agree.",
          "- **A layout arranges; it never re-renders.** Which regions show together and how they are drawn is decided by the layout's styles and a section attribute on the page. No region is rendered conditionally on the layout.",
          "- **Sections keep everything mounted.** On the repository page a section shows its regions and hides the others. A draft in one section, an open dialog and a running request survive a change of section and a change of layout.",
          "- **A region shown in two places is one region.** Where Overview shows a short list that another section shows in full, it is the same region, not a second copy with its own requests.",
          "- **Three regions are new, read-only and derived**: “Needs attention” on the dashboard, the status line on the repository page, and the credentials summary in repository settings. "
          "They make no request of their own and hold no control that sends anything. They are in the tree of every layout, and hidden in Classic, Individual and Team so that they are neither seen, nor reached by the keyboard, nor read.", "",
          "v1.1 recommended a new document order for these two pages. That is withdrawn: it would have changed keyboard and screen-reader order in the default layout.", "",
          "## Dashboard: `/`", "", "Order today is the order of the regions in `web-ui/src/pages/Dashboard.tsx` at partial-52, resolved by script.", ""]
    md += table("pages/Dashboard.tsx", dash)
    md += ["", "Order in Ledger, Console and Guide: scope, needs attention, then today's order: in flight, repositories, sync health, position, recent sync records, commit map, recent activity, server. "
           "From 1101 px Console puts needs attention in a left column and the rest beside it; it is first in the document, so the keyboard reaches it first.", "",
           "## Repository: `/repos/:id`", "", "Order today is the order of the regions in `web-ui/src/pages/RepoDetail.tsx` at partial-52.", ""]
    md += table("pages/RepoDetail.tsx", repo)
    md += ["", "Sections in the new options, with the `?section=` value for each (decision D7). Regions are listed in document order, which is the order they are shown in.", "",
           "| Section | `?section=` | Regions |", "|---|---|---|",
           "| Overview | `overview` or absent | paused, status, import (the current operation), records (short), pairs (short), server |",
           "| Activity | `activity` | records, commit-map, audit |",
           "| Branch pairs | `branches` | refresh, reanchor, pairs |",
           "| Operations | `operations` | import |",
           "| Settings | `settings` | svn, git, credentials, sync, policy, about, stop-remove |", "",
           "`head` and `status-line` show on every section.", "",
           "## Other pages", "",
           "| Route | Arrangement |", "|---|---|",
           "| `/repos`, `/conflicts`, `/conflicts/:id`, `/audit`, `/users`, `/config`, `/setup`, `/docs`, `/settings` | Regions keep today's order in every layout. The options change wording and how regions are drawn. |",
           "| `/ldap` | The new options show four headings between the fields: Directory sign-in, Connection, User mapping, Service account. They are elements in the tree of every layout and hidden in Classic, Individual and Team, from sight and from assistive technology, like the derived regions (decision D15). Those layouts are neither seen nor read differently. The form's own markup and field order do not change. |",
           "| `/login` | Unchanged (decision D5). |"]
    write_md("contracts/page-regions.md", "\n".join(md) + "\n", "Page regions")


def history_pointer_md():
    return f"""# History

The public repository carries one version of this package: **v1.6**, this one, with the publisher's captures beside it. That is the owner's decision.

Everything earlier stays a private Artifact. The links below open only for their owner. The SHA-256 of each `SHA256SUMS` is what identifies a version.

This version carries no copy of the 2026-11-12 design review. That packet is inside v1, unpacked and unchanged, in v1's folder `history/2026-11-12/` (41 files). The archive delivered on 2026-11-12 has SHA-256 `{kit.V1_ARCHIVE_SHA}`.

| Version | Date | What it is | Identified by |
|---|---|---|---|
| 2026-11-12 review | 2026-11-12 | A design review of the reliability epic at `acf027bec8b6c9c0ceb3f53fe5a9e794d40a2b81`, before #262. Superseded where it differs. | inside v1; SHA-256 of the archive delivered that day: `{kit.V1_ARCHIVE_SHA}` |
| v1 | 2026-11-16 | Three options, the matrix, crosswalk and contracts, with nine decisions open. Rendered and independently reviewed. | SHA-256 of its `SHA256SUMS`: `{kit.V1_SUMS}`. Private Artifact: {kit.V1_ARTIFACT} |
| v1.1 | 2026-11-16 | The first round of decisions applied. | SHA-256 `{kit.V11_SUMS}`. Private Artifact: {kit.V11_ARTIFACT} |
| v1.2 | 2026-11-16 | All decisions applied. Rendered. | SHA-256 `{kit.V12_SUMS}`. Private Artifact: {kit.V12_ARTIFACT} |
| v1.3 | 2026-11-16 | The first review's fourteen changes. Rendered and reviewed a second time. | SHA-256 `{kit.V13_SUMS}`. Private Artifact: {kit.V13_ARTIFACT}. An earlier issue, SHA-256 `{kit.V13_WITHDRAWN_SUMS}`, was withdrawn before use |
| v1.4 | 2026-11-16 | The second review's changes. Rendered and reviewed a third time. | SHA-256 `{kit.V14_SUMS}`. Private Artifact: {kit.V14_ARTIFACT} |
| v1.5 | 2026-11-16 | The third review's changes. Rendered and reviewed a fourth time. | SHA-256 `{kit.V15_SUMS}`. Private Artifact: {kit.V15_ARTIFACT} |
| v1.6 | 2026-11-16 | This version: the accepted design. See [../CHANGES-FROM-v1.5.md](../CHANGES-FROM-v1.5.md). | this tree |
"""
