# Option: Console (`console`)

An operations console. Navigation in a grouped sidebar, dense tables, and a status inspector that stays beside the work.

| | |
|---|---|
| For | Someone who watches several repositories through the day and acts on them: a team's sync operator or administrator. |
| Navigation | A 244 px sidebar with Workspace, Account and Administration groups; status, name, chooser and sign-out at its foot. Up to 900 px it becomes a top strip that stays in view, and the four workspace pages and “More” move to a bar fixed at the bottom. |
| Pages | 13 px text, 28 px controls, striped tables. From 1101 px the dashboard puts the attention list in a left column, and a page with sections keeps its status in a panel at the right. |
| Trade-offs | Small text and small pointer targets by design. Closest to today's Team layout, so the least new. The right-hand status panel needs 1101 px. |
| Themes | dark by default; light when chosen. Designed first in dark |

Accepted for #264 on 2026-11-16 ([decisions](../../DECISIONS.md)).

## Files

- [tokens.css](tokens.css): every colour, type and spacing value, for light and dark. The first twelve per theme are the #262 shell tokens.
- [option.css](option.css): how the shell is arranged and how components look in this option.
- `screens/`: 69 prototype pages. Start at [screens/g02-dashboard.html](screens/g02-dashboard.html) or use the [index of all screens](../../prototypes/index.html).
- [flows/index.html](flows/index.html): 16 task flows, each a sequence of the pages above.

## Viewing

Open any page from the file system; nothing is fetched. Add `#light` or `#dark` to the address to fix the theme, and `#light-nobar` or `#dark-nobar` to hide the grey prototype bar for captures. Without either, the page is dark unless Light was chosen with the bar's buttons. Sizes used in the matrix: 1440×900 and 390×844.

## Representative pages

| Page | Light | Dark |
|---|---|---|
| Dashboard | [open](screens/g02-dashboard.html#light) | [open](screens/g02-dashboard.html#dark) |
| Repositories | [open](screens/g03-repositories.html#light) | [open](screens/g03-repositories.html#dark) |
| Add repository | [open](screens/g03-add-repository.html#light) | [open](screens/g03-add-repository.html#dark) |
| Repository: overview | [open](screens/g04-overview.html#light) | [open](screens/g04-overview.html#dark) |
| Repository: operations and import | [open](screens/g04-operations.html#light) | [open](screens/g04-operations.html#dark) |
| Repository: settings | [open](screens/g04-settings.html#light) | [open](screens/g04-settings.html#dark) |
| Remove a branch pair | [open](screens/g04-remove-pair.html#light) | [open](screens/g04-remove-pair.html#dark) |
| Conflict | [open](screens/g06-conflict-diff.html#light) | [open](screens/g06-conflict-diff.html#dark) |
| Audit log | [open](screens/g07-audit.html#light) | [open](screens/g07-audit.html#dark) |
| My account | [open](screens/g08-account.html#light) | [open](screens/g08-account.html#dark) |
| Users | [open](screens/g09-users.html#light) | [open](screens/g09-users.html#dark) |
| Directory (LDAP) | [open](screens/g10-directory.html#light) | [open](screens/g10-directory.html#dark) |
| Identity & notifications | [open](screens/g11-identity-notifications.html#light) | [open](screens/g11-identity-notifications.html#dark) |
| Server setup: import | [open](screens/g12-setup-8.html#light) | [open](screens/g12-setup-8.html#dark) |
| Help | [open](screens/g13-docs-overview.html#light) | [open](screens/g13-docs-overview.html#dark) |
| Sign in | [open](screens/g01-login.html#light) | [open](screens/g01-login.html#dark) |
| Shell and roles | [open](screens/s01-shell-roles.html#light) | [open](screens/s01-shell-roles.html#dark) |
| Layout and theme chooser | [open](screens/s02-chooser.html#light) | [open](screens/s02-chooser.html#dark) |

## Where this option differs from the other two

Navigation, density, type, section treatment, how a page's sections are reached, the dashboard arrangement, and what a phone shows. Page content, labels, controls and states are the same in all three, and so is the markup inside every page: the build checks it. No option offers a different page structure. See the [capability parity map](../../matrix/capability-parity.md).

## Not verified

The designer checked these pages as files (structure, links, token definitions, computed contrast) and, from v1.5, in a sandboxed browser for errors, for a page wider than the screen, and for selected elements that reach past the box that holds them. That is not a visual review. The publisher rendered v1 to v1.5 and captures this version after delivery; see the package index. No keyboard, screen-reader or accessibility check was made.
