"""Prose documents for the #263 package: contracts, option READMEs and the package index."""
import csv
import io

import build
import kit
import matrix_data as M
import resolve as Z
import tokens

OPTS = list(kit.OPTIONS)
LEGACY_SHELL = ["#111827", "#030712", "#1f2937", "#374151", "#ffffff", "#9ca3af", "#2563eb", "#ffffff", "#dc2626", "#fee2e2", "#fde68a", "#93c5fd"]
SHELL_ROLE = {
    "canvas": "Page background behind the shell and the content", "chrome": "Navigation background", "border": "Navigation border",
    "hover": "Link hover and quiet fills in navigation", "text-strong": "Brand, active and hovered link text", "text-muted": "Resting link text",
    "accent": "Active link marker or fill", "accent-text": "Text on the accent fill", "alert": "Conflict count background",
    "alert-text": "Conflict count text", "notice-text": "Chooser notice text", "focus": "Focus ring in navigation"}
PAGE_ROLE = {
    "bg": "Page background behind sections", "surface": "Section, card, table and dialog background", "surface-2": "Inset areas: banners, table stripes, read-only values",
    "surface-3": "Pressed and selected fills, control tracks, loading placeholders", "line": "Hairlines and dividers", "line-strong": "Control borders (3:1 against surface)",
    "text": "Body text and headings", "text-2": "Secondary text", "text-3": "Muted text: times, help, captions", "accent": "Primary button and selected control",
    "accent-text": "Text on accent", "accent-soft": "Quiet accent fill", "link": "Links", "focus": "Focus ring",
    "ok": "State: idle, completed, verified", "run": "State: running, queued", "warn": "State: paused, conflict, stale", "held": "State: held, needs reconciliation",
    "danger": "State: error, failed; destructive outline", "off": "State: disabled, unknown", "ok-soft": "Tint behind an OK banner", "run-soft": "Tint behind a running banner",
    "warn-soft": "Tint behind a warning banner", "held-soft": "Tint behind a held banner", "danger-soft": "Tint behind an error banner",
    "svn": "Mark for an SVN reference", "git": "Mark for a Git reference", "code-bg": "Code, log and diff background", "code-text": "Code, log and diff text",
    "add-bg": "Added line in a diff", "del-bg": "Removed line in a diff", "scrim": "Layer behind a dialog", "shadow": "Dialog and menu shadow",
    "danger-solid": "Filled destructive button", "danger-solid-text": "Text on the filled destructive button"}
TYPE_ROLE = {"font-ui": "Interface text", "font-display": "Page titles and large values", "font-mono": "Revisions, hashes, paths, logs", "display-weight": "Title weight",
             "fs": "Base size", "lh": "Base line height", "h1": "Page title", "h2": "Section title", "radius": "Control corner", "radius-lg": "Section and dialog corner",
             "gap": "Space between related items", "gap-lg": "Space between sections", "pad": "Section padding", "pad-sm": "Banner and card padding", "cell": "Table cell padding",
             "control-h": "Control height", "btn-pad": "Button side padding"}
DELIVERY = {
    "layout only": "The option's shell arrangement, tokens and page arrangement deliver it.",
    "shell extension": "Needs one of the accepted changes to the #262 shell (E1 to E4, E6). Rows tagged E3 are labels that differ in the plain vocabulary.",
    "shared change, with the layouts": "An improvement to shared page code that only changes what is displayed. Built with the layouts; appears in every layout.",
    "shared change, own change": "An improvement that changes when a call is made, adds a step or stops a write. Same in every layout; its own review and tests.",
    "waits for an owner": "Belongs to an existing owner's open work. The options style what exists until that owner changes it.",
    "feature work": "Needs behaviour or server data that does not exist. A separately owned dependency, outside #264."}
LAYOUT_ONLY_P = {"G13.0", "S06", "G04.29"}
SHELL_EXT = {"S01": "E3", "S31": "E3", "S08": "E2", "S32": "E2", "S09": "E4", "S10": "E1", "S05": "E3", "G04.2": "E3", "G04.3": "E3", "G04.5": "E3", "G04.8": "E3", "G04.22": "E3", "G09.1": "E3", "G10.4": "E3"}
SHARED_OWN = {"G04.7", "G04.9", "G04.13", "G04.23", "G04.31", "G06.4", "G08.4", "G11.1", "G11.2", "G12.9", "G12.10"}
SHARED_WAIT = {"G12.8"}


# rows whose defect is recorded but for which the page draws no correction: the option styles what exists
DEFECT_ONLY = {"G12.0", "G12.1", "G12.2", "G12.3", "G12.5", "G12.7", "S22", "G08.2", "S25"}


def delivery(r):
    if r["id"] in DEFECT_ONLY:
        return "layout only"
    if r["id"] in SHELL_EXT:
        return "shell extension"
    if r["id"] in SHARED_WAIT:
        return "waits for an owner"
    if r["kind"] == M.N:
        return "feature work"
    if r["kind"] in (M.D, M.P):
        if r["id"] in LAYOUT_ONLY_P:
            return "layout only"
        return "shared change, own change" if r["id"] in SHARED_OWN else "shared change, with the layouts"
    return "layout only"


def tok(o, t, name):
    d = tokens.T[o][t]
    return d[name] if name in d else d["shell"][tokens.SHELL.index(name)]


def contrast_csv():
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\n")
    w.writerow(["option", "theme", "pair", "foreground_token", "background_token", "ratio", "target", "meets_target"])
    for o, t, name, fg, bg, ratio, target, ok in tokens.contrast_rows() + tokens.legacy_contrast_rows():
        w.writerow([o, t, name, fg, bg, ratio, target, "yes" if ok else "no"])
    return buf.getvalue()


# ------------------------------------------------------------------ design system
def design_system():
    rows = tokens.contrast_rows()
    md = ["# Design-system contract", "",
          "The visual rules for the three new options, written so #264 can build them on the #262 seam. This is a proposal. Nothing in it is in the product.", "",
          "## 1. Scope and selectors", "",
          "- Layout ids: `ledger`, `console`, `guide`. They join `classic`, `individual` and `team` in `LAYOUT_IDS`; those three are not changed.",
          "- Theme ids: `dark` and `light`. Dark is the default in every layout and stays until a person chooses light. Today `THEME_IDS` holds only `dark`.",
          "- Every token is declared on `.rs-layout-shell[data-rs-layout='<id>'][data-rs-theme='<theme>']`. #262 reserved `.rs-layout-shell[data-rs-theme='<id>']`; adding the layout to the selector is an extension of that shape. "
          "The token files in this package are `options/<id>/tokens.css`.",
          "- The sign-in page is outside the shell and keeps one appearance, dark, as today (decision D5).", "",
          "## 2. Shell tokens: the #262 seam", "",
          "The twelve `--rs-shell-*` names are used exactly as #262 defined them (`web-ui/src/layouts/layoutTokens.css`). No name is added, renamed or given a new meaning.", "",
          "| Token | Role | Classic, Individual, Team dark (unchanged) | Classic, Individual, Team light (proposed) | Ledger light | Ledger dark | Console light | Console dark | Guide light | Guide dark |", "|---|---|---|---|---|---|---|---|---|---|"]
    for i, n in enumerate(tokens.SHELL):
        md.append(f"| `--rs-shell-{n}` | {SHELL_ROLE[n]} | `{LEGACY_SHELL[i]}` | `{tokens.LEGACY['light']['shell'][i]}` | " + " | ".join(f"`{tokens.T[o][t]['shell'][i]}`" for o in OPTS for t in ("light", "dark")) + " |")
    md += ["", "## 3. Page tokens: the proposed extension", "",
           "At both epic tips, page content is coloured by fixed Tailwind utility classes (about 2,000 uses of about 190 distinct colour classes in the non-test `.tsx` files, counted at "
           f"`{Z.R[:7]}`), and the #262 contract says “Light rendering is not claimed”. A light theme therefore needs page colour to come from tokens. "
           "These are the names this design uses. The implementation map gives two ways to connect existing pages to them.", "",
           "| Token | Role | Ledger light | Ledger dark | Console light | Console dark | Guide light | Guide dark |", "|---|---|---|---|---|---|---|---|"]
    for n in PAGE_ROLE:
        md.append(f"| `--rs-{n}` | {PAGE_ROLE[n]} | " + " | ".join(f"`{tokens.T[o][t][n]}`" for o in OPTS for t in ("light", "dark")) + " |")
    lrows = tokens.legacy_contrast_rows()
    below = [r for r in lrows if r[1] == "dark" and not r[7]]
    md += ["", "### Existing layouts in light", "",
           "Classic, Individual and Team get a light theme (decisions 3 and 4, row S32). Their arrangement, labels and order do not change. "
           "Their page content reaches these values through the [colour bridge](colour-bridge.md); their dark theme stays exactly the Tailwind values pages use now.", "",
           "| Token | Light value | Token | Light value | Token | Light value |", "|---|---|---|---|---|---|"]
    names = [n for n in PAGE_ROLE]
    for k in range(0, len(names), 3):
        md.append("| " + " | ".join(f"`--rs-{n}` | `{tokens.LEGACY['light'][n]}`" for n in names[k:k + 3]) + (" | none | none" * (3 - len(names[k:k + 3]))) + " |")
    md += ["", f"Computed contrast for this light palette: {sum(1 for r in lrows if r[1] == 'light' and r[7])} of {sum(1 for r in lrows if r[1] == 'light')} pairs meet their target.", "",
           "An observation about today's dark palette, which this design does not change: computed on the Tailwind values the pages use now, "
           + "; ".join(f"{r[2].lower()} is {r[5]}:1 against a target of {r[6]}:1" for r in below) + ". The values are in [contrast-computed.csv](contrast-computed.csv) under “existing layouts”.", "",
           "Rules for these tokens:", "",
           "- A state colour is never the only signal. Every state is a mark, a word and a colour.",
           "- `--rs-svn` and `--rs-git` mark which side a value belongs to. They always sit beside the word “SVN” or “Git” or a revision or hash in its own format.",
           "- Tints (`*-soft`) carry text in `--rs-text`, with the state colour used for the mark and the title.", "",
           "## 4. Type, spacing and density", "",
           "No web fonts. Each option names system font stacks, so nothing is downloaded and the face differs by operating system.", "",
           "| Token | Role | Ledger | Console | Guide |", "|---|---|---|---|---|"]
    for n in TYPE_ROLE:
        md.append(f"| `--rs-{n}` | {TYPE_ROLE[n]} | " + " | ".join(f"`{tokens.T[o]['type'][n]}`" for o in OPTS) + " |")
    md += ["", "- **Ledger** reads like a record: serif titles, 15 px text, rules between sections instead of boxes, square corners, ink as the only accent.",
           "- **Console** is dense: 13 px text, 28 px controls, striped tables, uppercase section labels, a teal accent. It is the option for watching many repositories.",
           "- **Guide** is roomy: 16 px text, 42 px controls, rounded sections and pill buttons, a blue accent close to Classic's. It is the option for people who visit now and then.",
           "- Revisions, hashes, paths and operation ids are always in the monospace stack, in every option.", "",
           "## 5. Responsive rules", "",
           "| Rule | Ledger | Console | Guide |", "|---|---|---|---|",
           "| Navigation, wide | Two-row masthead: workspace links and status on the first row; account links, administration links, name, chooser and sign-out on the second | 244 px sticky sidebar with three labelled groups; status, name, chooser and sign-out at its foot | Sticky top bar: four workspace pill tabs with “Account and admin” at the end of the same row, then status |",
           "| Changes to narrow at | 860 px | 900 px | 760 px |",
           "| Navigation, narrow | First row: logo, name, status. Second row: the four workspace links and “More”, which opens account, administration, name, chooser and sign-out below | Top strip that stays in view; a fixed bottom bar holds the four workspace links and “More”, which opens the rest under the top strip | Top bar; a fixed bottom bar holds the four workspace links and “More” |",
           "| Content width | Up to 1180 px, centred | Fills the space beside the sidebar | Up to 1080 px, centred |",
           "| Page with sections, wide | Contents list at the left (from 861 px) | Tabs above, status inspector fixed at the right (from 1101 px) | Chips above the content |",
           "| Page with sections, narrow | Section links wrap onto more lines, so the current one is always in view | Same | Same |", "",
           "Rules shared by all three:", "",
           "- The page never scrolls sideways. A table wider than the screen scrolls inside its own frame, which is focusable and labelled and shows a shadow at an edge where more is hidden. "
           "On a phone several tables scroll this way. Stacking each row into a block at narrow widths is recorded in the implementation map as an improvement for #264 to decide against the real application.",
           "- Two-column arrangements become one column at 860 px. Label and value pairs stack at 480 px.",
           "- Side padding is at least 16 px (12 px in Console) and respects the device's safe areas. A fixed bottom bar adds its own height to the bottom padding of the page and respects the bottom safe area.",
           "- Dialogs are at most 560 px wide (wide dialogs 780 px) and never wider than the screen.",
           "- Guide's controls are 42 px high. Bottom-bar targets are an icon over a label with padding and are meant to come to about 44 px; that was not measured. "
           "Console's 28 px controls are a deliberate density choice for pointer use, and on a phone Console keeps them; only its bottom bar has large targets.", "",
           "## 6. Components and states", "",
           "Every component below is drawn in all three options and both themes. “Reuses” names the shared component that exists today; where it says “pattern”, pages build it from plain elements today.", "",
           "| Component | Variants and states | Reuses |", "|---|---|---|",
           "| Shell | wide, narrow; menu closed, open; administrator, non-administrator; conflict count absent, present | `LayoutFrame` in `components/Layout.tsx` |",
           "| Layout and theme chooser | saved; “Using Classic” (two reasons); “Not saved” (five reasons); theme not offered by a layout | `layouts/LayoutChooser.tsx`, `layouts/layoutPreference.ts` |",
           "| Page scaffold | title, breadcrumbs, facts, actions; status line; section navigation; body | pattern |",
           "| Section | heading, actions, body, note | pattern |",
           "| State mark | idle, running, initializing, busy, disabled, paused after error, conflict, held, error, unknown (dashed); normal and large | values from `statusObservation.ts` |",
           "| Observed-time stamp | fresh, stale, unavailable | `components/StatusObservation.tsx` |",
           "| Evidence block (“What this status proves”) | observed, stale, unavailable, malformed, held, restricted | `StatusObservationPanel` |",
           "| Button | default, primary, warning, destructive outline, destructive filled, quiet, small; disabled; busy. A label ending in “…” opens a dialog | pattern |",
           "| Segmented control | pressed, not pressed | pattern (status and view filters) |",
           "| Row menu | closed, open; item disabled with its reason | pattern |",
           "| Table | caption, sortless columns, striped (Console), ruled (Ledger), roomy (Guide); row expanded; empty; loading; failed. The same table markup in every option | pattern |",
           "| Label and value list | two columns; stacked on a phone | `ConfigRow` pattern |",
           "| Pager | first, middle, last, out of range (“Return to first page”) | `listPagination.ts` |",
           "| Progress meter | measured, unmeasured, complete, failed, stopped | `components/ImportProgressMeter.tsx` |",
           "| Operation card | queued, running, stop requested, stopped, failed, needs reconciliation, completed, held, unknown | `components/ImportProgressCard.tsx`, `StatusObservation.tsx` |",
           "| Log | lines; empty | wizard operation log |",
           "| Diff | SVN, Git, base panes; added and removed lines; binary (no diff) | `components/DiffViewer.tsx` |",
           "| Form field | default, with help, error, disabled, read-only, secret (set, not set, replacing) | pattern, with the label and error association from #121 |",
           "| Choice cards | selected, not selected, with dependent fields | `components/ImportModeFields.tsx` |",
           "| Steps header | done, current, upcoming, blocked | `StepIndicator` in `pages/SetupWizard.tsx` |",
           "| Banner | information, OK, running, warning, held, error; with actions; status or alert role | `RepositorySaveNotice`, `ManagedRemovalReceiptNotice`, `BranchPairRemovalNotice` |",
           "| Consequence list | what stays, what goes | text in the removal and pause dialogs |",
           "| Dialog | default, wide; busy (cannot be dismissed); error inside | `components/Dialog.tsx`, `components/ConfirmModal.tsx` |",
           "| Empty state | nothing yet; nothing matches; nothing on this page | pattern |",
           "| Loading | text line; placeholder blocks | pattern |", "",
           "The grey “Prototype” bar, the dashed “States” boards and the dashed markers that read “needs …” are prototype scaffolding. They are not components of the design.", "",
           "## 7. Status vocabulary", "",
           "One mark and one word per health value, in every option and theme.", "", "| Value | Word shown | Colour token |", "|---|---|---|"]
    tone = {"st-ok": "--rs-ok", "st-run": "--rs-run", "st-off": "--rs-off", "st-warn": "--rs-warn", "st-held": "--rs-held", "st-err": "--rs-danger", "st-unk": "--rs-off (dashed outline)"}
    for k, (cls, _ic, label) in kit.STATES.items():
        md.append(f"| `{k}` | {label} | `{tone[cls]}` |")
    md += ["", "A stale or unavailable observation is shown on the time stamp, not by changing the state word: the last known state stays visible with “stale” beside its time. "
           "An unknown value from the server is shown as “Unknown” and blocks actions.", "",
           "## 8. Keyboard and focus", "",
           "- Everything that acts is a native link, button, input or select. There are no click handlers on plain containers.",
           "- Focus is a 2 px outline in `--rs-focus` (`--rs-shell-focus` in navigation) with a 2 px offset, on `:focus-visible`. It is never removed.",
           "- Tab order is document order. The shell's document order is the #262 tree: brand, workspace links, account links, administration links, chooser, sign-out.",
           "- `main` keeps `tabindex=\"-1\"` so a skip target exists, as #262 built it.",
           "- A dialog takes focus on opening, traps it, closes on Escape unless it is busy, and returns focus to the control that opened it. This is the existing `Dialog` behaviour.",
           "- The “More” control follows the last workspace link in the document, so tab order equals reading order at every width: workspace links, “More”, then what it opens. "
           "It states whether it is open (`aria-expanded`) and names what it controls. Opening it moves focus to the first link it reveals; Escape closes it and returns focus to it.",
           "- Sideways-scrolling regions (wide tables, section rows on a phone) are focusable and labelled, so a keyboard can scroll them.",
           "- Motion: transitions are removed under “reduce motion”. In forced-colours mode, borders and marks use system colours.", "",
           "One known limit, for #264 to check in the real shell:", "",
           "- **Flattened containers.** Ledger and Guide flatten the brand, links and account containers (`display: contents`) so their children can be placed on two rows. "
           "Some browsers have dropped semantics for flattened elements in the past. The three containers carry no role of their own; the groups inside them keep `role=\"group\"` and are not flattened. This still needs checking in real browsers. "
           "On a phone in Console and Guide, “More” is in the bottom bar and the links it opens appear under the top bar; focus is moved there, and the distance between the two should be judged on a real device.", "",
           "## 9. Contrast", "",
           f"Computed from the token values by `tools/tokens.py`: {len(rows)} pairs (30 pairs, three options, two themes). "
           f"All {sum(1 for r in rows if r[7])} meet their target: 4.5:1 for text and state colours, 3:1 for control borders and focus rings. "
           "The full list is [contrast-computed.csv](contrast-computed.csv).", "",
           "| Option | Theme | Pair closest to its target | Ratio | Target |", "|---|---|---|---|---|"]
    for o in OPTS:
        for t in ("light", "dark"):
            best = min((r for r in rows if r[0] == o and r[1] == t), key=lambda r: r[5] - r[6])
            md.append(f"| {kit.OPTIONS[o]['name']} | {t} | {best[2]} | {best[5]}:1 | {best[6]}:1 |")
    md += ["", "This is arithmetic on declared colours. It is not a measurement of rendered pages, it does not cover text over tints that were not listed, and it is not an accessibility result.", "",
           "## 10. Theme rules", "",
           "- **Dark until chosen.** With nothing saved, every layout is dark. The device setting is not followed.",
           "- **Light is offered in all six layouts.** Ledger, Console and Guide each have a complete light and dark theme. Classic, Individual and Team gain the light palette above.",
           "- The theme is saved with the layout, per account and site. An account that saved a layout before this change keeps it and is dark.",
           "- The sign-in page is dark for everyone.",
           "- A theme changes colour only. It never changes layout, spacing, wording or which controls exist.",
           "- Diffs, logs and code follow the theme. Today page content is dark, with two exceptions that render light: the diff viewer and the documentation page. "
           "The colour bridge lists them with the other colour that does not come from utility classes.", "",
           "## 11. What this contract does not settle", "",
           "- Rendered appearance. These pages were not opened in a browser where they were written (see the package index). Sizes and breakpoints are as authored.",
           "- Print styles, right-to-left layout and translated text length. None exists in the product today and none is designed here.",
           "- Icons. The prototypes use a small inline set drawn for this package. #264 can keep them or map them to whatever the product adopts."]
    return "\n".join(md) + "\n"


# ------------------------------------------------------------------ interaction
def interaction():
    md = """# Interaction contract

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
"""
    return md


# ------------------------------------------------------------------ implementation map
def implementation_map(R):
    by = {k: [r["id"] for r in R if delivery(r) == k] for k in DELIVERY}
    md = ["# Implementation map for #264", "",
          "What #264 builds, on what, in what order, and how to tell it worked. Written for the implementation owner, who knows the code and decides how.", "",
          "**Accepted on 2026-11-16: Ledger, Console and Guide, all three, to the full extent the prototypes show.** See [DECISIONS.md](../DECISIONS.md). "
          "A delivered design package is still not implemented acceptance, and #263 cannot close #264.", "",
          "## 1. What #264 asks to be recorded first, and where it is", "",
          "| #264 asks for | In this package |", "|---|---|",
          "| Accepted design location and version | this tree, `v1.6`; the Artifact URL is in the designer's hand-off message |",
          "| Checksum manifest | `SHA256SUMS` at the package root |",
          "| Reviewed contract version | the files in `contracts/`, version `v1.6` |",
          "| Option ids | `ledger`, `console`, `guide` |",
          "| Theme ids | `dark` (default) and `light`, in all six layouts |",
          "| Source provenance | the package index, section “Provenance” |", "",
          "## 2. Kinds of work", "",
          "Each matrix row carries one of these in the column `delivery_for_264`.", "",
          "| Kind | Meaning | Rows |", "|---|---|---|"]
    for k, text in DELIVERY.items():
        md.append(f"| **{k}** | {text} | {len(by[k])} |")
    md += ["", "Shell extensions: " + ", ".join(f"{i} ({SHELL_EXT[i]})" for i in by["shell extension"]) + ".", "",
           f"Shared changes built with the layouts: {', '.join(by['shared change, with the layouts'])}.", "",
           f"Shared changes that are each their own change: {', '.join(by['shared change, own change'])}.", "",
           f"Waits for an owner: {', '.join(by['waits for an owner'])}. Feature work: {', '.join(by['feature work'])}.", "",
           "All shared changes are adopted (decision D6). They apply in every layout, Classic included, because behaviour must not differ between layouts.", "",
           "## 3. What exists to build on", "",
           "From #262. The source files are identical at both epic tips; the end-to-end test file differs by four lines, where it finds the path-policy field:", "",
           "- `web-ui/src/layouts/layoutCatalog.ts`: `LAYOUT_IDS`, `THEME_IDS`, `NAV_GROUP_IDS`, `PRIMARY_NAVIGATION`, the thirteen class slots per layout, `navPresentation`.",
           "- `web-ui/src/components/Layout.tsx`: `LayoutFrame`, one element tree for every layout.",
           "- `web-ui/src/layouts/layoutPreference.ts` and `LayoutChooser.tsx`: the per-account, per-site preference and its notices.",
           "- `web-ui/src/layouts/layoutTokens.css`: the twelve shell tokens and the reserved theme selector.",
           "- `web-ui/tests/layout-foundation.e2e.mjs` and `docs/reliability/contracts/layout-foundation.md`: the recorded checks and the contract, including its stated limits.", "",
           "Shared components the options reuse: `Dialog`, `ConfirmModal`, `StatusObservationPanel`, `SyncStatus`, `ImportProgressCard`, `ImportProgressMeter`, `ImportModeFields`, "
           "`RepositorySaveNotice`, `ManagedRemovalPanel`, `ManagedRemovalReceiptNotice`, `BranchPairRemovalNotice`, `DiffViewer`, `ServerMonitor`, `ErrorAcknowledgement`, "
           "and on partial-52 `PathPolicyEditor56` and `CommitMapTargetCell`. State and API modules (`repositorySave.ts`, `statusObservation.ts`, `managedRemoval.ts`, `listPagination.ts`, `session.ts`, `api.ts`) are not touched by a layout.", "",
           "## 4. Changes to the #262 shell", "",
           "Five are accepted and one is withdrawn (decisions D1 to D5, D14, D15). Each accepted change amends a sentence in `layout-foundation.md`; whether it is technically acceptable as specified is the foundation owner's call.", "",
           "What they do to Classic, Individual and Team: the “More” button (E1), the navigation icons (E6), the three derived page regions and the four group headings on the directory form are in the tree and hidden, so those layouts look and tab as they do today. "
           "The theme select (E2) is visible in every layout, because light is offered in all six. It adds one control to the account slot, where #262 records that Classic's bar already wraps between 1280 and 1490 px.", "",
           "| Id | Change | Contract sentence it changes | Risk and its test |", "|---|---|---|---|",
           "| E1 | A “More” button that follows the last workspace link, in every layout, hidden by CSS where unused. Opening it reveals account and administration links, name, chooser and sign-out, and moves focus to the first link; Escape closes it | the fixed tree gains one element inside the link slot; it is not on the path to `main` | tab order must equal reading order at both sizes in all three options; the existing layouts must render and tab exactly as before |",
           "| E2 | A theme select beside the layout select; `THEME_IDS` gains `light`; the theme is stored with the layout | the stored record gains a field, so its version changes | a record saved before the change must still select its layout, with theme dark; a wrong migration would drop saved layouts |",
           "| E3 | One text catalogue with two vocabularies, for navigation entries, shell text such as “Logout”, and page labels. A layout names its vocabulary. The existing vocabulary is today's strings, byte for byte, apart from the three corrections in decision D11 | “labels … are unchanged in every layout” becomes “unchanged in Classic, Individual and Team apart from three listed corrections” | a snapshot of every string in the existing layouts must be identical before and after apart from those three; the acceptance lab (PR #195) is updated for them in the same change |",
           "| E4 | A Help entry for `/docs` in Ledger, Console and Guide only | `PRIMARY_NAVIGATION` gains an entry limited to those layouts | Classic's bar must not gain an entry |",
           "| E5 | Withdrawn. The sign-in page stays as it is | none | the sign-in page must be identical before and after, in every layout and theme |",
           "| E6 | An optional icon for each navigation entry, rendered in every layout and hidden in the existing three | the link slot's entries gain one element each | Console's sidebar and the phone bottom bars need it; the existing layouts must not show it |", "",
           "## 5. Order of work", "",
           "Each step leaves the product working. Classic stays the default.", "",
           "1. **Foundation changes E1 to E4 and E6**, with the existing three layouts proven unchanged apart from what section 4 lists.",
           "2. **The three options' shell**: catalog entries, shell tokens for dark and light, and the arrangements in `options/<id>/option.css`.",
           "3. **Colour**: the [colour bridge](colour-bridge.md). One configuration change, one stylesheet, edits to the three classes a rule cannot decide, and the four sources of colour that are not utility classes. "
           "After this step every page is legible in every layout and theme, and the existing layouts' light theme exists.",
           "4. **Wording**: move page strings into the catalogue. The plain vocabulary is the right-hand column of the [crosswalk](../crosswalk/legacy-navigation-settings.md).",
           "5. **Page regions**: the fixed region tree in [page-regions.md](page-regions.md), the dashboard and the repository page first, with `?section=`.",
           "6. **Shared presentational classes**, route by route: section, table, state mark, banner, button, field, label and value list. This is what gives Ledger its rules, Console its density and Guide its cards.",
           "7. **Shared changes built with the layouts** (section 2).",
           "8. **Shared changes that are each their own change**, one review each.",
           "9. **Qualification** across six layouts, two themes and two sizes, with the tests in section 9.", "",
           "What changes for someone who stays on Classic, and nothing else: the shared changes (they are corrections and apply everywhere), three corrected labels (decision D11), "
           "and a Dark and Light choice beside the layout choice. Page order, tab order and everything else stay as they are.", "",
           "## 6. What does not change", "",
           "- **Routes.** `/login`, `/`, `/repos`, `/repos/:id`, `/conflicts`, `/conflicts/:id`, `/audit`, `/settings`, `/users`, `/ldap`, `/config`, `/setup`, `/docs`. Thirteen, identical at both tips. None is added, removed or renamed. `?section=` is a parameter on an existing route.",
           "- **The API module.** No call is added by a layout. The shared changes that alter when an existing call is made are listed in section 2.",
           "- **Permissions and roles.**",
           "- **Classic as default and fallback**, with dark as its theme, and every label in the existing three layouts apart from the three corrections in D11.",
           "- **The order of regions on every page**, in every layout (decision D12).",
           "- **Safety wording**, in both vocabularies.",
           "- **The sign-in page.**", "",
           "## 7. The two epics", "",
           f"#264 names `epic/reposync-partial-52` as the first target. Read at `{Z.P}`. The reliability epic was read at `{Z.R}`.", "",
           "- The #262 source files and every page except the two below are byte-identical at the two tips, so the same layout work applies to both. `api.ts` differs: partial-52 adds the #56 policy types and the commit-map target state from #54.",
           "- `pages/RepoDetail.tsx` differs: partial-52 has the #56 path-policy editor (`PathPolicyEditor56`); reliability-61 has two text areas. The option styles whichever is present. Row G04.6 draws both.",
           "- `pages/Dashboard.tsx` and `pages/RepoDetail.tsx` differ in the commit-map target cell: partial-52 uses `CommitMapTargetCell`. Rows G02.8 and G04.16.",
           "- A layout must not carry #56 code to the reliability epic or remove it from partial-52.",
           "- The colour bridge was counted on partial-52. The class list on the reliability epic differs only where those two pages do.", "",
           "## 8. Dependencies and owners", "",
           "| Item | State | Effect on the options |", "|---|---|---|",
           "| #262 layout foundation | closed as completed 2026-11-15 | the base for everything here; its stated limits (Classic's bar wrapping between 1280 and 1490 px, Classic on a phone not measured, focus outline clipped in scrolling link strips) are untouched |",
           "| #69 pair refresh and re-anchor | holds in force as far as the record shows | shown as held; no control that acts; those regions are restyled in place and not moved out of that owner's code |",
           "| #73 conflict application | open | the conflict page says a decision is recorded, not applied |",
           "| #56 path-policy forms (epic #52) | present on partial-52 only | styled, not redesigned |",
           "| #63 credential origin (RS-11) | not done | credential origin is shown as not reported |",
           "| #64 setup-wizard cancellation (RS-19) | follow-ups open | row G12.8 waits for that owner |",
           "| #121 screen-reader runbook and usability check | not run | no option can claim an accessibility pass |",
           "| #199 delete one side of a pair | separate backlog | not drawn |",
           "| PR #195 acceptance lab | finds controls by label and flow | the existing vocabulary is frozen for this reason; the shared changes that add a step need its flows updated in the same change |", "",
           "## 9. Test oracles", "",
           "Checks #264 can automate. None has been run. Sizes are 1440×900 and 390×844.", "",
           "### Every layout, existing and new", "",
           "- With nothing saved: Classic, dark. The device's colour setting has no effect.",
           "- A layout and a theme each survive a reload for the same account and site, and are not visible to another account in the same browser.",
           "- An unknown, corrupt, foreign or unreadable preference falls back to Classic and dark with #262's notice text. A record saved before E2 still selects its layout, with theme dark.",
           "- Switching layout or theme does not remount the routed page: a typed draft, an open dialog, the focused element and the scroll position are unchanged, and no request is sent.",
           "- The sign-in page is identical to the build before the change, whatever was last chosen.",
           "- Every token in the design-system contract has a value for the layout in both themes, and the computed pairs there and in the colour bridge meet their targets (unit tests on the token and bridge files).",
           "- After the colour bridge, none of the three classes it cannot decide is left in the source.",
           "- At both sizes the document is no wider than the window on every route, including with the longest synthetic repository name.",
           "- An automated accessibility scan (`axe-core` is already a dependency) reports no new violation against Classic dark on the same route and state. A scan is not an accessibility pass.", "",
           "### Classic, Individual and Team", "",
           "- In dark, before the shared changes: the same visible elements and the same tab order as the build before the change on every route, apart from one addition, the theme select beside the layout select. "
           "Apart from that select and its effect on where the bar wraps, the pixels are the same. The “More” button, the navigation icons, the three derived regions and the four group headings on the directory form are in the tree and hidden.",
           "- Every string identical to the build before the change (snapshot of the existing vocabulary), apart from the three corrected labels. The acceptance lab passes with those three updated.",
           "- No Help entry, no visible “More” control and no visible navigation icons.",
           "- In light: every route is legible, and the bridge's pair check passes for the existing-layouts light palette.", "",
           "### Ledger, Console and Guide", "",
           "- The chooser lists the three after the existing three.",
           "- The shell tree is the #262 tree plus the E1 button, the E2 select and the E6 icons: same element types and order, `nav` labelled “Primary”, `main` with `tabindex=\"-1\"`.",
           "- An administrator reaches ten destinations; another account sees six and no Administration group. The active entry has `aria-current=\"page\"`. The conflict count shows when there is one.",
           "- Tab order through navigation equals reading order at both sizes: workspace links, “More” where shown, then account links, administration links, chooser, sign-out.",
           "- Opening “More” sets `aria-expanded`, moves focus to the first revealed link, and Escape closes it and returns focus.",
           "- Every focusable control in navigation shows its focus outline fully inside the window.",
           "- On the dashboard and the repository page, regions are seen in the order they have in the document, at both sizes.",
           "- The markup inside the routed page is the same whichever of the six layouts is chosen. The prototypes meet this for the three new options, and `tools/build.py` checks it.",
           "- The shell status shows the health word the aggregate read returns, and switching layout sends no request.",
           "- `?section=` shows the named section; an unknown value shows Overview; Back returns to the previous section; a draft in Settings survives a visit to Activity and a change of layout.",
           "- **Ledger**: wide, workspace links and status on the first row and the rest on the second, with no “More”. At 860 px and below, the workspace links and “More” form the second row. Section headings carry a rule and no box; titles use the serif stack.",
           "- **Console**: wide, a 244 px sticky sidebar with three group labels. At 900 px and below, a bottom bar with the four workspace links and “More”; the last line of every page scrolls clear of it. From 1101 px a page with sections keeps its status beside the content.",
           "- **Guide**: four pill tabs with “Account and admin” at the end of the row at every width above 760 px; at 760 px and below a bottom bar with the four links and “More”. Controls are at least 42 px high.", "",
           "### Shared changes", "",
           "The matrix row is the oracle: its states column lists what must be reachable and its note says what differs from today. Each is tested in one existing layout and one new layout, since it is the same code.", "",
           "## 10. Outside #264", "",
           "- Showing why sign-in was reached after a session ended (G01.4), and the first-run refusal (G01.5).",
           "- Credential origin and revocation (G04.4, S24).",
           "- A resume control for a verified partial import (G04.14).",
           "- A catch-all route for unknown addresses (S28).",
           "- Redacting the Teams webhook on read (G11.3). It needs the server to stop returning the address, which issue #268's suggested direction does not cover.",
           "- Stacking the rows of wide tables into blocks at narrow widths, with the same markup. This is an improvement for #264 to decide against the real application; the prototypes keep scroll frames with a shadow at the hidden edge."]
    return "\n".join(md) + "\n"


# ------------------------------------------------------------------ option READMEs
OPT_TEXT = {
    "ledger": dict(
        idea="RepoSync as a ledger of what moved between SVN and Git. Pages read top to bottom like a record. Sections are separated by rules, not boxes; state is a small stamp; ink is the only accent, so colour is left for state.",
        who="Someone who wants to read what happened and be sure of it: a maintainer checking history before a release, or a reviewer.",
        nav="A two-row masthead. First row: the four workspace pages and the sync status. Second row: account links, administration links, the signed-in name, the layout and theme chooser, sign-out. Up to 860 px the four workspace links stay visible in a second row, and “More” at the end of that row opens the rest below.",
        page="A page with sections shows a contents list at the left from 861 px. Tables are ruled and use the full width. Titles are in a serif stack.",
        trade="Least dense of the three on a wide screen. Two rows of navigation cost height. The serif title face differs by operating system because no font is downloaded.",
        primary="light"),
    "console": dict(
        idea="An operations console. Navigation in a grouped sidebar, dense tables, and a status inspector that stays beside the work.",
        who="Someone who watches several repositories through the day and acts on them: a team's sync operator or administrator.",
        nav="A 244 px sidebar with Workspace, Account and Administration groups; status, name, chooser and sign-out at its foot. Up to 900 px it becomes a top strip that stays in view, and the four workspace pages and “More” move to a bar fixed at the bottom.",
        page="13 px text, 28 px controls, striped tables. From 1101 px the dashboard puts the attention list in a left column, and a page with sections keeps its status in a panel at the right.",
        trade="Small text and small pointer targets by design. Closest to today's Team layout, so the least new. The right-hand status panel needs 1101 px.",
        primary="dark"),
    "guide": dict(
        idea="A task-led layout. Four large destinations, everything else behind one “Account and admin” control, roomy sections and large targets.",
        who="Someone who opens RepoSync now and then and needs to see whether anything needs them: an individual with a few repositories.",
        nav="A sticky top bar with four pill tabs for the workspace pages and “Account and admin” at the end of the same row, which opens account links, administration links, name, chooser and sign-out. Up to 760 px the four tabs and “More” move to a bar fixed at the bottom.",
        page="16 px text, 42 px controls, rounded sections. The same tables and forms as the other two, drawn larger. A page with sections shows them as chips.",
        trade="Fewest items per screen. Administration is always one step further away than in the other two.",
        primary="light"),
}


def option_readme(o):
    t, name = OPT_TEXT[o], kit.OPTIONS[o]["name"]
    n = len(build.S)
    md = [f"# Option: {name} (`{o}`)", "", t["idea"], "",
          "| | |", "|---|---|", f"| For | {t['who']} |", f"| Navigation | {t['nav']} |", f"| Pages | {t['page']} |", f"| Trade-offs | {t['trade']} |",
          f"| Themes | dark by default; light when chosen. Designed first in {t['primary']} |", "", "Accepted for #264 on 2026-11-16 ([decisions](../../DECISIONS.md)).", "",
          "## Files", "",
          f"- [tokens.css](tokens.css): every colour, type and spacing value, for light and dark. The first twelve per theme are the #262 shell tokens.",
          f"- [option.css](option.css): how the shell is arranged and how components look in this option.",
          f"- `screens/`: {n} prototype pages. Start at [screens/g02-dashboard.html](screens/g02-dashboard.html) or use the [index of all screens](../../prototypes/index.html).",
          f"- [flows/index.html](flows/index.html): {len(build.FLOWS)} task flows, each a sequence of the pages above.", "",
          "## Viewing", "",
          "Open any page from the file system; nothing is fetched. Add `#light` or `#dark` to the address to fix the theme, and `#light-nobar` or `#dark-nobar` to hide the grey prototype bar for captures. "
          "Without either, the page is dark unless Light was chosen with the bar's buttons. "
          "Sizes used in the matrix: 1440×900 and 390×844.", "",
          "## Representative pages", "", "| Page | Light | Dark |", "|---|---|---|"]
    for slug, title in [("g02-dashboard", "Dashboard"), ("g03-repositories", "Repositories"), ("g03-add-repository", "Add repository"), ("g04-overview", "Repository: overview"),
                        ("g04-operations", "Repository: operations and import"), ("g04-settings", "Repository: settings"), ("g04-remove-pair", "Remove a branch pair"),
                        ("g06-conflict-diff", "Conflict"), ("g07-audit", "Audit log"), ("g08-account", "My account"), ("g09-users", "Users"), ("g10-directory", "Directory (LDAP)"),
                        ("g11-identity-notifications", "Identity & notifications"), ("g12-setup-8", "Server setup: import"), ("g13-docs-overview", "Help"), ("g01-login", "Sign in"),
                        ("s01-shell-roles", "Shell and roles"), ("s02-chooser", "Layout and theme chooser")]:
        md.append(f"| {title} | [open](screens/{slug}.html#light) | [open](screens/{slug}.html#dark) |")
    md += ["", "## Where this option differs from the other two", "",
           "Navigation, density, type, section treatment, how a page's sections are reached, the dashboard arrangement, and what a phone shows. "
           "Page content, labels, controls and states are the same in all three, and so is the markup inside every page: the build checks it. No option offers a different page structure. "
           "See the [capability parity map](../../matrix/capability-parity.md).", "",
           "## Not verified", "",
           "The designer checked these pages as files (structure, links, token definitions, computed contrast) and, from v1.5, in a sandboxed browser for errors, for a page wider than the screen, and for selected elements that reach past the box that holds them. "
           "That is not a visual review. The publisher rendered v1 to v1.5 and captures this version after delivery; see the package index. No keyboard, screen-reader or accessibility check was made."]
    return "\n".join(md) + "\n"


# What the render check reported for this tree. Set from the run on the finished tree.
RENDER = {"pages": 230, "loads": 920, "errors": 0, "blocked": 0, "page_overflow": 0, "inner": 0, "theme": 0, "wide_frames": 6, "narrow_frames": 124, "shots": 3}


# ------------------------------------------------------------------ package index
def readme(R, counts, kinds, extra):
    by = {k: sum(1 for r in R if delivery(r) == k) for k in DELIVERY}
    md = ["# RepoSync #263 design package v1.6", "",
          "Three new whole-interface options for RepoSync, each in dark and light, wide and narrow, with the matrix, crosswalk and contracts that #264 builds from. "
          "Written 2026-11-16 for [issue #263](https://github.com/chriscase/RepoSync/issues/263). It supersedes v1 to v1.5 of the same day. It applies the decisions Chris made and the changes that four independent reviews, of v1, v1.3, v1.4 and v1.5, required.", "",
          "This is the version for the public repository, at `docs/design/263/v1.6/`, with the publisher's captures beside it at `docs/design/263/captures/v1.6/`. "
          "No earlier version is in the repository: v1 to v1.5 stay private Artifacts, recorded by their checksums in [history/README.md](history/README.md).", "",
          "**This is a design. Nothing in it is in the product.** Classic, Individual and Team stay available and stay the default. #263 cannot close #264.", "",
          "## Start here", "",
          "- [DECISIONS.md](DECISIONS.md): what was decided, in Chris's words and the designer's.",
          "- [prototypes/index.html](prototypes/index.html): every screen in every option, with links to flows. Open it from the file system; nothing is fetched.",
          "- [matrix/surface-state-matrix.md](matrix/surface-state-matrix.md): every surface and state, with its source and its prototype pages.",
          "- [CHANGES-FROM-v1.5.md](CHANGES-FROM-v1.5.md): each item of the final check of v1.5, and what was done about it.",
          "- [CHANGES-FROM-v1.4.md](CHANGES-FROM-v1.4.md): each item of the closing check of v1.4, and what was done about it.",
          "- [CHANGES-FROM-v1.3.md](CHANGES-FROM-v1.3.md): each item of the follow-up review of v1.3, and what was done about it.",
          "- [CHANGES-FROM-v1.md](CHANGES-FROM-v1.md): each finding of the independent review of v1, and what was done about it.",
          "- [CHANGES.md](CHANGES.md): what differs from each earlier version.", "",
          "## The three options", "",
          "All three are accepted for #264.", "",
          "| Option | Id | Idea | Navigation, wide | Navigation, narrow | Text and controls |", "|---|---|---|---|---|---|",
          "| [Ledger](options/ledger/README.md) | `ledger` | A record to read. Ruled sections, stamps, ink-only accent | Two-row masthead | Workspace links in a second row, with “More” for the rest | 15 px, 34 px controls, serif titles |",
          "| [Console](options/console/README.md) | `console` | An operations console. Dense tables, status beside the work | Grouped sidebar | Bottom bar: four workspace pages and “More” | 13 px, 28 px controls |",
          "| [Guide](options/guide/README.md) | `guide` | Task-led. Four large destinations, roomy sections, big targets | Four pill tabs and “Account and admin” | Bottom bar: four destinations and “More” | 16 px, 42 px controls, rounded |", "",
          "What they share: one plain vocabulary, one grouping, the same page content, controls and states, the same routes, and the same markup inside every page (identical on 68 of the 69 screens; the chooser page differs only in the option's own name). "
          "What differs: navigation model, density, type, how sections are drawn, how a page's sections are reached, the dashboard arrangement, and the phone pattern. "
          "No option offers a different page structure. That is deliberate: a layout may arrange a page but never render a different one.", "",
          "## Coverage", "",
          f"- **{len(R)} matrix rows: {counts['covered']} covered, {counts['explicit gap']} explicit gap, {counts['not applicable']} not applicable.** No blank cells.",
          f"- {len(build.S)} prototype pages per option, {len(build.S) * 3} in all, plus {len(build.FLOWS)} flows per option. Each page is built for dark and light and for 1440×900 and 390×844.",
          f"- For #264: {by['layout only']} rows are layout only; {by['shell extension']} need an accepted change to the #262 shell; {by['shared change, with the layouts']} are shared changes built with the layouts; "
          f"{by['shared change, own change']} are shared changes that are each their own change; {by['waits for an owner']} waits for an owner; {by['feature work']} are feature work outside #264.",
          "- The two epics differ in three rows (path-policy editor, commit-map target cell twice). Each of those rows describes both.", "",
          "## Contents", "",
          "| Path | What it is |", "|---|---|",
          "| `README.md` | this index |",
          "| `DECISIONS.md` | the decisions of 2026-11-16 |",
          "| `CHANGES-FROM-v1.5.md` | the final check's items K1 to K6, with what changed and where |",
          "| `CHANGES-FROM-v1.4.md` | the closing check's items M1 to M8, with what changed and where |",
          "| `CHANGES-FROM-v1.3.md` | the follow-up review's items N1 to N13 and the residuals, with what changed and where |",
          "| `CHANGES-FROM-v1.md` | the first review's findings F1 to F14 and O1 to O5, with what changed and where |",
          "| `CHANGES.md` | what changed from each earlier version |",
          "| `SHA256SUMS` | SHA-256 of every other file in the tree |",
          "| `prototypes/index.html` | index of every option, screen and flow |",
          "| `prototypes/assets/` | the shared stylesheet, the existing layouts' palette and the one script |",
          "| `options/<id>/README.md` | the option's idea, trade-offs and representative pages |",
          "| `options/<id>/tokens.css`, `option.css` | the option's tokens and arrangement |",
          "| `options/<id>/screens/*.html` | the prototype screens |",
          "| `options/<id>/flows/index.html` | the task flows |",
          "| `matrix/surface-state-matrix.csv`, `.json`, `.md` | the surface and state matrix |",
          "| `matrix/capability-parity.md` | every capability and where each option carries it |",
          "| `crosswalk/legacy-navigation-settings.md` | today's navigation, sections, settings and labels, and where each is in the new options |",
          "| `contracts/design-system.md`, `contrast-computed.csv` | tokens, type, responsive rules, components, focus, contrast, the existing layouts' light palette |",
          "| `contracts/interaction.md` | actions, states, validation, permissions, receipts, held capabilities |",
          "| `contracts/colour-bridge.md` and two `.csv` files | how existing pages take colour from the tokens, with every colour class in the source and its contrast |",
          "| `contracts/page-regions.md`, `.json` | the fixed region tree for the dashboard and the repository page |",
          "| `contracts/implementation-map.md` | what #264 builds, in what order, with test oracles |",
          "| `history/README.md` | where the earlier versions are |",
          "| `tools/` | the scripts that generated and checked this tree, and the render check |", "",
          "Each `.md` file has an `.html` rendering beside it for reading in a browser.", "",
          "The screens are in `options/<id>/screens/`, not `options/<id>/prototype/` as the issue suggested, because the Artifact service does not accept a published path that contains a folder named `prototype`.", "",
          "## Decided", "",
          "1. All three options go to #264, to the full extent the prototypes show.",
          "2. Dark until chosen, in every layout. Light is offered in all six.",
          "3. Five changes to the #262 shell are accepted (E1 to E4 and E6): a “More” control, a saved theme, wording per layout family, a Help link in the new layouts, and navigation icons. The sign-in page stays as it is.",
          "4. Every shared change the prototypes draw is adopted, in every layout.",
          "5. Three labels that misstate what a control does are corrected in every layout. Every other label in Classic, Individual and Team stays.",
          "6. Pages keep today's order of regions in every layout. The new options fit that order and add read-only summaries at the top.",
          "7. Repository sections are addressable with `?section=`.",
          "8. Held and missing items stay as they are.",
          "9. The markup inside every page is the same in all three options.", "",
          "The full record, with who decided what and what is still open, is in [DECISIONS.md](DECISIONS.md).", "",
          "## Provenance", "",
          "### Source read", "",
          f"- `epic/reposync-partial-52` at `{Z.P}`.",
          f"- `epic/reposync-reliability-61` at `{Z.R}`.",
          "- Both tips were fetched and read on 2026-11-16. The thirteen routes are identical.",
          "- Against the 2026-11-12 pin `acf027bec8b6c9c0ceb3f53fe5a9e794d40a2b81`, the reliability tip changes nine `web-ui` files, all of them #262: `components/Layout.tsx`, the seven new files under `layouts/` and `tests/layout-foundation.e2e.mjs`.",
          "- Between the two tips, twelve `web-ui` files differ, all on the partial-52 side: `api.ts`, `pages/Dashboard.tsx` and `pages/RepoDetail.tsx` change; eight files are added for the #56 path-policy editor and the commit-map target cell (#54) with their tests; and `tests/layout-foundation.e2e.mjs` differs by four lines. Everything else under `web-ui` is byte-identical, including the #262 catalog, frame, preference and token files.",
          "- Line numbers in the matrix and the page regions were resolved by `tools/resolve.py` against both revisions with `git show`. The colour bridge was counted by `tools/bridge.py` the same way. Nothing was checked out, built or run from the repository.",
          "- Issues read: #263 (the authority for this package), #264, #262, and the issues named in the implementation map as read on 2026-11-12.", "",
          "### Captures, made by the publisher", "",
          "The publisher's captures are the visual record. The publisher rendered **v1** on 2026-11-16 with a sandboxed Chromium run as a non-root user: 221 pages, light and dark, at 1440×900 and 390×844, 884 captures. "
          "As the publisher reported them: no page or console errors, no request outside the package, the requested theme applied in all 832 prototype captures, and four captures wider than the screen, all the save-recovery page at 390 px. "
          "The results file has SHA-256 `106bc228eea2d65d6bbf8051560bc28e18de0e5cae21e60950c2dff294d5b5a9`, and the publisher's checksum list for the capture folder has SHA-256 `380d2aba6ec040158c90b7bb83b8ef8baccbd7a3bb323b1ee6088b53ff124a14`. "
          "The captures are not in this package.", "",
          "The publisher rendered **v1.2** the same way later that day: 880 captures, again with no errors, no outside requests and no theme mismatches, and the same four over-wide captures. "
          "The “More” control was opened by hand in each option and rendered complete at 390 px: every link, both selects and sign-out. Seen by eye: identifiers breaking inside two tables, and one table cut at its frame's edge on a phone with nothing to show that it scrolls. "
          "The publisher's notes file has SHA-256 `fcb1c12213f9d700cc82627a2fd5c0eb9fcac03564bc365f9d18bb7d6090da52`.", "",
          "The publisher rendered **v1.3** too: 964 captures, including the open “More” control through the `-menu` fragment, with no errors, no outside requests and no theme mismatches. "
          "The save-recovery page fitted. Eighteen captures were wider than the screen at 390 px, from two causes that v1.4 removed: review notes that did not wrap, and long addresses in one document. "
          "The results file has SHA-256 `fe8dbc7e8d321dd61f6232f5acd2ccb5c821fb63b9c677c346de019a78137026`, and the publisher's checksum list has SHA-256 `20b97313264b3a1399e2900714614f405d4760a6bdcf69bc13d0385aad35fc4e`.", "",
          "The publisher rendered **v1.4** as well: 968 captures with no errors, no outside requests, no theme mismatches and no page wider than the screen. "
          "The review of v1.4 then found identifiers that crossed the edge of their card at 1440 px, which a page-width measure does not see, and one table header cut at its frame. This version corrects them. "
          "The results file has SHA-256 `b6a8c88c65ebc21f5118f0ab276f388edac0b7d986a1edfec1f3045d5f9d7e23`, and the publisher's checksum list has SHA-256 `42dae6d1516601c0e1973800e6076a7a4f8ddfcd093427203e11ebebf163d6d4`.", "",
          "The publisher rendered **v1.5** too: 972 captures with no errors, no outside requests, no theme mismatches and no page wider than the screen. "
          "The final check viewed 29 of them and found no overflow inside a card, and the three tables corrected in v1.5 fitting at 1440 px in every option. "
          "The results file has SHA-256 `f8301b7c0335e9ec7c5f568c1a4f3c345748999caf72ca0f6e8cf1cf3552718a`, and the publisher's checksum list has SHA-256 `52e20f8eb684908cfabb6bb1fc38a3eaa00f1528f3abda58313c17d101ed22b3`.", "",
          "- What they are: pictures of the static, synthetic v1, v1.2, v1.3, v1.4 and v1.5 prototypes.",
          "- What they do not show: the application, the keyboard, a screen reader, zoom, or this version.",
          "- **The publisher captures v1.6 after delivery.** Those captures go into the repository beside this tree, at `docs/design/263/captures/v1.6/`.", "",
          "For the open “More” control, capture these with `-menu` added to the fragment (`#light-nobar-menu`, `#dark-nobar-menu`): `s01-shell-roles`, `s01-shell-member`, `g02-dashboard` and `g09-users`, in each option. "
          "Ledger and Console show the control at the narrow width only; Guide shows it at both.", "",
          "### The designer's render check", "",
          "Added in v1.5 and run again on this version. Versions up to v1.4 said that no sandboxed browser could start in the container the package is written in. That was true as root, which is how it had been tried. "
          "Run as a non-root user, the same Chromium starts with its sandbox enabled. The script reads that user's browser processes once, right after launch, and none carried a flag that turns the sandbox off.", "",
          f"`tools/render_check.js` loaded every HTML page of this tree except the tools, {RENDER['pages']} pages, in light and dark at 1440×900 and 390×844: {RENDER['loads']} loads. "
          "Requests outside the package are aborted and counted. On each load it looks for page and console errors, a page wider than the screen and a theme other than the one asked for.", "",
          "It also measures selected elements inside `main`: identifiers, notes, pills, chips, stamps, buttons, links, headings, paragraphs, labels, fields and inline results. "
          "Each is measured left and right against the nearest section, banner, dialog, operation card, evidence box, list item, table cell or specimen figure. Table frames are measured separately.", "",
          "What it does not measure: height; text that overflows inside a block whose own box fits; anything outside `main`, which means the shell and its menu; and any open-menu state. The publisher's captures cover those.", "",
          f"- Result: {RENDER['errors']} errors, {RENDER['blocked']} requests outside the package, {RENDER['page_overflow']} pages wider than the screen, {RENDER['inner']} elements past their box, {RENDER['theme']} theme mismatches.",
          f"- At 1440 px no table frame in a prototype page scrolls. Three wide tables in the documents do: the matrix, the crosswalk and the design-system contract. "
          f"At 390 px table frames scroll on {RENDER['narrow_frames']} of the {RENDER['loads'] // 2} loads, which is the accepted treatment for a phone (decision D22).",
          f"- The designer looked at a sample: {RENDER['shots']} screenshots of pages this version changes, taken by the same check.",
          "- Run on v1.4, the same check reproduced the overflows that version's review found, and found three tables cut off at 1440 px that no review had listed. v1.5 corrected them; see [CHANGES-FROM-v1.4.md](CHANGES-FROM-v1.4.md).",
          "- What it is not: the publisher's captures, which remain the visual record; keyboard, screen-reader, zoom or accessibility evidence; or a test of the application. The results file and the screenshots are not in the package.", "",
          "### What was done to check this package", "",
          "Authored checks, run on the files:", "",
          f"- Every HTML page: tags balanced, no duplicate ids, every link and stylesheet resolves inside the tree, no reference to anything outside it, every icon used is defined. {extra['pages']} pages, {extra['problems']} problems.",
          "- Every token used by the stylesheets has a value in each option and theme, and in the existing layouts' palette.",
          f"- Contrast computed from token values: {extra['contrast']} pairs across the three options, all at or above target; {extra['legacy_light']} of 30 for the existing layouts' light palette.",
          f"- Colour bridge: {extra['bridge']['classes']} colour classes in the source resolved by rule except {extra['bridge']['undecided']}; "
          f"{extra['bridge']['pairs']} background and text pairs that appear together, computed in seven token sets, {extra['bridge']['low']} below 4.5:1.",
          "- Every matrix row: source text found at both revisions, every referenced prototype page exists, no blank cell; every prototype page is referenced by a row.",
          "- Status evidence on each page describes the same health as the page's status line.",
          "- The markup inside every page is identical in all three options on 68 of the 69 screens. On the chooser page it differs only in the option's own name.",
          "- The render check described above.", "",
          "Not done:", "",
          "- **No visual review of every page.** The render check measures; it does not judge how a page looks. The designer looked at a sample of screenshots of changed pages. "
          "Faults that a measure does not catch are possible until the publisher's captures of this version are reviewed.",
          "- No build, test, lint, type-check or application launch. No real application evidence of any kind.",
          "- No screen reader, keyboard walk-through, zoom or automated accessibility scan. **No accessibility pass is claimed for any option.** The statement that tab order equals reading order is a property of the markup as written, not a tested result.",
          "- No usability testing. The fit of each option to its audience is the designer's judgement.", "",
          "### Data", "",
          "All names, repositories, revisions, hashes and people in the prototypes are invented. Hosts use the reserved `.example` and `.invalid` names (`makerspace.example`, `example.org`, `corp.example`), "
          "apart from GitHub's public API address, which is the product's own default for that field, and `localhost` in a help example. No real repository content or credential appears.", "",
          "### Earlier versions", "",
          "v1 to v1.5 are kept as published and none of them is in the repository. "
          "Each is identified by the SHA-256 of its `SHA256SUMS`. The links are private Artifacts and open only for their owner.", "",
          "| Version | SHA-256 of its `SHA256SUMS` | Private Artifact |", "|---|---|---|",
          f"| v1 | `{kit.V1_SUMS}` | {kit.V1_ARTIFACT} |",
          f"| v1.1 | `{kit.V11_SUMS}` | {kit.V11_ARTIFACT} |",
          f"| v1.2 | `{kit.V12_SUMS}` | {kit.V12_ARTIFACT} |",
          f"| v1.3 | `{kit.V13_SUMS}` | {kit.V13_ARTIFACT} |",
          f"| v1.4 | `{kit.V14_SUMS}` | {kit.V14_ARTIFACT} |",
          f"| v1.5 | `{kit.V15_SUMS}` | {kit.V15_ARTIFACT} |", "",
          f"An earlier issue of v1.3, SHA-256 `{kit.V13_WITHDRAWN_SUMS}`, was withdrawn before use. More is in [history/README.md](history/README.md).", "",
          f"The 2026-11-12 review is inside v1, the private Artifact above, unpacked and unchanged. The archive delivered that day has SHA-256 `{kit.V1_ARCHIVE_SHA}`. This version carries no copy. "
          "One disclosure about the 2026-11-12 packet: its screenshots were taken with a headless browser started with default options, which does not enable the browser sandbox, on local files with the network blocked. No later version did that.", "",
          "### Versions", "",
          "`v1.6` is not edited after publication. Corrections go to `v1.7`, beside it."]
    return "\n".join(md) + "\n"


def tools_readme():
    return """# Tools

The scripts that generated and checked this tree, as they were run. The generator and its checks need Python 3 and its standard library only. The render check needs Node with Playwright and its Chromium. Nothing is installed or fetched.

| File | What it does |
|---|---|
| `kit.py` | helpers that build the HTML pieces |
| `tokens.py` | every token value; writes `options/<id>/tokens.css`; computes contrast |
| `base.css`, `opt_ledger.css`, `opt_console.css`, `opt_guide.css` | sources of `prototypes/assets/base.css` and each `option.css` |
| `proto.js`, `icons.html` | the prototype script and icon set |
| `screens_a.py`, `screens_b.py` | the content of every screen, written once for all options |
| `build.py` | renders every screen in every option, the index and the flows; validates structure, links, icons and tokens |
| `matrix_data.py` | the matrix rows |
| `resolve.py` | finds each row's source text at both epic revisions with `git show` |
| `mdhtml.py` | renders the Markdown documents to HTML |
| `docs.py`, `docs_text.py`, `v11.py` | write the matrix, parity map, crosswalk, contracts, decisions and READMEs |
| `bridge.py` | lists the colour classes in the source and computes contrast for the colour bridge |
| `finish.py`, `cover.py` | finish the tree (index, checks, checksums) and write the Artifact cover page |
| `render_check.js` | the designer's render check: loads every page in a sandboxed Chromium and measures errors, outside requests and overflow |

Run from the package root. `REPOSYNC_REPO` is the path of a clone of the repository; `resolve.py` and `bridge.py` read it with `git show`:

```
REPOSYNC_REPO=/path/to/RepoSync python3 tools/build.py out
REPOSYNC_REPO=/path/to/RepoSync python3 tools/docs.py out
REPOSYNC_REPO=/path/to/RepoSync python3 tools/finish.py out
```

`render_check.js` needs Node with Playwright and its Chromium, both already present where it was run; nothing was installed. It was run as a non-root user so that the browser's sandbox is on, on a copy of the tree:

```
node tools/render_check.js <copy of the tree> <output folder>
```

It starts the browser with the sandbox enabled. Once, right after launch, it reads the browser processes of the user it runs as and reports how many carry a flag that turns the sandbox off; that number was 0.

`resolve.py` and `bridge.py` read the repository with `git show` only. They do not check out, build or run anything. `build.py` also checks that status evidence matches the status badge on each page and that the markup inside every page is the same in all three options.
"""


def write_all(OUT, R, counts, kinds, write, write_md):
    write_md("contracts/design-system.md", design_system(), "Design-system contract")
    write("contracts/contrast-computed.csv", contrast_csv())
    write_md("contracts/interaction.md", interaction(), "Interaction contract")
    write_md("contracts/implementation-map.md", implementation_map(R), "Implementation map for #264")
    for o in OPTS:
        write_md(f"options/{o}/README.md", option_readme(o), f"Option: {kit.OPTIONS[o]['name']}")
    write_md("tools/README.md", tools_readme(), "Tools")
