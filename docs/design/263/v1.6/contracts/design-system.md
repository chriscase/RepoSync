# Design-system contract

The visual rules for the three new options, written so #264 can build them on the #262 seam. This is a proposal. Nothing in it is in the product.

## 1. Scope and selectors

- Layout ids: `ledger`, `console`, `guide`. They join `classic`, `individual` and `team` in `LAYOUT_IDS`; those three are not changed.
- Theme ids: `dark` and `light`. Dark is the default in every layout and stays until a person chooses light. Today `THEME_IDS` holds only `dark`.
- Every token is declared on `.rs-layout-shell[data-rs-layout='<id>'][data-rs-theme='<theme>']`. #262 reserved `.rs-layout-shell[data-rs-theme='<id>']`; adding the layout to the selector is an extension of that shape. The token files in this package are `options/<id>/tokens.css`.
- The sign-in page is outside the shell and keeps one appearance, dark, as today (decision D5).

## 2. Shell tokens: the #262 seam

The twelve `--rs-shell-*` names are used exactly as #262 defined them (`web-ui/src/layouts/layoutTokens.css`). No name is added, renamed or given a new meaning.

| Token | Role | Classic, Individual, Team dark (unchanged) | Classic, Individual, Team light (proposed) | Ledger light | Ledger dark | Console light | Console dark | Guide light | Guide dark |
|---|---|---|---|---|---|---|---|---|---|
| `--rs-shell-canvas` | Page background behind the shell and the content | `#111827` | `#f3f4f6` | `#fbfbf8` | `#131417` | `#f4f6f8` | `#0f141a` | `#f2f4f8` | `#12151c` |
| `--rs-shell-chrome` | Navigation background | `#030712` | `#ffffff` | `#fbfbf8` | `#131417` | `#e9edf1` | `#0a0e13` | `#ffffff` | `#171b24` |
| `--rs-shell-border` | Navigation border | `#1f2937` | `#d1d5db` | `#16181d` | `#7d7b76` | `#c5ced6` | `#2a3644` | `#dbe0ea` | `#313848` |
| `--rs-shell-hover` | Link hover and quiet fills in navigation | `#374151` | `#e5e7eb` | `#e7e7df` | `#2a2d34` | `#dde3e9` | `#1b242e` | `#eaeef5` | `#252b3a` |
| `--rs-shell-text-strong` | Brand, active and hovered link text | `#ffffff` | `#111827` | `#16181d` | `#eceae4` | `#111820` | `#e6edf3` | `#131722` | `#eef1f7` |
| `--rs-shell-text-muted` | Resting link text | `#9ca3af` | `#4b5563` | `#4a4f59` | `#b4b2ab` | `#4a5764` | `#9aa7b4` | `#4b556a` | `#a6afc0` |
| `--rs-shell-accent` | Active link marker or fill | `#2563eb` | `#2563eb` | `#16181d` | `#eceae4` | `#0b6f7c` | `#2fb7c6` | `#2456e6` | `#7da2ff` |
| `--rs-shell-accent-text` | Text on the accent fill | `#ffffff` | `#ffffff` | `#ffffff` | `#131417` | `#ffffff` | `#04161a` | `#ffffff` | `#0b1020` |
| `--rs-shell-alert` | Conflict count background | `#dc2626` | `#b91c1c` | `#b3261e` | `#ff8a80` | `#b42318` | `#f47067` | `#b42318` | `#ff8f85` |
| `--rs-shell-alert-text` | Conflict count text | `#fee2e2` | `#ffffff` | `#ffffff` | `#2a0806` | `#ffffff` | `#1a0605` | `#ffffff` | `#2a0806` |
| `--rs-shell-notice-text` | Chooser notice text | `#fde68a` | `#92400e` | `#7a4f00` | `#e6b450` | `#7a4f00` | `#e3b341` | `#7a4f00` | `#e8b84e` |
| `--rs-shell-focus` | Focus ring in navigation | `#93c5fd` | `#1d4ed8` | `#1a4fa3` | `#8fb8ff` | `#0b6f7c` | `#6fd3de` | `#2456e6` | `#9bb8ff` |

## 3. Page tokens: the proposed extension

At both epic tips, page content is coloured by fixed Tailwind utility classes (about 2,000 uses of about 190 distinct colour classes in the non-test `.tsx` files, counted at `5853a88`), and the #262 contract says “Light rendering is not claimed”. A light theme therefore needs page colour to come from tokens. These are the names this design uses. The implementation map gives two ways to connect existing pages to them.

| Token | Role | Ledger light | Ledger dark | Console light | Console dark | Guide light | Guide dark |
|---|---|---|---|---|---|---|---|
| `--rs-bg` | Page background behind sections | `#fbfbf8` | `#131417` | `#f4f6f8` | `#0f141a` | `#f2f4f8` | `#12151c` |
| `--rs-surface` | Section, card, table and dialog background | `#ffffff` | `#1a1c20` | `#ffffff` | `#151c24` | `#ffffff` | `#1a1e28` |
| `--rs-surface-2` | Inset areas: banners, table stripes, read-only values | `#f3f3ee` | `#212329` | `#eef1f4` | `#1b242e` | `#f6f8fb` | `#212634` |
| `--rs-surface-3` | Pressed and selected fills, control tracks, loading placeholders | `#e7e7df` | `#2a2d34` | `#e2e7ec` | `#24303c` | `#e6eaf2` | `#2b3142` |
| `--rs-line` | Hairlines and dividers | `#d6d6cd` | `#353841` | `#cfd6dd` | `#2a3644` | `#dbe0ea` | `#313848` |
| `--rs-line-strong` | Control borders (3:1 against surface) | `#8f8f85` | `#6b6f7a` | `#7f8c99` | `#5a6b7d` | `#8792a6` | `#66708a` |
| `--rs-text` | Body text and headings | `#16181d` | `#eceae4` | `#111820` | `#e6edf3` | `#131722` | `#eef1f7` |
| `--rs-text-2` | Secondary text | `#3d424b` | `#c5c3bc` | `#36424e` | `#b7c2cd` | `#3b4354` | `#c3c9d6` |
| `--rs-text-3` | Muted text: times, help, captions | `#5a606b` | `#a09f98` | `#566371` | `#8f9caa` | `#586176` | `#9aa3b5` |
| `--rs-accent` | Primary button and selected control | `#16181d` | `#eceae4` | `#0b6f7c` | `#2fb7c6` | `#2456e6` | `#7da2ff` |
| `--rs-accent-text` | Text on accent | `#ffffff` | `#131417` | `#ffffff` | `#04161a` | `#ffffff` | `#0b1020` |
| `--rs-accent-soft` | Quiet accent fill | `#ecece5` | `#2a2d34` | `#d9eef1` | `#0f3038` | `#e3ebff` | `#1c2747` |
| `--rs-link` | Links | `#1a4fa3` | `#8fb8ff` | `#0b6370` | `#6fd3de` | `#1f4fd6` | `#9bb8ff` |
| `--rs-focus` | Focus ring | `#1a4fa3` | `#8fb8ff` | `#0b6f7c` | `#6fd3de` | `#2456e6` | `#9bb8ff` |
| `--rs-ok` | State: idle, completed, verified | `#17663c` | `#6fd39a` | `#1a7446` | `#4cc38a` | `#157040` | `#63d19a` |
| `--rs-run` | State: running, queued | `#1b5fa8` | `#7fb4f5` | `#1766b3` | `#5bb8f0` | `#1f4fd6` | `#7da2ff` |
| `--rs-warn` | State: paused, conflict, stale | `#855600` | `#e6b450` | `#855600` | `#e3b341` | `#855600` | `#e8b84e` |
| `--rs-held` | State: held, needs reconciliation | `#a3460a` | `#f0935a` | `#a3460a` | `#f0883e` | `#a3460a` | `#f29a62` |
| `--rs-danger` | State: error, failed; destructive outline | `#b3261e` | `#ff8a80` | `#b42318` | `#f47067` | `#b42318` | `#ff8f85` |
| `--rs-off` | State: disabled, unknown | `#5a606b` | `#a09f98` | `#566371` | `#8f9caa` | `#586176` | `#9aa3b5` |
| `--rs-ok-soft` | Tint behind an OK banner | `#e3f1e7` | `#16301f` | `#dff2e6` | `#12301f` | `#e0f3e7` | `#143023` |
| `--rs-run-soft` | Tint behind a running banner | `#e3edf8` | `#152a44` | `#e0ecf8` | `#10283b` | `#e3ebff` | `#1c2747` |
| `--rs-warn-soft` | Tint behind a warning banner | `#f8eed2` | `#3a2d0c` | `#f7edcf` | `#3b2f0b` | `#f8eed0` | `#3a2e0d` |
| `--rs-held-soft` | Tint behind a held banner | `#fbe6d6` | `#3d2212` | `#fbe5d5` | `#3d230f` | `#fbe6d6` | `#3e2413` |
| `--rs-danger-soft` | Tint behind an error banner | `#fbe4e1` | `#421a17` | `#fbe3e0` | `#46191a` | `#fbe3e0` | `#431b18` |
| `--rs-svn` | Mark for an SVN reference | `#1b5fa8` | `#7fb4f5` | `#1766b3` | `#6cb6ff` | `#1b66ab` | `#79b8f7` |
| `--rs-git` | Mark for a Git reference | `#6d3fc4` | `#c3a2ff` | `#6f42c1` | `#c297ff` | `#7438c9` | `#c7a4ff` |
| `--rs-code-bg` | Code, log and diff background | `#f1f1ec` | `#0e0f12` | `#eaeef2` | `#0a0e13` | `#eef1f6` | `#0d1016` |
| `--rs-code-text` | Code, log and diff text | `#16181d` | `#eceae4` | `#111820` | `#e6edf3` | `#131722` | `#eef1f7` |
| `--rs-add-bg` | Added line in a diff | `#dff3e4` | `#153220` | `#dbf1e2` | `#12301f` | `#dcf2e3` | `#143023` |
| `--rs-del-bg` | Removed line in a diff | `#fbe1de` | `#45201c` | `#fbe0dd` | `#46191a` | `#fbe0dd` | `#431b18` |
| `--rs-scrim` | Layer behind a dialog | `rgba(22,24,29,.45)` | `rgba(0,0,0,.6)` | `rgba(17,24,32,.45)` | `rgba(0,0,0,.62)` | `rgba(19,23,34,.45)` | `rgba(0,0,0,.6)` |
| `--rs-shadow` | Dialog and menu shadow | `0 8px 28px rgba(22,24,29,.18)` | `0 10px 32px rgba(0,0,0,.55)` | `0 8px 26px rgba(17,24,32,.2)` | `0 10px 30px rgba(0,0,0,.6)` | `0 12px 34px rgba(19,23,34,.16)` | `0 14px 36px rgba(0,0,0,.55)` |
| `--rs-danger-solid` | Filled destructive button | `#b3261e` | `#c9372c` | `#b42318` | `#c93c37` | `#b42318` | `#c93c37` |
| `--rs-danger-solid-text` | Text on the filled destructive button | `#ffffff` | `#ffffff` | `#ffffff` | `#ffffff` | `#ffffff` | `#ffffff` |

### Existing layouts in light

Classic, Individual and Team get a light theme (decisions 3 and 4, row S32). Their arrangement, labels and order do not change. Their page content reaches these values through the [colour bridge](colour-bridge.md); their dark theme stays exactly the Tailwind values pages use now.

| Token | Light value | Token | Light value | Token | Light value |
|---|---|---|---|---|---|
| `--rs-bg` | `#f3f4f6` | `--rs-surface` | `#ffffff` | `--rs-surface-2` | `#f3f4f6` |
| `--rs-surface-3` | `#e5e7eb` | `--rs-line` | `#e5e7eb` | `--rs-line-strong` | `#7f8a9a` |
| `--rs-text` | `#111827` | `--rs-text-2` | `#374151` | `--rs-text-3` | `#4b5563` |
| `--rs-accent` | `#2563eb` | `--rs-accent-text` | `#ffffff` | `--rs-accent-soft` | `#dbeafe` |
| `--rs-link` | `#1d4ed8` | `--rs-focus` | `#1d4ed8` | `--rs-ok` | `#15803d` |
| `--rs-run` | `#1d4ed8` | `--rs-warn` | `#925207` | `--rs-held` | `#b93c0c` |
| `--rs-danger` | `#b91c1c` | `--rs-off` | `#4b5563` | `--rs-ok-soft` | `#dcfce7` |
| `--rs-run-soft` | `#dbeafe` | `--rs-warn-soft` | `#fef9c3` | `--rs-held-soft` | `#ffedd5` |
| `--rs-danger-soft` | `#fee2e2` | `--rs-svn` | `#1d4ed8` | `--rs-git` | `#7e22ce` |
| `--rs-code-bg` | `#f3f4f6` | `--rs-code-text` | `#111827` | `--rs-add-bg` | `#dcfce7` |
| `--rs-del-bg` | `#fee2e2` | `--rs-scrim` | `rgba(17,24,39,.5)` | `--rs-shadow` | `0 10px 30px rgba(17,24,39,.18)` |
| `--rs-danger-solid` | `#b91c1c` | `--rs-danger-solid-text` | `#ffffff` | none | none |

Computed contrast for this light palette: 30 of 30 pairs meet their target.

An observation about today's dark palette, which this design does not change: computed on the Tailwind values the pages use now, muted text on inset is 4.06:1 against a target of 4.5:1; control border on surface is 1.94:1 against a target of 3.0:1; conflict count is 3.95:1 against a target of 4.5:1. The values are in [contrast-computed.csv](contrast-computed.csv) under “existing layouts”.

Rules for these tokens:

- A state colour is never the only signal. Every state is a mark, a word and a colour.
- `--rs-svn` and `--rs-git` mark which side a value belongs to. They always sit beside the word “SVN” or “Git” or a revision or hash in its own format.
- Tints (`*-soft`) carry text in `--rs-text`, with the state colour used for the mark and the title.

## 4. Type, spacing and density

No web fonts. Each option names system font stacks, so nothing is downloaded and the face differs by operating system.

| Token | Role | Ledger | Console | Guide |
|---|---|---|---|---|
| `--rs-font-ui` | Interface text | `system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif` | `"Segoe UI", system-ui, -apple-system, "Helvetica Neue", Arial, sans-serif` | `ui-rounded, "SF Pro Rounded", "Hiragino Maru Gothic ProN", "Segoe UI", system-ui, sans-serif` |
| `--rs-font-display` | Page titles and large values | `"Iowan Old Style", "Palatino Linotype", Palatino, "Book Antiqua", Georgia, serif` | `"Segoe UI", system-ui, -apple-system, "Helvetica Neue", Arial, sans-serif` | `ui-rounded, "SF Pro Rounded", "Hiragino Maru Gothic ProN", "Segoe UI", system-ui, sans-serif` |
| `--rs-font-mono` | Revisions, hashes, paths, logs | `ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace` | `ui-monospace, "Cascadia Mono", "SF Mono", Menlo, Consolas, monospace` | `ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace` |
| `--rs-display-weight` | Title weight | `600` | `650` | `700` |
| `--rs-fs` | Base size | `15px` | `13px` | `16px` |
| `--rs-lh` | Base line height | `1.55` | `1.45` | `1.5` |
| `--rs-h1` | Page title | `1.85rem` | `1.35rem` | `1.7rem` |
| `--rs-h2` | Section title | `1.15rem` | `0.98rem` | `1.12rem` |
| `--rs-radius` | Control corner | `2px` | `4px` | `10px` |
| `--rs-radius-lg` | Section and dialog corner | `2px` | `6px` | `16px` |
| `--rs-gap` | Space between related items | `12px` | `8px` | `12px` |
| `--rs-gap-lg` | Space between sections | `22px` | `14px` | `20px` |
| `--rs-pad` | Section padding | `18px` | `12px` | `20px` |
| `--rs-pad-sm` | Banner and card padding | `12px` | `9px` | `14px` |
| `--rs-cell` | Table cell padding | `9px 10px` | `5px 9px` | `12px 14px` |
| `--rs-control-h` | Control height | `34px` | `28px` | `42px` |
| `--rs-btn-pad` | Button side padding | `14px` | `10px` | `18px` |

- **Ledger** reads like a record: serif titles, 15 px text, rules between sections instead of boxes, square corners, ink as the only accent.
- **Console** is dense: 13 px text, 28 px controls, striped tables, uppercase section labels, a teal accent. It is the option for watching many repositories.
- **Guide** is roomy: 16 px text, 42 px controls, rounded sections and pill buttons, a blue accent close to Classic's. It is the option for people who visit now and then.
- Revisions, hashes, paths and operation ids are always in the monospace stack, in every option.

## 5. Responsive rules

| Rule | Ledger | Console | Guide |
|---|---|---|---|
| Navigation, wide | Two-row masthead: workspace links and status on the first row; account links, administration links, name, chooser and sign-out on the second | 244 px sticky sidebar with three labelled groups; status, name, chooser and sign-out at its foot | Sticky top bar: four workspace pill tabs with “Account and admin” at the end of the same row, then status |
| Changes to narrow at | 860 px | 900 px | 760 px |
| Navigation, narrow | First row: logo, name, status. Second row: the four workspace links and “More”, which opens account, administration, name, chooser and sign-out below | Top strip that stays in view; a fixed bottom bar holds the four workspace links and “More”, which opens the rest under the top strip | Top bar; a fixed bottom bar holds the four workspace links and “More” |
| Content width | Up to 1180 px, centred | Fills the space beside the sidebar | Up to 1080 px, centred |
| Page with sections, wide | Contents list at the left (from 861 px) | Tabs above, status inspector fixed at the right (from 1101 px) | Chips above the content |
| Page with sections, narrow | Section links wrap onto more lines, so the current one is always in view | Same | Same |

Rules shared by all three:

- The page never scrolls sideways. A table wider than the screen scrolls inside its own frame, which is focusable and labelled and shows a shadow at an edge where more is hidden. On a phone several tables scroll this way. Stacking each row into a block at narrow widths is recorded in the implementation map as an improvement for #264 to decide against the real application.
- Two-column arrangements become one column at 860 px. Label and value pairs stack at 480 px.
- Side padding is at least 16 px (12 px in Console) and respects the device's safe areas. A fixed bottom bar adds its own height to the bottom padding of the page and respects the bottom safe area.
- Dialogs are at most 560 px wide (wide dialogs 780 px) and never wider than the screen.
- Guide's controls are 42 px high. Bottom-bar targets are an icon over a label with padding and are meant to come to about 44 px; that was not measured. Console's 28 px controls are a deliberate density choice for pointer use, and on a phone Console keeps them; only its bottom bar has large targets.

## 6. Components and states

Every component below is drawn in all three options and both themes. “Reuses” names the shared component that exists today; where it says “pattern”, pages build it from plain elements today.

| Component | Variants and states | Reuses |
|---|---|---|
| Shell | wide, narrow; menu closed, open; administrator, non-administrator; conflict count absent, present | `LayoutFrame` in `components/Layout.tsx` |
| Layout and theme chooser | saved; “Using Classic” (two reasons); “Not saved” (five reasons); theme not offered by a layout | `layouts/LayoutChooser.tsx`, `layouts/layoutPreference.ts` |
| Page scaffold | title, breadcrumbs, facts, actions; status line; section navigation; body | pattern |
| Section | heading, actions, body, note | pattern |
| State mark | idle, running, initializing, busy, disabled, paused after error, conflict, held, error, unknown (dashed); normal and large | values from `statusObservation.ts` |
| Observed-time stamp | fresh, stale, unavailable | `components/StatusObservation.tsx` |
| Evidence block (“What this status proves”) | observed, stale, unavailable, malformed, held, restricted | `StatusObservationPanel` |
| Button | default, primary, warning, destructive outline, destructive filled, quiet, small; disabled; busy. A label ending in “…” opens a dialog | pattern |
| Segmented control | pressed, not pressed | pattern (status and view filters) |
| Row menu | closed, open; item disabled with its reason | pattern |
| Table | caption, sortless columns, striped (Console), ruled (Ledger), roomy (Guide); row expanded; empty; loading; failed. The same table markup in every option | pattern |
| Label and value list | two columns; stacked on a phone | `ConfigRow` pattern |
| Pager | first, middle, last, out of range (“Return to first page”) | `listPagination.ts` |
| Progress meter | measured, unmeasured, complete, failed, stopped | `components/ImportProgressMeter.tsx` |
| Operation card | queued, running, stop requested, stopped, failed, needs reconciliation, completed, held, unknown | `components/ImportProgressCard.tsx`, `StatusObservation.tsx` |
| Log | lines; empty | wizard operation log |
| Diff | SVN, Git, base panes; added and removed lines; binary (no diff) | `components/DiffViewer.tsx` |
| Form field | default, with help, error, disabled, read-only, secret (set, not set, replacing) | pattern, with the label and error association from #121 |
| Choice cards | selected, not selected, with dependent fields | `components/ImportModeFields.tsx` |
| Steps header | done, current, upcoming, blocked | `StepIndicator` in `pages/SetupWizard.tsx` |
| Banner | information, OK, running, warning, held, error; with actions; status or alert role | `RepositorySaveNotice`, `ManagedRemovalReceiptNotice`, `BranchPairRemovalNotice` |
| Consequence list | what stays, what goes | text in the removal and pause dialogs |
| Dialog | default, wide; busy (cannot be dismissed); error inside | `components/Dialog.tsx`, `components/ConfirmModal.tsx` |
| Empty state | nothing yet; nothing matches; nothing on this page | pattern |
| Loading | text line; placeholder blocks | pattern |

The grey “Prototype” bar, the dashed “States” boards and the dashed markers that read “needs …” are prototype scaffolding. They are not components of the design.

## 7. Status vocabulary

One mark and one word per health value, in every option and theme.

| Value | Word shown | Colour token |
|---|---|---|
| `idle` | Idle | `--rs-ok` |
| `running` | Running | `--rs-run` |
| `initializing` | Initializing | `--rs-run` |
| `busy` | Busy | `--rs-run` |
| `disabled` | Disabled | `--rs-off` |
| `paused` | Paused after error | `--rs-warn` |
| `conflict` | Conflict | `--rs-warn` |
| `held` | Held | `--rs-held` |
| `error` | Error | `--rs-danger` |
| `unknown` | Unknown | `--rs-off (dashed outline)` |

A stale or unavailable observation is shown on the time stamp, not by changing the state word: the last known state stays visible with “stale” beside its time. An unknown value from the server is shown as “Unknown” and blocks actions.

## 8. Keyboard and focus

- Everything that acts is a native link, button, input or select. There are no click handlers on plain containers.
- Focus is a 2 px outline in `--rs-focus` (`--rs-shell-focus` in navigation) with a 2 px offset, on `:focus-visible`. It is never removed.
- Tab order is document order. The shell's document order is the #262 tree: brand, workspace links, account links, administration links, chooser, sign-out.
- `main` keeps `tabindex="-1"` so a skip target exists, as #262 built it.
- A dialog takes focus on opening, traps it, closes on Escape unless it is busy, and returns focus to the control that opened it. This is the existing `Dialog` behaviour.
- The “More” control follows the last workspace link in the document, so tab order equals reading order at every width: workspace links, “More”, then what it opens. It states whether it is open (`aria-expanded`) and names what it controls. Opening it moves focus to the first link it reveals; Escape closes it and returns focus to it.
- Sideways-scrolling regions (wide tables, section rows on a phone) are focusable and labelled, so a keyboard can scroll them.
- Motion: transitions are removed under “reduce motion”. In forced-colours mode, borders and marks use system colours.

One known limit, for #264 to check in the real shell:

- **Flattened containers.** Ledger and Guide flatten the brand, links and account containers (`display: contents`) so their children can be placed on two rows. Some browsers have dropped semantics for flattened elements in the past. The three containers carry no role of their own; the groups inside them keep `role="group"` and are not flattened. This still needs checking in real browsers. On a phone in Console and Guide, “More” is in the bottom bar and the links it opens appear under the top bar; focus is moved there, and the distance between the two should be judged on a real device.

## 9. Contrast

Computed from the token values by `tools/tokens.py`: 180 pairs (30 pairs, three options, two themes). All 180 meet their target: 4.5:1 for text and state colours, 3:1 for control borders and focus rings. The full list is [contrast-computed.csv](contrast-computed.csv).

| Option | Theme | Pair closest to its target | Ratio | Target |
|---|---|---|---|---|
| Ledger | light | Control border on surface | 3.26:1 | 3.0:1 |
| Ledger | dark | Control border on surface | 3.4:1 | 3.0:1 |
| Console | light | Control border on surface | 3.43:1 | 3.0:1 |
| Console | dark | Control border on surface | 3.13:1 | 3.0:1 |
| Guide | light | Control border on surface | 3.14:1 | 3.0:1 |
| Guide | dark | Control border on surface | 3.37:1 | 3.0:1 |

This is arithmetic on declared colours. It is not a measurement of rendered pages, it does not cover text over tints that were not listed, and it is not an accessibility result.

## 10. Theme rules

- **Dark until chosen.** With nothing saved, every layout is dark. The device setting is not followed.
- **Light is offered in all six layouts.** Ledger, Console and Guide each have a complete light and dark theme. Classic, Individual and Team gain the light palette above.
- The theme is saved with the layout, per account and site. An account that saved a layout before this change keeps it and is dark.
- The sign-in page is dark for everyone.
- A theme changes colour only. It never changes layout, spacing, wording or which controls exist.
- Diffs, logs and code follow the theme. Today page content is dark, with two exceptions that render light: the diff viewer and the documentation page. The colour bridge lists them with the other colour that does not come from utility classes.

## 11. What this contract does not settle

- Rendered appearance. These pages were not opened in a browser where they were written (see the package index). Sizes and breakpoints are as authored.
- Print styles, right-to-left layout and translated text length. None exists in the product today and none is designed here.
- Icons. The prototypes use a small inline set drawn for this package. #264 can keep them or map them to whatever the product adopts.
