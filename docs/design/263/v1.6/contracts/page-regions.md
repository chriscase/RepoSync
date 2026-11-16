# Page regions

The rule that lets a page be arranged for a layout without losing a person's work when they switch, and without changing what the keyboard reaches first. It applies decisions D8 and D12.

## The rule

- **One tree per page.** A page renders the same named regions, with the same parents and keys, in every layout. `LayoutFrame` already does this for the shell.
- **Document order is today's order.** Regions stay in the order they have in the source now, in every layout. Classic, Individual and Team are unchanged. Ledger, Console and Guide show regions in that same order within each view, so what is seen, what the keyboard reaches and what a screen reader reads agree.
- **A layout arranges; it never re-renders.** Which regions show together and how they are drawn is decided by the layout's styles and a section attribute on the page. No region is rendered conditionally on the layout.
- **Sections keep everything mounted.** On the repository page a section shows its regions and hides the others. A draft in one section, an open dialog and a running request survive a change of section and a change of layout.
- **A region shown in two places is one region.** Where Overview shows a short list that another section shows in full, it is the same region, not a second copy with its own requests.
- **Three regions are new, read-only and derived**: “Needs attention” on the dashboard, the status line on the repository page, and the credentials summary in repository settings. They make no request of their own and hold no control that sends anything. They are in the tree of every layout, and hidden in Classic, Individual and Team so that they are neither seen, nor reached by the keyboard, nor read.

v1.1 recommended a new document order for these two pages. That is withdrawn: it would have changed keyboard and screen-reader order in the default layout.

## Dashboard: `/`

Order today is the order of the regions in `web-ui/src/pages/Dashboard.tsx` at partial-52, resolved by script.

| Region | Order today | Today | Source line (partial-52, reliability-61) | Matrix rows | In Ledger, Console and Guide | Note |
|---|---|---|---|---|---|---|
| `scope` | 1 | Repository scope select | `pages/Dashboard.tsx:107`, `:106` | G02.1 | Page head | none |
| `imports` | 2 | Import progress cards | `pages/Dashboard.tsx:123`, `:122` | G02.2 | In flight | Running operations from the status read are listed with them |
| `repositories` | 3 | “Repositories” overview | `pages/Dashboard.tsx:130`, `:129` | G02.3 | Repositories | A table in every option |
| `health` | 4 | Sync health panel and the count cards | `pages/Dashboard.tsx:196`, `:195` | G02.4, G02.5 | Sync health | Keeps the error acknowledgement |
| `position` | 5 | “Sync Position” (one repository in scope) | `pages/Dashboard.tsx:248`, `:247` | G02.6 | Sync health and position | none |
| `records` | 6 | “Recent Sync Records” | `pages/Dashboard.tsx:296`, `:295` | G02.7 | Recent sync records | none |
| `commit-map` | 7 | “Commit Map (SVN ↔ Git)” | `pages/Dashboard.tsx:331`, `:330` | G02.8 | Commit map | none |
| `activity` | 8 | “Recent Activity” | `pages/Dashboard.tsx:380`, `:381` | G02.9 | Recent activity | none |
| `server` | 9 | Server panel | `pages/Dashboard.tsx:407`, `:408` | G02.10 | Server | none |
| `attention` | not in the page today | none: new, derived from the same status reads | none | G02.11 | Needs attention: first, under the page head | Read-only; links only. In the tree of every layout and hidden in Classic, Individual and Team |

Order in Ledger, Console and Guide: scope, needs attention, then today's order: in flight, repositories, sync health, position, recent sync records, commit map, recent activity, server. From 1101 px Console puts needs attention in a left column and the rest beside it; it is first in the document, so the keyboard reaches it first.

## Repository: `/repos/:id`

Order today is the order of the regions in `web-ui/src/pages/RepoDetail.tsx` at partial-52.

| Region | Order today | Today | Source line (partial-52, reliability-61) | Matrix rows | In Ledger, Console and Guide | Note |
|---|---|---|---|---|---|---|
| `head` | 1 | Name, sync switch, Edit, Git Hook, Trigger Sync | `pages/RepoDetail.tsx:1085`, `:1078` | G04.1, G04.7 | Page head, on every section | none |
| `svn` | 2 | “SVN Configuration” | `pages/RepoDetail.tsx:1105`, `:1098` | G04.2, G04.4 | Settings: SVN source | The password field stays inside this region |
| `git` | 3 | “Git Configuration” | `pages/RepoDetail.tsx:1154`, `:1147` | G04.3, G04.4 | Settings: Git target | The token field stays inside this region |
| `sync` | 4 | “Sync Settings” | `pages/RepoDetail.tsx:1208`, `:1201` | G04.5 | Settings: Sync policy | none |
| `about` | 5 | “Quick Stats” | `pages/RepoDetail.tsx:1257`, `:1268` | G04.8 | Settings: About | none |
| `paused` | 6 | Paused-after-error banner | `pages/RepoDetail.tsx:1283`, `:1294` | G04.9, G04.10 | Overview, first, while paused | none |
| `status` | 7 | Status panel and cards | `pages/RepoDetail.tsx:1380`, `:1391` | G04.11, G04.30 | Overview: “What this status proves” and “Errors” | Keeps the error acknowledgement |
| `import` | 8 | Import notice and import card | `pages/RepoDetail.tsx:1424`, `:1435` | G04.12, G04.13, G04.14 | Operations; the current operation also shows on Overview | One region shown in two sections |
| `records` | 9 | “Sync Records” | `pages/RepoDetail.tsx:1437`, `:1448` | G04.15 | Activity; also shown short on Overview | One region shown in two sections, not two copies |
| `commit-map` | 10 | “Commit Map (SVN ↔ Git)” | `pages/RepoDetail.tsx:1461`, `:1472` | G04.16 | Activity | none |
| `audit` | 11 | “Recent Audit Log” | `pages/RepoDetail.tsx:1506`, `:1519` | G04.17 | Activity | none |
| `refresh` | 12 | “Update pair from parent” | `pages/RepoDetail.tsx:1609`, `:1622` | G04.18 | Branch pairs, first | Held under #69; preview only; not moved out of that owner's code |
| `reanchor` | 13 | “Re-anchor pair (not implemented)” | `pages/RepoDetail.tsx:1627`, `:1640` | G04.19 | Branch pairs, second | Held |
| `pairs` | 14 | “Branch Pairs” | `pages/RepoDetail.tsx:1678`, `:1691` | G04.20, G04.21, G04.25 | Branch pairs, third; also shown short on Overview | One region shown in two sections |
| `server` | 15 | Server panel | `pages/RepoDetail.tsx:1949`, `:1962` | G04.29 | Overview, last, collapsed | none |
| `stop-remove` | 16 | “Danger Zone” | `pages/RepoDetail.tsx:1951`, `:1964` | G04.22, G04.23, G04.24, G04.26 | Settings: Stop or remove, last | none |
| `policy` | inside the edit form | Path policy (inside the edit form) | none | G04.6 | Settings: Path policy | The #56 editor on partial-52; two text areas on reliability-61 |
| `status-line` | not in the page today | none: new, derived from the status read | none | G04.32 | Under the page head, on every section | Read-only: state, observed time, last sync, position and a link to the evidence. Hidden in the existing three layouts |
| `credentials` | not in the page today | none: new, derived from the credential read | none | G04.4, S23, S24 | Settings: Credentials, after Git target | A summary. “Replace…” opens the field in its own region and sends nothing itself. Hidden in the existing three layouts |

Sections in the new options, with the `?section=` value for each (decision D7). Regions are listed in document order, which is the order they are shown in.

| Section | `?section=` | Regions |
|---|---|---|
| Overview | `overview` or absent | paused, status, import (the current operation), records (short), pairs (short), server |
| Activity | `activity` | records, commit-map, audit |
| Branch pairs | `branches` | refresh, reanchor, pairs |
| Operations | `operations` | import |
| Settings | `settings` | svn, git, credentials, sync, policy, about, stop-remove |

`head` and `status-line` show on every section.

## Other pages

| Route | Arrangement |
|---|---|
| `/repos`, `/conflicts`, `/conflicts/:id`, `/audit`, `/users`, `/config`, `/setup`, `/docs`, `/settings` | Regions keep today's order in every layout. The options change wording and how regions are drawn. |
| `/ldap` | The new options show four headings between the fields: Directory sign-in, Connection, User mapping, Service account. They are elements in the tree of every layout and hidden in Classic, Individual and Team, from sight and from assistive technology, like the derived regions (decision D15). Those layouts are neither seen nor read differently. The form's own markup and field order do not change. |
| `/login` | Unchanged (decision D5). |
