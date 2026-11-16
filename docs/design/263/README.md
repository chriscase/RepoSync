# #263 whole-interface design

This folder holds the accepted design for [issue #263](https://github.com/chriscase/RepoSync/issues/263): three new whole-interface options for RepoSync, Ledger, Console and Guide, each in dark and light, at desktop and phone widths. It also holds the surface and state matrix, the crosswalk from today's interface, and the contracts that [#264](https://github.com/chriscase/RepoSync/issues/264) builds from.

**This is a design, not the product.** Nothing here changes RepoSync. Classic, Individual and Team stay available and stay the default. The prototypes are static pages with synthetic data, and no accessibility pass is claimed for them.

## What is here

| Path | What it is |
|---|---|
| [`v1.6/`](v1.6/README.md) | The design package, version 1.6. Start at `v1.6/README.md`, or open `v1.6/prototypes/index.html` in a browser; it fetches nothing. |
| [`captures/v1.6/`](captures/v1.6/README.md) | The publisher's renders of v1.6: 264 representative screenshots, the results of all 976 captures, and the script that made them. |

Each folder has a `SHA256SUMS` file covering every other file in it.

## How v1.6 was reached

The designer delivered v1 on 2026-11-16 and then revised it through v1.6 the same day.

- **Each version was checked by the publisher before it was reviewed:**
  - every file was verified against the version's checksums;
  - every page was rendered in a sandboxed browser, in both themes and at both widths, with all outside requests blocked.
- **Five independent, read-only reviews followed,** of v1, v1.3, v1.4, v1.5 and the final changes in v1.6. Each one checked the package against the source code at both epic branches.
- **Every required change was made in the next version.** The `CHANGES-FROM-*.md` files in `v1.6/` list each item and what was done about it. The final check of v1.6 found nothing blocking. It left three minor follow-ups for #264, which are recorded on #263.
- **Chris's decisions** on the options, themes and scope are in `v1.6/DECISIONS.md`.

The repository carries only the accepted version. Versions v1 to v1.5 and the review reports are kept privately. `v1.6/history/README.md` records the earlier versions by checksum.

## Access control

Where the package touches access control, it describes what each page shows to whom. For what the server enforces, it defers to [issue #268](https://github.com/chriscase/RepoSync/issues/268).

## What comes next

#264 implements the design in RepoSync. `v1.6/contracts/implementation-map.md` is the starting point. The matrix marks which rows are layout-only work, which are shared changes, and which are feature work outside #264.
