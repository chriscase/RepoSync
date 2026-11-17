# Switchable layout foundation (#262, amended by #264)

A person can show the web UI as **Classic**, **Individual** or **Team**, and
since #264 also as **Ledger**, **Console** or **Guide**, each in a **dark** or
a **light** theme. A layout and a theme are presentation only. Every layout
shows the same routes, pages, data, actions, status and account controls.
Choosing one never grants or removes a capability, never changes server mode,
sync behavior or account settings, and never sends anything to the server.

Classic in dark is the default. It is also the fallback whenever a preference
is absent, unknown, corrupt, foreign or unreadable.

## Catalog

`web-ui/src/layouts/layoutCatalog.ts` holds a frozen, strict enum of six
layout ids: `classic | individual | team | ledger | console | guide`.
`isLayoutId` accepts only those exact strings, so there is no case folding, no
trimming and no prototype keys. `resolveLayout` maps anything else to Classic.
`THEME_IDS` is `['dark', 'light']`, and `isThemeId` is as strict.

Each entry carries only presentation fields:

- `id`, `label` and `description`.
- `navPresentation`: `inline` or `grouped`.
- `themes`: `dark` and `light` in every layout.
- `vocabulary`: `existing` for Classic, Individual and Team, `plain` for
  Ledger, Console and Guide (E3).
- Layout and spacing classes for a fixed set of shell slots. They are empty
  for Ledger, Console and Guide, whose arrangement is in
  `web-ui/src/layouts/layoutOptions.css`, keyed on `data-rs-layout`.
- For Guide, a longer label for the More button.

| Layout | Arrangement |
| --- | --- |
| Classic | The pre-#262 top bar, with the same slots and spacing. At desktop widths (measured from 1280px up), when the bar no longer fits, the account controls wrap to a second line instead of going off screen. |
| Individual | Centred content (`max-w-7xl`) and tab-style links. On narrow screens the bar wraps and the links scroll horizontally. |
| Team | From `md` up, a 16rem sticky sidebar that groups navigation under Workspace, Account and Administration. Below `md` it becomes a top bar whose groups scroll horizontally, and the group headings stay available to screen readers. |
| Ledger | A two-row masthead, ruled sections and stamps. |
| Console | A grouped sidebar, dense tables and status beside the work. |
| Guide | Four large destinations, roomy sections and big targets. |

Classic, Individual and Team arrange pages as before #264. Ledger, Console and
Guide arrange the same page markup through `web-ui/src/layouts/pageClasses.css`.

## Navigation

The navigation is rendered by `web-ui/src/components/Layout.tsx` from
`PRIMARY_NAVIGATION` in `layoutCatalog.ts`. In Classic, Individual and Team
its targets, labels, order and active matching are unchanged, and the
Dashboard link keeps `end`.

- Admin visibility is still `getStoredUser()?.role === 'admin'`, so Users,
  LDAP, Config and Setup appear only for admins.
- The Team group headings are presentation only. The Administration heading
  appears only when an admin entry is visible.
- The status query, the account name, the conflict badge and the Logout
  handler are unchanged. Since #264 the status shows its health as a mark
  with its own shape (S03); its text is unchanged.
- A focused link inside a horizontally scrolling link strip (Individual, and
  Team below `md`) is scrolled fully into view within that strip. The handler
  scrolls only that strip, never the page. The browser's own focus scrolling
  still applies, for example when a dialog restores focus with a plain `focus()`
  to a link that is off screen.

#264 adds:

- **More (E1).** A "More" button follows the last workspace link, in every
  layout. Opening it marks the shell with `data-nav-open`, which the layout's
  CSS uses to show the account and administration entries it folds away, and
  moves focus to the first of those links. Escape closes it and returns focus
  to the button, and a route change closes it. In Classic, Individual and Team
  one CSS rule hides it, so those layouts look and tab as before.
- **Help (E4).** An entry for the existing `/docs` route, in Ledger, Console
  and Guide only. Classic, Individual and Team gain no entry.
- **Icons (E6).** Each link starts with an `aria-hidden` icon, in every
  layout. The same CSS rule hides the icons in Classic, Individual and Team.
- **Plain labels (E3).** Ledger, Console and Guide label the entries in the
  plain vocabulary, for example "My account" for Settings and "Sign out" for
  Logout. Apart from Help, the targets, order, active matching and admin
  visibility are the same in every layout.

## Vocabulary

`web-ui/src/layouts/textCatalogue.ts` and `vocabulary.ts` hold one text
catalogue with two vocabularies (E3). A layout names its vocabulary:
`existing` for Classic, Individual and Team, `plain` for Ledger, Console and
Guide. The existing vocabulary is the text those layouts showed before #264,
byte for byte. Safety and truthful-status sentences are the same in both.
Two sentences on the repository page name a catalogued control: the header
switch's title names the Stop or remove section, and the pause note in that
section names the header pause control. Each is a whole-sentence catalogue
entry. In the plain vocabulary it names the control's plain label, "Stop or
remove" or "Pause sync…", and is otherwise the same sentence.
Switching layout changes the text in place and remounts nothing.

Labels in Classic, Individual and Team are unchanged, apart from these
corrections. #264's shared changes make them in every layout and both
vocabularies:

- "Retry", shown while sync is paused after errors, is "Resume sync…". It
  opens a confirmation (D11, G04.9).
- "Cancel operation" is "Stop operation…" (D11). It opens a confirmation
  that says what stopping does not undo, then sends the same request.
- The branch pair removal dialog is titled "Remove branch pair", and its
  confirm button reads "Remove pair" and "Removing…" while pending. Before,
  they read "Delete branch pair", "Delete Branch Pair" and "Deleting...". When
  remote deletion is chosen, the title and button name what will be deleted,
  and the pending text is "Deleting..." (G04.25). This goes beyond the three
  D11 corrections; the owner chose to keep it in every layout.
- The audit log's action filter lists its actions in groups (G07.1). It no
  longer lists Conflicts Detected, Conflicts Resolved, Sync Errors, Webhooks,
  Daemon Events, Auth Events or Config Changes. An action in the address that
  is not listed still gets its own option and still filters.

"Trigger Sync" keeps its label in every layout; D11's condition for "Request
sync" is not met.

## Stable content boundary

`LayoutFrame` renders one fixed element tree for every layout:

```
div.rs-layout-shell[data-rs-layout][data-rs-theme][data-rs-bridge]
├─ nav[aria-label="Primary"] > div > div > [brand + links slot, account slot]
└─ main[tabindex=-1] > <Outlet/>
```

`data-rs-layout` is one of the six layout ids and `data-rs-theme` is `dark` or
`light`. `data-rs-bridge` is present for every layout in light and for
Ledger, Console and Guide in dark, and absent for Classic, Individual and Team
in dark (see Theme seam). #264 adds the More button (E1) inside the links slot
and an icon (E6) inside each link; neither is on the path to `main`. The
vocabulary provider around `<Outlet/>` adds no element.

- **What a layout may change:** classes, `data-rs-*` attributes, and how the
  link slot groups its entries.
- **What it may not change:** an element type or key on the path to `main` or
  `<Outlet/>`. There is no strategy wrapper and no strategy-based key, and the
  status and chooser elements keep fixed slots.

So switching does not remount the routed page. The page keeps:

- its React state, unsaved drafts, query observers and in-flight mutations;
- the focused element, text selection and inner scroll;
- the URL and history. Switching never navigates.

Switching issues no request of its own. Pages keep their own polling.

## Preference

`web-ui/src/layouts/layoutPreference.ts` stores the choice in this browser
origin's `localStorage`:

- **Key:** `reposync.layout-preference.v1:<encoded origin>:<encoded user id>`.
  #264 keeps the same key, so no saved layout is lost.
- **Value:** exactly
  `{"version":2,"layout":…,"theme":…,"identity":…,"origin":…}` (E2).
- **Version-1 records.** A record saved before #264, exactly
  `{"version":1,"layout":…,"identity":…,"origin":…}`, is still read. It
  selects its layout with the dark theme and is not rewritten until the next
  choice, which stores version 2.

Rules:

- **Stable identity only.** The identity must be the server user id: a
  non-empty string of at most 256 UTF-16 code units, with no control
  characters and no lone surrogate. Valid surrogate pairs and other Unicode
  are kept. An unusable id is rejected, never trimmed, replaced or
  normalized into a different scope. A sign-in without a usable id, such as a
  legacy login with no user object, is not persisted and does not read stored
  preferences. The role, display name, token and credentials are never stored.
- **No throwing key construction.** `preferenceKey` returns `null` instead of
  throwing for text it cannot encode, and the origin gets the same
  well-formedness check as the identity.
- **Every saved record re-reads.** A stored record may be at most 512 UTF-16
  code units. JSON escaping can lengthen an accepted id: 256 quotation marks
  become 512 code units. So a scope persists only if its record fits for the
  longest layout id and the longest theme id. Otherwise it is unscoped
  (`record-too-large`) for every layout: nothing is read or written, and
  Classic in dark shows without a notice. A choice still applies, with a "Not
  saved" notice that names the length, not the browser. The writer also
  refuses an oversized record before `setItem`, so an earlier stored value is
  kept, and "saved" always re-reads as the same layout and theme.
- **Strict reading.** Extra or missing fields, another version, another
  identity or origin, an unknown layout, an unknown theme, an over-long value
  and invalid JSON are all ignored. The shell then shows Classic in dark with
  a visible status, "Using Classic", and the stored value is not deleted.
- **Unreadable storage.** If storage cannot be read, or accessing it throws,
  the preference shows Classic in dark with the same notice. It does not
  crash.
  - A throwing `localStorage` getter is kept as `storageRefused`. For a
    genuine account and origin it reads as unavailable and shows "Using
    Classic".
  - Missing identity, origin or storage API stays quiet and distinct.
  - In the full shell, the unchanged session reader also cannot read the
    signed-in account when the getter throws. That is existing auth-storage
    behavior, left as is, so the shell shows Classic with member navigation
    and no preference notice.
- **Unsaved choices still apply.** A choice of layout or theme that cannot be
  saved — because storage refuses, or the sign-in has no identity or origin —
  still applies in that tab, with a "Not saved" status explaining why.
  Storage failure never changes the layout or theme silently.
- **Account changes.** An account change remounts the shell (SessionBoundary
  keys on the session epoch). The shell then re-reads that account's own
  preference, and a scope change without a remount does the same.
- **Other tabs.** A choice made in another tab applies here after a reload.

## Chooser

`web-ui/src/layouts/LayoutChooser.tsx` holds native `<select>` elements in the
account slot: Layout and, since #264, Theme (E2).

- The layout select is labelled "Layout". In Classic, Individual and Team it
  is described as changing only the navigation arrangement. In Ledger,
  Console and Guide it is described as changing how navigation and pages are
  arranged and worded.
- The theme select is labelled "Theme", offers Dark and Light, and is
  described as changing only colours.
- A polite `role="status"` paragraph next to them carries preference notices
  for both. While it is empty, Ledger, Console and Guide hide it visually
  only, so it stays in the accessibility tree.
- Tab reaches each select, arrow keys change the value, and focus stays on the
  same node while the layout or theme changes.
- The chooser is kept compact so it adds as little as possible to the Classic
  header.
  By estimate (its measured width minus the chooser), that header was already
  slightly wider than a 1280px screen for administrators before #262, with
  Logout still on screen; with the chooser it pushed Logout off screen at
  1280px. Classic's row now wraps the account controls to a second,
  right-aligned line only when they cannot fit even with their text wrapped.
  Measured with the synthetic fixture's administrator before #264: one 64px
  row from 1536px up; one row of 76px at 1500 to 1520px, where the status text
  takes three lines; two rows of 87px at 1280 to 1490px. Logout was on screen
  at each measured width (1280 to 1920px). Narrower widths were not measured.
- #264 adds the theme select and the status mark (S03) to the account slot,
  so the administrator's Classic bar needs more width for one row. It is one
  64px row from 1800px, and the browser test checks it there; before #264 the
  test checked 1600px. Narrower widths are not recorded here for #264.

## Theme seam

`web-ui/src/layouts/layoutTokens.css` defines semantic shell tokens (`--rs-shell-*`):

- canvas, chrome, border, hover;
- strong and muted text;
- accent and accent text;
- alert and alert text;
- notice;
- focus.

They are scoped to `.rs-layout-shell`. Only the shell chrome, links, badge
and chooser use them. Their values in `layoutTokens.css` are the dark values
for Classic, Individual and Team: the pre-#262 Tailwind colors, so Classic's
shell keeps the same colors in dark. The browser test compares the computed
colors.

`data-rs-theme` is `dark` or `light`, and `THEME_IDS` is `['dark', 'light']`
(E2). Every layout lists both themes. Dark is the default, and it stays until
the person chooses light; the foundation never chooses a theme for them.

`web-ui/src/layouts/pageTokens.css` holds the shell tokens and the page
tokens (`--rs-bg`, `--rs-surface`, `--rs-text` and the rest) for seven token
sets: Ledger, Console and Guide in light and in dark, and Classic, Individual
and Team in light. Where the shell carries `data-rs-bridge`:

- `web-ui/src/layouts/colourBridge.css` points each colour utility class the
  pages use at a page token. `web-ui/scripts/colour-bridge.mjs` generates it
  from the source.
- `web-ui/src/layouts/pageClasses.css` styles the shared presentational
  classes (`rs-btn`, `rs-sec`, `rs-bn` and the rest) in Ledger, Console and
  Guide.

Classic, Individual and Team in dark carry no `data-rs-bridge` and declare no
page tokens, so their pages keep their own Tailwind classes exactly as before
#264.

A later design adds a theme by:

1. adding an id to `THEME_IDS`;
2. adding its shell and page token blocks to `pageTokens.css`;
3. listing the id in each layout's `themes`.

## Evidence

- **Vitest, existing configuration** (`src/**/*.test.ts`, node environment):
  - `src/layouts/layoutCatalog.test.ts`: the strict enum, the Classic
    fallback, immutability, presentation-only fields, Classic spacing,
    narrow-screen classes and the theme default. Since #264 also the six
    layouts, both themes in every layout, the vocabularies, the Help entry
    and the navigation icons.
  - `src/layouts/layoutPreference.test.ts`: identity and origin scoping,
    strict parsing, unreadable or unwritable storage, account isolation,
    notices and the hook's initial state. Also:
    - lone high and low surrogates, and valid pairs (RS262-1);
    - escaped ids, records exactly at and one beyond 512, layout-length
      differences, writer refusal before `setItem`, and save→read round trips
      (RS262-2);
    - refused storage carried through scope, read, write, hook and the
      chooser notice (RS262-3);
    - version-2 records with the theme, and version-1 records read as dark
      (#264, E2).
  - `src/layouts/LayoutFoundation.test.ts`: element-path identity of
    `main`/`Outlet`, the chooser and status slots in every layout; rendered
    navigation targets, labels, order, active state and admin visibility per
    layout; the same queries and no mutations; per-account preference and
    visible fallback; and the chooser markup. Also the shell path for:
    - lone-surrogate and surrogate-pair account ids (RS262-1);
    - an over-long escaped id (RS262-2);
    - a refused `localStorage` getter alongside existing auth-storage
      behavior (RS262-3).

    Since #264 it also checks `data-rs-bridge`, plain labels and Help in
    Ledger, Console and Guide, the icons and the More button in every layout,
    and the one rule that hides both in Classic, Individual and Team.
  - `src/layouts/textCatalogue.test.ts` and `textCataloguePages.test.ts`: the
    two vocabularies (E3).
  - `src/layouts/colourBridge.test.ts`: the committed `colourBridge.css`
    matches the generator's output, every colour class has a rule, and the
    generator reads every colour class in the source, whatever its variant
    prefix.
  - `src/layouts/pageTokens.test.ts`: every token the bridge and page styles
    use has a value in each of the seven token sets, and the design's contrast
    pairs meet their targets in each set.
- **Browser** (`web-ui/tests/layout-foundation.e2e.mjs`): runs against the
  existing synthetic fixture with an admitted Chromium. It checks:
  - Classic default colors and its single 64px row at 1800px (1600px before
    #264), axe on the shell nav in Classic, Individual and Team at 1800px
    and 1280px (`web-ui/tests/gui-variations-264.e2e.mjs` scans Ledger,
    Console and Guide), and a Classic bar at 1280px that keeps Logout on
    screen;
  - keyboard switching that keeps the same page, draft input, focus, URL and
    history, with no mutation;
  - a dirty repository edit draft keeping a changed Poll Interval, plus the
    text, selection, inner scroll and focus of #56's preview sample box, and a
    page scroll that is not reset to the top;
  - phone-width reachability for Individual and Team, with each focused link
    fully inside its strip, and the Team sidebar when wide;
  - focusing navigation links with `preventScroll` never moving the page, at
    390px and 1280px;
  - persistence, account scoping, and corrupt, foreign and blocked storage;
  - member visibility in every layout.

  Run it from `web-ui/`:

  ```
  REPOSYNC_A11Y_BROWSER=/path/to/chromium node --test --test-concurrency=1 tests/layout-foundation.e2e.mjs
  ```

- **Browser, #264** (`web-ui/tests/gui-variations-264.e2e.mjs`): Ledger,
  Console and Guide, the light theme, the repository sections and the
  version-1 record. Run it the same way.

Results for a given source are recorded with its qualification, not in this
document.

## Out of scope

- **Classic at phone width:** its links still overflow horizontally as before.
  The account controls now wrap to their own line, which is estimated, not
  measured, to be wider than a 390px screen, so Logout may still be off screen
  there.
- **Not changed by #262:** routes, pages, the API, auth/session code, the
  engine or schema, dependencies, workflows and the Vite or Tailwind
  configuration.
- **Not changed by #264:** routes, the API client (`web-ui/src/api.ts`), the
  engine or schema, dependencies, workflows and the Vite or Tailwind
  configuration. #264 changes pages: its shared changes apply in every
  layout, and the label corrections they make are listed under Vocabulary.
- **Not implemented:** the November 12 design review's global Operations
  endpoint and its phased stories.
