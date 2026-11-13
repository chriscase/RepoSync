# Switchable layout foundation (#262)

A person can show the web UI as **Classic**, **Individual** or **Team**. A
layout is presentation only. Every layout shows the same routes, pages, data,
actions, status and account controls. Choosing one never grants or removes a
capability, never changes server mode, sync behavior or account settings, and
never sends anything to the server.

Classic is the default. It is also the fallback whenever a preference is
absent, unknown, corrupt, foreign or unreadable.

## Catalog

`web-ui/src/layouts/layoutCatalog.ts` holds a frozen, strict enum:
`classic | individual | team`. `isLayoutId` accepts only those exact strings,
so there is no case folding, no trimming and no prototype keys.
`resolveLayout` maps anything else to Classic.

Each entry carries only presentation fields:

- `id`, `label` and `description`.
- `navPresentation`: `inline` or `grouped`.
- `themes`.
- Layout and spacing classes for a fixed set of shell slots.

| Layout | Arrangement |
| --- | --- |
| Classic | The pre-#262 top bar, with the same slots and spacing. |
| Individual | Centred content (`max-w-7xl`) and tab-style links. On narrow screens the bar wraps and the links scroll horizontally. |
| Team | From `md` up, a 16rem sticky sidebar that groups navigation under Workspace, Account and Administration. Below `md` it becomes a top bar whose groups scroll horizontally, and the group headings stay available to screen readers. |

## Navigation

The navigation stays in `web-ui/src/components/Layout.tsx`, in
`PRIMARY_NAVIGATION`. Its targets, labels, order and active matching are
unchanged in every layout, and the Dashboard link keeps `end`.

- Admin visibility is still `getStoredUser()?.role === 'admin'`, so Users,
  LDAP, Config and Setup appear only for admins.
- The Team group headings are presentation only. The Administration heading
  appears only when an admin entry is visible.
- The status query, the account name, the conflict badge and the Logout
  handler are unchanged.

## Stable content boundary

`LayoutFrame` renders one fixed element tree for every layout:

```
div.rs-layout-shell[data-rs-layout][data-rs-theme]
├─ nav[aria-label="Primary"] > div > div > [brand + links slot, account slot]
└─ main[tabindex=-1] > <Outlet/>
```

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
- **Value:** exactly `{"version":1,"layout":…,"identity":…,"origin":…}`.

Rules:

- **Stable identity only.** The identity must be the server user id: a
  non-empty string of at most 256 characters with no control characters. A
  sign-in without one, such as a legacy login with no user object, is not
  persisted and does not read stored preferences. The role, display name,
  token and credentials are never stored.
- **Strict reading.** Extra or missing fields, another version, another
  identity or origin, an unknown layout, an over-long value and invalid JSON
  are all ignored. The shell then shows Classic with a visible status, "Using
  Classic", and the stored value is not deleted.
- **Unreadable storage.** If storage cannot be read, or accessing it throws,
  the shell shows Classic with the same notice. It does not crash.
- **Unsaved choices still apply.** A choice that cannot be saved — because
  storage refuses, or the sign-in has no identity or origin — still applies
  in that tab, with a "Not saved" status explaining why. Storage failure
  never changes the layout silently.
- **Account changes.** An account change remounts the shell (SessionBoundary
  keys on the session epoch). The shell then re-reads that account's own
  preference, and a scope change without a remount does the same.
- **Other tabs.** A choice made in another tab applies here after a reload.

## Chooser

`web-ui/src/layouts/LayoutChooser.tsx` is a native `<select>` in the account
slot.

- It is labelled "Layout" and described as changing only the navigation
  arrangement.
- A polite `role="status"` paragraph next to it carries preference notices.
- Tab reaches it, arrow keys change the value, and focus stays on the same
  node while the layout changes.
- It is kept compact so it adds as little as possible to the Classic header.
  That header was already wider than common laptop widths for administrators
  before #262, and Classic keeps that existing behavior.

## Theme seam

`web-ui/src/layouts/layoutTokens.css` defines semantic shell tokens (`--rs-shell-*`):

- canvas, chrome, border, hover;
- strong and muted text;
- accent and accent text;
- alert and alert text;
- notice;
- focus.

They are scoped to `.rs-layout-shell`. Only the shell chrome, links, badge
and chooser use them. The dark values are the pre-#262 Tailwind colors, so
Classic's shell keeps the same colors. The browser test compares the
computed colors. `data-rs-theme` is always `dark`, and
`THEME_IDS` is `['dark']`.

A later design adds a theme by:

1. adding an id to `THEME_IDS`;
2. adding a `.rs-layout-shell[data-rs-theme='<id>']` token block;
3. listing the id in each layout's `themes`.

**Light rendering is not claimed.** Page content and `SyncStatus` still use
their existing dark Tailwind classes. A light theme for pages needs separate,
owned page work, and this foundation does not choose a theme for the person.

## Evidence (authored; execution not yet admitted)

- **Vitest, existing configuration** (`src/**/*.test.ts`, node environment):
  - `src/layouts/layoutCatalog.test.ts`: the strict enum, the Classic
    fallback, immutability, presentation-only fields, Classic spacing,
    narrow-screen classes and the theme default.
  - `src/layouts/layoutPreference.test.ts`: identity and origin scoping,
    strict parsing, unreadable or unwritable storage, account isolation,
    notices and the hook's initial state.
  - `src/layouts/LayoutFoundation.test.ts`: element-path identity of
    `main`/`Outlet`, the chooser and status slots in every layout; rendered
    navigation targets, labels, order, active state and admin visibility per
    layout; the same queries and no mutations; per-account preference and
    visible fallback; and the chooser markup.
- **Browser** (`web-ui/tests/layout-foundation.e2e.mjs`): runs against the
  existing synthetic fixture with an admitted Chromium. It checks:
  - Classic default colors and axe on the shell nav in every layout;
  - keyboard switching that keeps the same page, draft input, focus, URL and
    history, with no mutation;
  - a dirty repository edit draft keeping its text, selection, inner scroll,
    focus and page scroll;
  - phone-width reachability for Individual and Team, and the Team sidebar
    when wide;
  - persistence, account scoping, and corrupt, foreign and blocked storage;
  - member visibility in every layout.

  Run it from `web-ui/`:

  ```
  REPOSYNC_A11Y_BROWSER=/path/to/chromium node --test --test-concurrency=1 tests/layout-foundation.e2e.mjs
  ```

None of these has been executed for this source. Typecheck, Vitest, build and
browser results need their own admission.

## Out of scope

- **Classic at phone width:** it keeps its existing behavior.
- **Not changed:** routes, pages, the API, auth/session code, the engine or
  schema, dependencies, workflows and the Vite or Tailwind configuration.
- **Not implemented:** the November 12 design review's global Operations
  endpoint and its phased stories.
