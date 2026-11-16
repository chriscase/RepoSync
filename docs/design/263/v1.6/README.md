# RepoSync #263 design package v1.6

Three new whole-interface options for RepoSync, each in dark and light, wide and narrow, with the matrix, crosswalk and contracts that #264 builds from. Written 2026-11-16 for [issue #263](https://github.com/chriscase/RepoSync/issues/263). It supersedes v1 to v1.5 of the same day. It applies the decisions Chris made and the changes that four independent reviews, of v1, v1.3, v1.4 and v1.5, required.

This is the version for the public repository, at `docs/design/263/v1.6/`, with the publisher's captures beside it at `docs/design/263/captures/v1.6/`. No earlier version is in the repository: v1 to v1.5 stay private Artifacts, recorded by their checksums in [history/README.md](history/README.md).

**This is a design. Nothing in it is in the product.** Classic, Individual and Team stay available and stay the default. #263 cannot close #264.

## Start here

- [DECISIONS.md](DECISIONS.md): what was decided, in Chris's words and the designer's.
- [prototypes/index.html](prototypes/index.html): every screen in every option, with links to flows. Open it from the file system; nothing is fetched.
- [matrix/surface-state-matrix.md](matrix/surface-state-matrix.md): every surface and state, with its source and its prototype pages.
- [CHANGES-FROM-v1.5.md](CHANGES-FROM-v1.5.md): each item of the final check of v1.5, and what was done about it.
- [CHANGES-FROM-v1.4.md](CHANGES-FROM-v1.4.md): each item of the closing check of v1.4, and what was done about it.
- [CHANGES-FROM-v1.3.md](CHANGES-FROM-v1.3.md): each item of the follow-up review of v1.3, and what was done about it.
- [CHANGES-FROM-v1.md](CHANGES-FROM-v1.md): each finding of the independent review of v1, and what was done about it.
- [CHANGES.md](CHANGES.md): what differs from each earlier version.

## The three options

All three are accepted for #264.

| Option | Id | Idea | Navigation, wide | Navigation, narrow | Text and controls |
|---|---|---|---|---|---|
| [Ledger](options/ledger/README.md) | `ledger` | A record to read. Ruled sections, stamps, ink-only accent | Two-row masthead | Workspace links in a second row, with “More” for the rest | 15 px, 34 px controls, serif titles |
| [Console](options/console/README.md) | `console` | An operations console. Dense tables, status beside the work | Grouped sidebar | Bottom bar: four workspace pages and “More” | 13 px, 28 px controls |
| [Guide](options/guide/README.md) | `guide` | Task-led. Four large destinations, roomy sections, big targets | Four pill tabs and “Account and admin” | Bottom bar: four destinations and “More” | 16 px, 42 px controls, rounded |

What they share: one plain vocabulary, one grouping, the same page content, controls and states, the same routes, and the same markup inside every page (identical on 68 of the 69 screens; the chooser page differs only in the option's own name). What differs: navigation model, density, type, how sections are drawn, how a page's sections are reached, the dashboard arrangement, and the phone pattern. No option offers a different page structure. That is deliberate: a layout may arrange a page but never render a different one.

## Coverage

- **140 matrix rows: 124 covered, 15 explicit gap, 1 not applicable.** No blank cells.
- 69 prototype pages per option, 207 in all, plus 16 flows per option. Each page is built for dark and light and for 1440×900 and 390×844.
- For #264: 82 rows are layout only; 14 need an accepted change to the #262 shell; 22 are shared changes built with the layouts; 11 are shared changes that are each their own change; 1 waits for an owner; 10 are feature work outside #264.
- The two epics differ in three rows (path-policy editor, commit-map target cell twice). Each of those rows describes both.

## Contents

| Path | What it is |
|---|---|
| `README.md` | this index |
| `DECISIONS.md` | the decisions of 2026-11-16 |
| `CHANGES-FROM-v1.5.md` | the final check's items K1 to K6, with what changed and where |
| `CHANGES-FROM-v1.4.md` | the closing check's items M1 to M8, with what changed and where |
| `CHANGES-FROM-v1.3.md` | the follow-up review's items N1 to N13 and the residuals, with what changed and where |
| `CHANGES-FROM-v1.md` | the first review's findings F1 to F14 and O1 to O5, with what changed and where |
| `CHANGES.md` | what changed from each earlier version |
| `SHA256SUMS` | SHA-256 of every other file in the tree |
| `prototypes/index.html` | index of every option, screen and flow |
| `prototypes/assets/` | the shared stylesheet, the existing layouts' palette and the one script |
| `options/<id>/README.md` | the option's idea, trade-offs and representative pages |
| `options/<id>/tokens.css`, `option.css` | the option's tokens and arrangement |
| `options/<id>/screens/*.html` | the prototype screens |
| `options/<id>/flows/index.html` | the task flows |
| `matrix/surface-state-matrix.csv`, `.json`, `.md` | the surface and state matrix |
| `matrix/capability-parity.md` | every capability and where each option carries it |
| `crosswalk/legacy-navigation-settings.md` | today's navigation, sections, settings and labels, and where each is in the new options |
| `contracts/design-system.md`, `contrast-computed.csv` | tokens, type, responsive rules, components, focus, contrast, the existing layouts' light palette |
| `contracts/interaction.md` | actions, states, validation, permissions, receipts, held capabilities |
| `contracts/colour-bridge.md` and two `.csv` files | how existing pages take colour from the tokens, with every colour class in the source and its contrast |
| `contracts/page-regions.md`, `.json` | the fixed region tree for the dashboard and the repository page |
| `contracts/implementation-map.md` | what #264 builds, in what order, with test oracles |
| `history/README.md` | where the earlier versions are |
| `tools/` | the scripts that generated and checked this tree, and the render check |

Each `.md` file has an `.html` rendering beside it for reading in a browser.

The screens are in `options/<id>/screens/`, not `options/<id>/prototype/` as the issue suggested, because the Artifact service does not accept a published path that contains a folder named `prototype`.

## Decided

1. All three options go to #264, to the full extent the prototypes show.
2. Dark until chosen, in every layout. Light is offered in all six.
3. Five changes to the #262 shell are accepted (E1 to E4 and E6): a “More” control, a saved theme, wording per layout family, a Help link in the new layouts, and navigation icons. The sign-in page stays as it is.
4. Every shared change the prototypes draw is adopted, in every layout.
5. Three labels that misstate what a control does are corrected in every layout. Every other label in Classic, Individual and Team stays.
6. Pages keep today's order of regions in every layout. The new options fit that order and add read-only summaries at the top.
7. Repository sections are addressable with `?section=`.
8. Held and missing items stay as they are.
9. The markup inside every page is the same in all three options.

The full record, with who decided what and what is still open, is in [DECISIONS.md](DECISIONS.md).

## Provenance

### Source read

- `epic/reposync-partial-52` at `b2b055a72657824fd6662b451f69718d73405aa5`.
- `epic/reposync-reliability-61` at `5853a8884496d294ce21898415d7d2718b73bf7c`.
- Both tips were fetched and read on 2026-11-16. The thirteen routes are identical.
- Against the 2026-11-12 pin `acf027bec8b6c9c0ceb3f53fe5a9e794d40a2b81`, the reliability tip changes nine `web-ui` files, all of them #262: `components/Layout.tsx`, the seven new files under `layouts/` and `tests/layout-foundation.e2e.mjs`.
- Between the two tips, twelve `web-ui` files differ, all on the partial-52 side: `api.ts`, `pages/Dashboard.tsx` and `pages/RepoDetail.tsx` change; eight files are added for the #56 path-policy editor and the commit-map target cell (#54) with their tests; and `tests/layout-foundation.e2e.mjs` differs by four lines. Everything else under `web-ui` is byte-identical, including the #262 catalog, frame, preference and token files.
- Line numbers in the matrix and the page regions were resolved by `tools/resolve.py` against both revisions with `git show`. The colour bridge was counted by `tools/bridge.py` the same way. Nothing was checked out, built or run from the repository.
- Issues read: #263 (the authority for this package), #264, #262, and the issues named in the implementation map as read on 2026-11-12.

### Captures, made by the publisher

The publisher's captures are the visual record. The publisher rendered **v1** on 2026-11-16 with a sandboxed Chromium run as a non-root user: 221 pages, light and dark, at 1440×900 and 390×844, 884 captures. As the publisher reported them: no page or console errors, no request outside the package, the requested theme applied in all 832 prototype captures, and four captures wider than the screen, all the save-recovery page at 390 px. The results file has SHA-256 `106bc228eea2d65d6bbf8051560bc28e18de0e5cae21e60950c2dff294d5b5a9`, and the publisher's checksum list for the capture folder has SHA-256 `380d2aba6ec040158c90b7bb83b8ef8baccbd7a3bb323b1ee6088b53ff124a14`. The captures are not in this package.

The publisher rendered **v1.2** the same way later that day: 880 captures, again with no errors, no outside requests and no theme mismatches, and the same four over-wide captures. The “More” control was opened by hand in each option and rendered complete at 390 px: every link, both selects and sign-out. Seen by eye: identifiers breaking inside two tables, and one table cut at its frame's edge on a phone with nothing to show that it scrolls. The publisher's notes file has SHA-256 `fcb1c12213f9d700cc82627a2fd5c0eb9fcac03564bc365f9d18bb7d6090da52`.

The publisher rendered **v1.3** too: 964 captures, including the open “More” control through the `-menu` fragment, with no errors, no outside requests and no theme mismatches. The save-recovery page fitted. Eighteen captures were wider than the screen at 390 px, from two causes that v1.4 removed: review notes that did not wrap, and long addresses in one document. The results file has SHA-256 `fe8dbc7e8d321dd61f6232f5acd2ccb5c821fb63b9c677c346de019a78137026`, and the publisher's checksum list has SHA-256 `20b97313264b3a1399e2900714614f405d4760a6bdcf69bc13d0385aad35fc4e`.

The publisher rendered **v1.4** as well: 968 captures with no errors, no outside requests, no theme mismatches and no page wider than the screen. The review of v1.4 then found identifiers that crossed the edge of their card at 1440 px, which a page-width measure does not see, and one table header cut at its frame. This version corrects them. The results file has SHA-256 `b6a8c88c65ebc21f5118f0ab276f388edac0b7d986a1edfec1f3045d5f9d7e23`, and the publisher's checksum list has SHA-256 `42dae6d1516601c0e1973800e6076a7a4f8ddfcd093427203e11ebebf163d6d4`.

The publisher rendered **v1.5** too: 972 captures with no errors, no outside requests, no theme mismatches and no page wider than the screen. The final check viewed 29 of them and found no overflow inside a card, and the three tables corrected in v1.5 fitting at 1440 px in every option. The results file has SHA-256 `f8301b7c0335e9ec7c5f568c1a4f3c345748999caf72ca0f6e8cf1cf3552718a`, and the publisher's checksum list has SHA-256 `52e20f8eb684908cfabb6bb1fc38a3eaa00f1528f3abda58313c17d101ed22b3`.

- What they are: pictures of the static, synthetic v1, v1.2, v1.3, v1.4 and v1.5 prototypes.
- What they do not show: the application, the keyboard, a screen reader, zoom, or this version.
- **The publisher captures v1.6 after delivery.** Those captures go into the repository beside this tree, at `docs/design/263/captures/v1.6/`.

For the open “More” control, capture these with `-menu` added to the fragment (`#light-nobar-menu`, `#dark-nobar-menu`): `s01-shell-roles`, `s01-shell-member`, `g02-dashboard` and `g09-users`, in each option. Ledger and Console show the control at the narrow width only; Guide shows it at both.

### The designer's render check

Added in v1.5 and run again on this version. Versions up to v1.4 said that no sandboxed browser could start in the container the package is written in. That was true as root, which is how it had been tried. Run as a non-root user, the same Chromium starts with its sandbox enabled. The script reads that user's browser processes once, right after launch, and none carried a flag that turns the sandbox off.

`tools/render_check.js` loaded every HTML page of this tree except the tools, 230 pages, in light and dark at 1440×900 and 390×844: 920 loads. Requests outside the package are aborted and counted. On each load it looks for page and console errors, a page wider than the screen and a theme other than the one asked for.

It also measures selected elements inside `main`: identifiers, notes, pills, chips, stamps, buttons, links, headings, paragraphs, labels, fields and inline results. Each is measured left and right against the nearest section, banner, dialog, operation card, evidence box, list item, table cell or specimen figure. Table frames are measured separately.

What it does not measure: height; text that overflows inside a block whose own box fits; anything outside `main`, which means the shell and its menu; and any open-menu state. The publisher's captures cover those.

- Result: 0 errors, 0 requests outside the package, 0 pages wider than the screen, 0 elements past their box, 0 theme mismatches.
- At 1440 px no table frame in a prototype page scrolls. Three wide tables in the documents do: the matrix, the crosswalk and the design-system contract. At 390 px table frames scroll on 124 of the 460 loads, which is the accepted treatment for a phone (decision D22).
- The designer looked at a sample: 3 screenshots of pages this version changes, taken by the same check.
- Run on v1.4, the same check reproduced the overflows that version's review found, and found three tables cut off at 1440 px that no review had listed. v1.5 corrected them; see [CHANGES-FROM-v1.4.md](CHANGES-FROM-v1.4.md).
- What it is not: the publisher's captures, which remain the visual record; keyboard, screen-reader, zoom or accessibility evidence; or a test of the application. The results file and the screenshots are not in the package.

### What was done to check this package

Authored checks, run on the files:

- Every HTML page: tags balanced, no duplicate ids, every link and stylesheet resolves inside the tree, no reference to anything outside it, every icon used is defined. 231 pages, 0 problems.
- Every token used by the stylesheets has a value in each option and theme, and in the existing layouts' palette.
- Contrast computed from token values: 180 pairs across the three options, all at or above target; 30 of 30 for the existing layouts' light palette.
- Colour bridge: 211 colour classes in the source resolved by rule except 3; 49 background and text pairs that appear together, computed in seven token sets, 0 below 4.5:1.
- Every matrix row: source text found at both revisions, every referenced prototype page exists, no blank cell; every prototype page is referenced by a row.
- Status evidence on each page describes the same health as the page's status line.
- The markup inside every page is identical in all three options on 68 of the 69 screens. On the chooser page it differs only in the option's own name.
- The render check described above.

Not done:

- **No visual review of every page.** The render check measures; it does not judge how a page looks. The designer looked at a sample of screenshots of changed pages. Faults that a measure does not catch are possible until the publisher's captures of this version are reviewed.
- No build, test, lint, type-check or application launch. No real application evidence of any kind.
- No screen reader, keyboard walk-through, zoom or automated accessibility scan. **No accessibility pass is claimed for any option.** The statement that tab order equals reading order is a property of the markup as written, not a tested result.
- No usability testing. The fit of each option to its audience is the designer's judgement.

### Data

All names, repositories, revisions, hashes and people in the prototypes are invented. Hosts use the reserved `.example` and `.invalid` names (`makerspace.example`, `example.org`, `corp.example`), apart from GitHub's public API address, which is the product's own default for that field, and `localhost` in a help example. No real repository content or credential appears.

### Earlier versions

v1 to v1.5 are kept as published and none of them is in the repository. Each is identified by the SHA-256 of its `SHA256SUMS`. The links are private Artifacts and open only for their owner.

| Version | SHA-256 of its `SHA256SUMS` | Private Artifact |
|---|---|---|
| v1 | `5eb5812a4548774f9c33103c8a3b3521d0df434c397537cfad538649f8f8d814` | https://claude.ai/artifact/U4qoeNzph9x32eey55Hjg1 |
| v1.1 | `46d58debfd6b453bf184233ceea3daeb4679d6d20370cae718119ef17742ff65` | https://claude.ai/artifact/5aKdwSu3jSJgBQ1LYZ59RA |
| v1.2 | `e1085be16de128e4ce376edac0da62996a4505077d8cb0103eabc5580fff2c45` | https://claude.ai/artifact/H4hqT7suswfojB3jk3ke5A |
| v1.3 | `3c0ea6696929d637cd53af41466833ad3645cfb17debc1f11533f8d103625127` | https://claude.ai/artifact/522LiBWfRtC2X8b16azYBd |
| v1.4 | `671a9cbf59a8e90e800339607524d07831004d85864794c2d818df8b38dbd769` | https://claude.ai/artifact/GVVT4zmw43gCmDiiwLEqiB |
| v1.5 | `3d4562774d3a19365982def3042dc3b336273ffee73d22f8d39b663e103cf37f` | https://claude.ai/artifact/4aJnJVnfadwoLUhL5Vov9p |

An earlier issue of v1.3, SHA-256 `e5432433b808dbeda75319eccf443558cca97a116efd4b13c081887c2810c921`, was withdrawn before use. More is in [history/README.md](history/README.md).

The 2026-11-12 review is inside v1, the private Artifact above, unpacked and unchanged. The archive delivered that day has SHA-256 `6b63b9dc4b0667a86bf1accd4b2f62034655952eca6b04ee8858843dee5df6de`. This version carries no copy. One disclosure about the 2026-11-12 packet: its screenshots were taken with a headless browser started with default options, which does not enable the browser sandbox, on local files with the network blocked. No later version did that.

### Versions

`v1.6` is not edited after publication. Corrections go to `v1.7`, beside it.
