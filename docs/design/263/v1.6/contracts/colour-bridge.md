# Colour bridge

How existing pages get their colour from the page tokens without every page being rewritten first. It serves the light theme in all six layouts and both themes of the three new ones.

This is analysis of source text and arithmetic on token values. Nothing was built or rendered.

## What was counted

Every non-test `.tsx` file under `web-ui/src` at `b2b055a72657824fd6662b451f69718d73405aa5` (epic/reposync-partial-52), read with `git show`: 40 files. 1,216 class strings set a colour. They use **211 distinct colour classes, 1,811 times**. The list, with the token each resolves to, is [colour-bridge-classes.csv](colour-bridge-classes.csv).

## How the bridge works

- `tailwind.config.js` has an empty `theme.extend` today. The bridge points each colour utility that is in use at a CSS variable, and the variable's default is the value Tailwind gives now. With no theme attribute set, every page renders exactly as it does today.
- Tailwind lets background, text, border, ring, divide and placeholder colours be set separately, so `bg-gray-800` and `text-gray-800` can resolve to different tokens.
- Each layout and theme other than today's dark sets those variables to its page tokens, by the rules below.

## Rules

| Utility | Colour and step | Resolves to |
|---|---|---|
| background | grey 900 and darker | `--rs-bg` |
| background | grey 800 | `--rs-surface` |
| background | grey 700, and grey 200 to 400 | `--rs-surface-2` |
| background | grey 500 and 600 | `--rs-surface-3` |
| background | a colour at 800 or darker, at 200 or lighter, or at any step with opacity 50 or less | that colour's tint (`--rs-danger-soft`, `--rs-ok-soft`, `--rs-warn-soft`, `--rs-held-soft`, `--rs-accent-soft`, `--rs-run-soft`) |
| background | blue or indigo at 300 to 700, opaque | `--rs-accent` |
| background | red at 300 to 700, opaque | `--rs-danger-solid` |
| background | green, yellow, orange, purple, cyan at 300 to 700, opaque | that state's colour (`--rs-ok`, `--rs-warn`, `--rs-held`, `--rs-git`, `--rs-run`) |
| background | black with opacity | `--rs-scrim` |
| text | grey 100 and 200 | `--rs-text` |
| text | grey 300 and 400 | `--rs-text-2` |
| text | grey 500 and darker | `--rs-text-3` |
| text | a colour at 300 or darker | that state's colour; blue resolves to `--rs-link` |
| text | a colour at 100 or 200 | `--rs-text` (it sits on its own tint) |
| border, ring, divide | grey 700 and darker | `--rs-line` |
| border, ring, divide | grey 600 and lighter | `--rs-line-strong` |
| ring, outline | blue | `--rs-focus` |
| border | a colour | that state's colour |

Green and emerald are both “OK”; yellow and amber are both “warning”; purple marks the Git side and directory accounts. Each of these should be confirmed against its uses when the bridge is built; the rules are a starting map.

## What the bridge cannot do

**3 classes, 55 uses, need an edit where they are used.**

| Class | Uses | Why a rule cannot decide it |
|---|---|---|
| `text-white` | 39 | White text sits on a filled control in 38 of its 39 uses. The right colour depends on the fill: text on the accent, on the destructive fill and on a state fill are three different tokens in the dark themes. |
| `hover:text-white` | 10 | A hover colour for links and quiet buttons. It should become the strong text colour, which white is not in a light theme. |
| `bg-white` | 6 | Used for the documentation panel and for switch knobs. One is a surface and the other sits on a fill. |

These become classes that name their role, for example text on the accent fill. That is a change to those lines and to nothing else, and Classic renders the same because the new classes default to white.

## Contrast of the pairs that appear together

Where one class string sets both a background and a text colour, the pair can be checked: **49 distinct pairs, 184 uses**. Each was resolved by the rules, with white text resolved by the fill it sits on, and its contrast computed in every token set. The full table is [colour-bridge-pairs.csv](colour-bridge-pairs.csv).

| Token set | Pairs below 4.5:1 | Lowest ratio | Pair |
|---|---|---|---|
| existing layouts, light | 0 | 4.57:1 | `bg-green-900/50` with `text-green-300` |
| ledger, light | 0 | 5.06:1 | `bg-orange-900/50` with `text-orange-300` |
| ledger, dark | 0 | 5.16:1 | `bg-red-600` with `text-white` |
| console, light | 0 | 4.88:1 | `bg-cyan-900/50` with `text-cyan-300` |
| console, dark | 0 | 5.02:1 | `bg-red-600` with `text-white` |
| guide, light | 0 | 5.06:1 | `bg-orange-900/50` with `text-orange-300` |
| guide, dark | 0 | 5.02:1 | `bg-red-600` with `text-white` |

Limits of this check:

- It covers pairs written in the same class string. Most text takes its background from an ancestor element, and those pairs are covered only by the token pairs in the design-system contract.
- A class string split across a condition is read as separate strings, so some pairs are missed.
- Translucent backgrounds are treated as their tint token, not blended.
- It is arithmetic on declared values. It is not a rendered measurement and not an accessibility result.

## Tokens the classes resolve to

| Page token | Classes | Uses |
|---|---|---|
| `--rs-text-2` | 3 | 391 |
| `--rs-text` | 16 | 199 |
| `--rs-link` | 22 | 138 |
| `--rs-text-3` | 6 | 111 |
| `--rs-line` | 4 | 104 |
| `--rs-line-strong` | 10 | 100 |
| `--rs-danger` | 14 | 100 |
| `--rs-surface` | 4 | 86 |
| `--rs-surface-2` | 7 | 77 |
| `--rs-accent` | 4 | 60 |
| `--rs-ok` | 18 | 60 |
| `per-use decision` | 3 | 55 |
| `--rs-warn` | 19 | 55 |
| `--rs-focus` | 4 | 52 |
| `--rs-bg` | 6 | 42 |
| `--rs-git` | 9 | 36 |
| `--rs-danger-soft` | 8 | 33 |
| `--rs-accent-soft` | 12 | 22 |
| `--rs-warn-soft` | 10 | 20 |
| `--rs-ok-soft` | 8 | 16 |
| `--rs-surface-3` | 5 | 15 |
| `--rs-danger-solid` | 4 | 12 |
| `--rs-run` | 5 | 10 |
| `--rs-held` | 7 | 10 |
| `--rs-held-soft` | 2 | 4 |
| `--rs-run-soft` | 1 | 3 |

## Colour that does not come from utility classes

The bridge cannot reach these. Each needs a change in its own file, which is why the bridge is not “no page changes” for every page.

| Where | Today | Treatment |
|---|---|---|
| `UIForgeActivityStream` on the dashboard and the audit log (`Dashboard.tsx:313`, `:392`; `AuditLog.tsx:197`) and `uiforge-overrides.css` | `theme="dark"` is fixed | Pass the current theme, and move the overrides onto the page tokens |
| `react-diff-viewer` (`DiffViewer.tsx:24`) | `useDarkTheme={false}`: the diff renders light inside a dark page | Set it from the current theme, and take the diff colours from `--rs-code-bg`, `--rs-add-bg` and `--rs-del-bg` |
| Inline SVG colours (`Dashboard.tsx:162` to `165`, `Repositories.tsx:31` to `45`) | fixed hex values for the SVN and Git marks | Use `--rs-svn` and `--rs-git` through `currentColor` |
| `Documentation.tsx` | light utility classes on a white panel inside the dark shell | Covered by the bridge once its classes are mapped like any other page; today it is the one page that is already light |

These line numbers are at the partial-52 tip.

## What it means for #264

- The bridge is one configuration change and one stylesheet, plus the per-use edits above. It makes every page legible in every layout and theme before any page is restructured.
- It does not give Ledger its rules, Console its density or Guide its cards. Those come from the shared presentational classes and the page regions.
- The existing layouts' light theme is the bridge with the existing-layouts light palette, plus the four items in the section above.
