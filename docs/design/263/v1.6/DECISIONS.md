# Decisions

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
| Built with the layouts | G01.2, G02.5, G02.7, G02.8, G02.11, G03.3, G04.32, S03, G04.1, G04.10, G04.15, G04.25, G06.1, G06.2, G06.7, G07.1, G07.3, G12.4, G12.6, S16, S20, S23 | Display only: no call is added, changed or made at a different time, and no step is added. Failed reads shown as failed, states as a mark and a word, secrets as “Set”, explanations as text, the confirm label saying what will happen. |
| Each its own small change, with its own tests | G04.7, G04.9, G04.13, G04.23, G04.31, G06.4, G08.4, G11.1, G11.2, G12.9, G12.10 | It changes when a call is made, adds a confirmation, or stops a write after a failed read. Same behaviour in every layout. Each gets its own review so a regression can be traced to it. |
| Waits for its owner | G12.8 | The setup wizard's import step belongs to #64 (RS-19). The options style what exists until that owner changes it. |

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
