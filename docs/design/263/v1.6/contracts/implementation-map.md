# Implementation map for #264

What #264 builds, on what, in what order, and how to tell it worked. Written for the implementation owner, who knows the code and decides how.

**Accepted on 2026-11-16: Ledger, Console and Guide, all three, to the full extent the prototypes show.** See [DECISIONS.md](../DECISIONS.md). A delivered design package is still not implemented acceptance, and #263 cannot close #264.

## 1. What #264 asks to be recorded first, and where it is

| #264 asks for | In this package |
|---|---|
| Accepted design location and version | this tree, `v1.6`; the Artifact URL is in the designer's hand-off message |
| Checksum manifest | `SHA256SUMS` at the package root |
| Reviewed contract version | the files in `contracts/`, version `v1.6` |
| Option ids | `ledger`, `console`, `guide` |
| Theme ids | `dark` (default) and `light`, in all six layouts |
| Source provenance | the package index, section “Provenance” |

## 2. Kinds of work

Each matrix row carries one of these in the column `delivery_for_264`.

| Kind | Meaning | Rows |
|---|---|---|
| **layout only** | The option's shell arrangement, tokens and page arrangement deliver it. | 82 |
| **shell extension** | Needs one of the accepted changes to the #262 shell (E1 to E4, E6). Rows tagged E3 are labels that differ in the plain vocabulary. | 14 |
| **shared change, with the layouts** | An improvement to shared page code that only changes what is displayed. Built with the layouts; appears in every layout. | 22 |
| **shared change, own change** | An improvement that changes when a call is made, adds a step or stops a write. Same in every layout; its own review and tests. | 11 |
| **waits for an owner** | Belongs to an existing owner's open work. The options style what exists until that owner changes it. | 1 |
| **feature work** | Needs behaviour or server data that does not exist. A separately owned dependency, outside #264. | 10 |

Shell extensions: G04.2 (E3), G04.3 (E3), G04.5 (E3), G04.8 (E3), G04.22 (E3), G09.1 (E3), G10.4 (E3), S01 (E3), S05 (E3), S08 (E2), S09 (E4), S10 (E1), S31 (E3), S32 (E2).

Shared changes built with the layouts: G01.2, G02.5, G02.7, G02.8, G02.11, G03.3, G04.1, G04.10, G04.15, G04.25, G06.1, G06.2, G06.7, G07.1, G07.3, G12.4, G12.6, S03, S16, S20, S23, G04.32.

Shared changes that are each their own change: G04.7, G04.9, G04.13, G04.23, G06.4, G08.4, G11.1, G11.2, G12.9, G12.10, G04.31.

Waits for an owner: G12.8. Feature work: G01.4, G01.5, G04.4, G04.14, G08.6, G11.3, S24, S28, S33, S34.

All shared changes are adopted (decision D6). They apply in every layout, Classic included, because behaviour must not differ between layouts.

## 3. What exists to build on

From #262. The source files are identical at both epic tips; the end-to-end test file differs by four lines, where it finds the path-policy field:

- `web-ui/src/layouts/layoutCatalog.ts`: `LAYOUT_IDS`, `THEME_IDS`, `NAV_GROUP_IDS`, `PRIMARY_NAVIGATION`, the thirteen class slots per layout, `navPresentation`.
- `web-ui/src/components/Layout.tsx`: `LayoutFrame`, one element tree for every layout.
- `web-ui/src/layouts/layoutPreference.ts` and `LayoutChooser.tsx`: the per-account, per-site preference and its notices.
- `web-ui/src/layouts/layoutTokens.css`: the twelve shell tokens and the reserved theme selector.
- `web-ui/tests/layout-foundation.e2e.mjs` and `docs/reliability/contracts/layout-foundation.md`: the recorded checks and the contract, including its stated limits.

Shared components the options reuse: `Dialog`, `ConfirmModal`, `StatusObservationPanel`, `SyncStatus`, `ImportProgressCard`, `ImportProgressMeter`, `ImportModeFields`, `RepositorySaveNotice`, `ManagedRemovalPanel`, `ManagedRemovalReceiptNotice`, `BranchPairRemovalNotice`, `DiffViewer`, `ServerMonitor`, `ErrorAcknowledgement`, and on partial-52 `PathPolicyEditor56` and `CommitMapTargetCell`. State and API modules (`repositorySave.ts`, `statusObservation.ts`, `managedRemoval.ts`, `listPagination.ts`, `session.ts`, `api.ts`) are not touched by a layout.

## 4. Changes to the #262 shell

Five are accepted and one is withdrawn (decisions D1 to D5, D14, D15). Each accepted change amends a sentence in `layout-foundation.md`; whether it is technically acceptable as specified is the foundation owner's call.

What they do to Classic, Individual and Team: the “More” button (E1), the navigation icons (E6), the three derived page regions and the four group headings on the directory form are in the tree and hidden, so those layouts look and tab as they do today. The theme select (E2) is visible in every layout, because light is offered in all six. It adds one control to the account slot, where #262 records that Classic's bar already wraps between 1280 and 1490 px.

| Id | Change | Contract sentence it changes | Risk and its test |
|---|---|---|---|
| E1 | A “More” button that follows the last workspace link, in every layout, hidden by CSS where unused. Opening it reveals account and administration links, name, chooser and sign-out, and moves focus to the first link; Escape closes it | the fixed tree gains one element inside the link slot; it is not on the path to `main` | tab order must equal reading order at both sizes in all three options; the existing layouts must render and tab exactly as before |
| E2 | A theme select beside the layout select; `THEME_IDS` gains `light`; the theme is stored with the layout | the stored record gains a field, so its version changes | a record saved before the change must still select its layout, with theme dark; a wrong migration would drop saved layouts |
| E3 | One text catalogue with two vocabularies, for navigation entries, shell text such as “Logout”, and page labels. A layout names its vocabulary. The existing vocabulary is today's strings, byte for byte, apart from the three corrections in decision D11 | “labels … are unchanged in every layout” becomes “unchanged in Classic, Individual and Team apart from three listed corrections” | a snapshot of every string in the existing layouts must be identical before and after apart from those three; the acceptance lab (PR #195) is updated for them in the same change |
| E4 | A Help entry for `/docs` in Ledger, Console and Guide only | `PRIMARY_NAVIGATION` gains an entry limited to those layouts | Classic's bar must not gain an entry |
| E5 | Withdrawn. The sign-in page stays as it is | none | the sign-in page must be identical before and after, in every layout and theme |
| E6 | An optional icon for each navigation entry, rendered in every layout and hidden in the existing three | the link slot's entries gain one element each | Console's sidebar and the phone bottom bars need it; the existing layouts must not show it |

## 5. Order of work

Each step leaves the product working. Classic stays the default.

1. **Foundation changes E1 to E4 and E6**, with the existing three layouts proven unchanged apart from what section 4 lists.
2. **The three options' shell**: catalog entries, shell tokens for dark and light, and the arrangements in `options/<id>/option.css`.
3. **Colour**: the [colour bridge](colour-bridge.md). One configuration change, one stylesheet, edits to the three classes a rule cannot decide, and the four sources of colour that are not utility classes. After this step every page is legible in every layout and theme, and the existing layouts' light theme exists.
4. **Wording**: move page strings into the catalogue. The plain vocabulary is the right-hand column of the [crosswalk](../crosswalk/legacy-navigation-settings.md).
5. **Page regions**: the fixed region tree in [page-regions.md](page-regions.md), the dashboard and the repository page first, with `?section=`.
6. **Shared presentational classes**, route by route: section, table, state mark, banner, button, field, label and value list. This is what gives Ledger its rules, Console its density and Guide its cards.
7. **Shared changes built with the layouts** (section 2).
8. **Shared changes that are each their own change**, one review each.
9. **Qualification** across six layouts, two themes and two sizes, with the tests in section 9.

What changes for someone who stays on Classic, and nothing else: the shared changes (they are corrections and apply everywhere), three corrected labels (decision D11), and a Dark and Light choice beside the layout choice. Page order, tab order and everything else stay as they are.

## 6. What does not change

- **Routes.** `/login`, `/`, `/repos`, `/repos/:id`, `/conflicts`, `/conflicts/:id`, `/audit`, `/settings`, `/users`, `/ldap`, `/config`, `/setup`, `/docs`. Thirteen, identical at both tips. None is added, removed or renamed. `?section=` is a parameter on an existing route.
- **The API module.** No call is added by a layout. The shared changes that alter when an existing call is made are listed in section 2.
- **Permissions and roles.**
- **Classic as default and fallback**, with dark as its theme, and every label in the existing three layouts apart from the three corrections in D11.
- **The order of regions on every page**, in every layout (decision D12).
- **Safety wording**, in both vocabularies.
- **The sign-in page.**

## 7. The two epics

#264 names `epic/reposync-partial-52` as the first target. Read at `b2b055a72657824fd6662b451f69718d73405aa5`. The reliability epic was read at `5853a8884496d294ce21898415d7d2718b73bf7c`.

- The #262 source files and every page except the two below are byte-identical at the two tips, so the same layout work applies to both. `api.ts` differs: partial-52 adds the #56 policy types and the commit-map target state from #54.
- `pages/RepoDetail.tsx` differs: partial-52 has the #56 path-policy editor (`PathPolicyEditor56`); reliability-61 has two text areas. The option styles whichever is present. Row G04.6 draws both.
- `pages/Dashboard.tsx` and `pages/RepoDetail.tsx` differ in the commit-map target cell: partial-52 uses `CommitMapTargetCell`. Rows G02.8 and G04.16.
- A layout must not carry #56 code to the reliability epic or remove it from partial-52.
- The colour bridge was counted on partial-52. The class list on the reliability epic differs only where those two pages do.

## 8. Dependencies and owners

| Item | State | Effect on the options |
|---|---|---|
| #262 layout foundation | closed as completed 2026-11-15 | the base for everything here; its stated limits (Classic's bar wrapping between 1280 and 1490 px, Classic on a phone not measured, focus outline clipped in scrolling link strips) are untouched |
| #69 pair refresh and re-anchor | holds in force as far as the record shows | shown as held; no control that acts; those regions are restyled in place and not moved out of that owner's code |
| #73 conflict application | open | the conflict page says a decision is recorded, not applied |
| #56 path-policy forms (epic #52) | present on partial-52 only | styled, not redesigned |
| #63 credential origin (RS-11) | not done | credential origin is shown as not reported |
| #64 setup-wizard cancellation (RS-19) | follow-ups open | row G12.8 waits for that owner |
| #121 screen-reader runbook and usability check | not run | no option can claim an accessibility pass |
| #199 delete one side of a pair | separate backlog | not drawn |
| PR #195 acceptance lab | finds controls by label and flow | the existing vocabulary is frozen for this reason; the shared changes that add a step need its flows updated in the same change |

## 9. Test oracles

Checks #264 can automate. None has been run. Sizes are 1440×900 and 390×844.

### Every layout, existing and new

- With nothing saved: Classic, dark. The device's colour setting has no effect.
- A layout and a theme each survive a reload for the same account and site, and are not visible to another account in the same browser.
- An unknown, corrupt, foreign or unreadable preference falls back to Classic and dark with #262's notice text. A record saved before E2 still selects its layout, with theme dark.
- Switching layout or theme does not remount the routed page: a typed draft, an open dialog, the focused element and the scroll position are unchanged, and no request is sent.
- The sign-in page is identical to the build before the change, whatever was last chosen.
- Every token in the design-system contract has a value for the layout in both themes, and the computed pairs there and in the colour bridge meet their targets (unit tests on the token and bridge files).
- After the colour bridge, none of the three classes it cannot decide is left in the source.
- At both sizes the document is no wider than the window on every route, including with the longest synthetic repository name.
- An automated accessibility scan (`axe-core` is already a dependency) reports no new violation against Classic dark on the same route and state. A scan is not an accessibility pass.

### Classic, Individual and Team

- In dark, before the shared changes: the same visible elements and the same tab order as the build before the change on every route, apart from one addition, the theme select beside the layout select. Apart from that select and its effect on where the bar wraps, the pixels are the same. The “More” button, the navigation icons, the three derived regions and the four group headings on the directory form are in the tree and hidden.
- Every string identical to the build before the change (snapshot of the existing vocabulary), apart from the three corrected labels. The acceptance lab passes with those three updated.
- No Help entry, no visible “More” control and no visible navigation icons.
- In light: every route is legible, and the bridge's pair check passes for the existing-layouts light palette.

### Ledger, Console and Guide

- The chooser lists the three after the existing three.
- The shell tree is the #262 tree plus the E1 button, the E2 select and the E6 icons: same element types and order, `nav` labelled “Primary”, `main` with `tabindex="-1"`.
- An administrator reaches ten destinations; another account sees six and no Administration group. The active entry has `aria-current="page"`. The conflict count shows when there is one.
- Tab order through navigation equals reading order at both sizes: workspace links, “More” where shown, then account links, administration links, chooser, sign-out.
- Opening “More” sets `aria-expanded`, moves focus to the first revealed link, and Escape closes it and returns focus.
- Every focusable control in navigation shows its focus outline fully inside the window.
- On the dashboard and the repository page, regions are seen in the order they have in the document, at both sizes.
- The markup inside the routed page is the same whichever of the six layouts is chosen. The prototypes meet this for the three new options, and `tools/build.py` checks it.
- The shell status shows the health word the aggregate read returns, and switching layout sends no request.
- `?section=` shows the named section; an unknown value shows Overview; Back returns to the previous section; a draft in Settings survives a visit to Activity and a change of layout.
- **Ledger**: wide, workspace links and status on the first row and the rest on the second, with no “More”. At 860 px and below, the workspace links and “More” form the second row. Section headings carry a rule and no box; titles use the serif stack.
- **Console**: wide, a 244 px sticky sidebar with three group labels. At 900 px and below, a bottom bar with the four workspace links and “More”; the last line of every page scrolls clear of it. From 1101 px a page with sections keeps its status beside the content.
- **Guide**: four pill tabs with “Account and admin” at the end of the row at every width above 760 px; at 760 px and below a bottom bar with the four links and “More”. Controls are at least 42 px high.

### Shared changes

The matrix row is the oracle: its states column lists what must be reachable and its note says what differs from today. Each is tested in one existing layout and one new layout, since it is the same code.

## 10. Outside #264

- Showing why sign-in was reached after a session ended (G01.4), and the first-run refusal (G01.5).
- Credential origin and revocation (G04.4, S24).
- A resume control for a verified partial import (G04.14).
- A catch-all route for unknown addresses (S28).
- Redacting the Teams webhook on read (G11.3). It needs the server to stop returning the address, which issue #268's suggested direction does not cover.
- Stacking the rows of wide tables into blocks at narrow widths, with the same markup. This is an improvement for #264 to decide against the real application; the prototypes keep scroll frames with a shadow at the hidden edge.
