# Changes from v1.3, by item

This file was written for v1.4 and is kept as it was, apart from the lines it marks as corrected. [CHANGES-FROM-v1.4.md](CHANGES-FROM-v1.4.md) records what the closing check of v1.4 found afterwards.

The follow-up review of v1.3 (2026-11-16) confirmed most of the first review's findings as fixed and listed what remained: the rest of F8, residuals in F2, F6 and F14, and new items N1 to N13. Every one is accepted and changed here. Nothing is disputed.

| Item | What changed in v1.4 | Where |
|---|---|---|
| N1 | Role cells describe what the page does: “Shown to administrators; the page hides it from other accounts”. The sentence about the server is removed from every row that carried it, and from S02, G04.1 and interaction contract §5. The package makes no statement about what the server enforces outside issue #268 (decision D21). | `matrix/*`; `contracts/interaction.md` §5; `tools/matrix_data.py` |
| N2 | The three items are changed as the review lists them. The setup row of the secrets table names only what the wizard has fields for. One sentence on the sixth setup step is deleted. The setup warning shown is one the source returns, the Git LFS preflight warning, and the failure reads “API error 500.”. The tree was then searched for other statements of the same kind. One more was found and removed: the note on empty fields in the same row. | `options/*/screens/s06-credentials-identity`, `g12-setup-6`, `g12-setup-states`; `tools/screens_b.py` |
| F8 residual | The two panes show SVN's lines on the left and Git's on the right, compared with each other as the page does today. The base appears only in its own panel. The confirmation's row reads “Not chosen: Git version …”. | `options/*/screens/g06-conflict-diff`, `g06-conflict-states`; `tools/screens_b.py` |
| N3 (and the F6 and F9 residuals) | “Resume” is removed from the operations vocabulary. Stop is described the same way there, in interaction contract §3 and §5 and in row G04.13: shown to administrators. (v1.4 added a second half to that sentence; v1.6 removed it.) | `options/*/screens/s05-operations`; `contracts/interaction.md` §3; row G04.13 |
| N4 | S33, D16, interaction contract §5 and the note on the shell pages say what issue #268 says: on Identity & notifications, reading, and replacing the identity mappings, need only a session; saving and testing the Teams webhook are checked for the administrator role. “For Chris” item 2 says that G11.3 needs its own change. The shell pages' table reads “Administrators, in navigation only (#268)” for the two pages. | row S33, row G11.3; `DECISIONS.md`; `contracts/interaction.md` §5; `options/*/screens/s01-*` |
| N5 | The non-administrator's shell shows “Held”, the same aggregate health as every other page. | `options/*/screens/s01-shell-member`; `tools/build.py` (`shell`) |
| N6 (F2 residual) | The unknown evidence specimen reads “Sync configuration: unknown. No supported observation is available. Health, freshness and operation evidence remain unknown.” | `options/*/screens/g04-overview`; `tools/screens_a.py` (`evidence`) |
| N7 (F2 residual) | The held repository has one story everywhere: a full history import stopped at r768 of r1871, written locally through r768, confirmed on the Git remote through r750. The dashboard's attention list, its table, the operations page's status line and its operation card agree. The snapshot chip is gone, the snapshot notice is labelled as another repository's, and the commit map no longer lists the held repository. | `options/*/screens/g02-dashboard`, `g04-operations`, `g04-states`, `s08-existing-layouts-light`; `tools/screens_a.py` |
| N8 | The label column of a label and value list is capped at 40% of its container, so a long label cannot squeeze its value. | `prototypes/assets/base.css` (`dl.kv`) |
| N9 | Review notes wrap and cannot be wider than their container. Paragraphs, list items and headings in the document pages break long words, which covers plain addresses. | `prototypes/assets/base.css` (`.tag-needs`); every `*.html` rendering of a document |
| N10 | Names that are links inside a table stay whole. Identifiers of up to 24 characters never break. Longer ones, such as file paths, break after a slash before they break inside a word. | `prototypes/assets/base.css`; `tools/kit.py` (`mono`, `svn`, `git`) |
| N11 | The Skip dialog opens with nothing selected and “Skip selected” unavailable, as the page does today. The state after a choice is a labelled specimen on the same page. | `options/*/screens/g04-skip` |
| N12 | Corrected: five shell changes, E1 to E4 and E6, wherever four or “E1 to E5” was written; the cover no longer says the existing layouts are untouched; implementation map §5, §9 and §10; the matrix file's reference text; S31's note; the index says the markup is identical on 68 of 69 screens. For `/ldap`, the existing layouts' form is not changed: the four group headings are elements hidden in those layouts, and D15 and the implementation map now list them with the other hidden elements. | `README.md`; `DECISIONS.md` (D15); `contracts/implementation-map.md`; `contracts/page-regions.md`; `matrix/surface-state-matrix.json`; row S31; the Artifact cover |
| N13 | Links to earlier versions are labelled as private Artifacts and each is given with the SHA-256 that identifies it. | `README.md`, `CHANGES.md`, `history/README.md`, `tools/kit.py` |
| F14 residual | On the sixth setup step the marker “Collected but not sent today” sits directly under Authentication mode and names it and the OAuth fields. The matrix file no longer says captures are not part of v1. | `options/*/screens/g12-setup-6`; `matrix/surface-state-matrix.json` |
| Optional items | All three taken: the two “Who sees it” cells; the two opened groups in Guide start at the same height; the shell specimen pages mark no destination as current. | `options/*/screens/s01-*`, `s02-chooser`, `s03-*`, `s07-*`; `options/guide/option.css` |
| Phone tables | Kept as scroll frames with the shadow, as the publisher asked. Stacked rows is recorded as an improvement for #264 to decide (implementation map §10, decision D22). | `contracts/implementation-map.md` §10; `DECISIONS.md` |

## Checked after the changes

- `tools/build.py` and its checks pass: structure, links, tokens, evidence against badge, and markup inside `main` that is identical on 68 of the 69 screens (wording corrected in v1.6).
- All 278 source anchors still resolve at both epic tips.
- The tree was searched for statements about access control. What remains is what issue #268 says, cited to it. Functional refusals are described where the page shows their result. (This line was corrected in v1.5: as first written it claimed less than the tree says.)

## Not done, as of v1.4

The designer rendered nothing. The publisher captured v1.4 after delivery; the package index records that run.
