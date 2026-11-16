# What changed

Earlier versions are unchanged and none of them is in the repository. They are private Artifacts: the links open only for their owner, and the SHA-256 of each `SHA256SUMS` is what identifies them.

- v1: SHA-256 `5eb5812a4548774f9c33103c8a3b3521d0df434c397537cfad538649f8f8d814` (private Artifact: https://claude.ai/artifact/U4qoeNzph9x32eey55Hjg1)
- v1.1: SHA-256 `46d58debfd6b453bf184233ceea3daeb4679d6d20370cae718119ef17742ff65` (private Artifact: https://claude.ai/artifact/5aKdwSu3jSJgBQ1LYZ59RA)
- v1.2: SHA-256 `e1085be16de128e4ce376edac0da62996a4505077d8cb0103eabc5580fff2c45` (private Artifact: https://claude.ai/artifact/H4hqT7suswfojB3jk3ke5A)
- v1.3: SHA-256 `3c0ea6696929d637cd53af41466833ad3645cfb17debc1f11533f8d103625127` (private Artifact: https://claude.ai/artifact/522LiBWfRtC2X8b16azYBd). An earlier issue of v1.3, SHA-256 `e5432433b808dbeda75319eccf443558cca97a116efd4b13c081887c2810c921`, was withdrawn before use.
- v1.4: SHA-256 `671a9cbf59a8e90e800339607524d07831004d85864794c2d818df8b38dbd769` (private Artifact: https://claude.ai/artifact/GVVT4zmw43gCmDiiwLEqiB)
- v1.5: SHA-256 `3d4562774d3a19365982def3042dc3b336273ffee73d22f8d39b663e103cf37f` (private Artifact: https://claude.ai/artifact/4aJnJVnfadwoLUhL5Vov9p)

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
