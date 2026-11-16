# Changes from v1.5, by item

The final check of v1.5 (2026-11-16) verified M1 to M8 as fixed apart from two role statements, which are its one required change, K1. It listed five smaller items, K2 to K6. Every item is accepted and changed here. Nothing is disputed.

| Item | What changed in v1.6 | Where |
|---|---|---|
| Publication | The public repository carries v1.6 and its captures. D23, the index, `history/README.md`, `CHANGES.md` and the cover name it. v1.5 joins the private earlier versions with its checksum. The repository path is `docs/design/263/v1.6/`, with captures at `docs/design/263/captures/v1.6/`. The publisher's v1.5 capture run is recorded beside the earlier runs. | `DECISIONS.md` (D23); `README.md`; `history/README.md`; `CHANGES.md`; the Artifact cover |
| K1 | (a) Row G04.30 says what the page does: every signed-in account sees the Errors card, and “Acknowledge” is shown to administrators (row G02.5). (b) The crosswalk's settings table no longer names a role beside a setting. Its column now gives the scope and the matrix rows that say who sees the setting, for every line of the table (decision D28). The whole column was checked against the rows it points to. Nothing is said about what the server checks. | row G04.30; `crosswalk/legacy-navigation-settings.md`; `tools/docs.py`; `DECISIONS.md` (D28) |
| K2 | The render check's scope is stated exactly in the index: which elements, against which boxes, and what it does not measure. D27 and the option pages use the same wording. The script now measures against dialogs, includes third-level headings, and reads only its own user's browser processes, once, right after launch. The three older lines that said no browser could start are dated to their version. The screenshots the designer looked at are called a sample. | `README.md`; `DECISIONS.md` (D27); `tools/render_check.js`; `tools/README.md`; `CHANGES.md`; `CHANGES-FROM-v1.md`; `CHANGES-FROM-v1.4.md` |
| K3 | The sentence about who else may stop an import is removed. The operations vocabulary, interaction contract §3 and §5 and row G04.13 say that the import controls are shown to administrators. D21 is unchanged and now literally true. | `options/*/screens/s05-operations`; `contracts/interaction.md` §3, §5; row G04.13; `CHANGES-FROM-v1.3.md` |
| K4 | (a) The note beside the two tests in the edit view says only that the result appears as a message, as today. (b) “Credential save failed” shows the client's text as it renders it, with the reply's own body. (c) The stopped import was stopped by an administrator, on the operations page and on the dashboard. (d) The Git token field in the edit view says that no token is saved, matching the read view. | `options/*/screens/g04-settings-edit`, `g08-account-states`, `g04-operations`, `g02-dashboard` |
| K5 | Tables inside a dialog cover their edge shadow with the dialog's own colour, so Ledger's dialogs show no shadow when nothing is hidden. A row of buttons that follows a form keeps a gap from the last field. An identifier may also break after a full stop, so a long host name wraps there and no slash is left alone on a line. The wording about tables is corrected in `CHANGES-FROM-v1.4.md`: inside a table a short identifier does not break, and a path may break after a slash. | `prototypes/assets/base.css`; `options/ledger/option.css`; `tools/kit.py` (`_ident`); `CHANGES-FROM-v1.4.md` |
| K6 | Row G08.6 follows G08.5. The unused history code is removed from `tools/finish.py` and `tools/docs_text.py`. `CHANGES-FROM-v1.4.md` and `CHANGES-FROM-v1.3.md` say the markup is identical on 68 of 69 screens. | `matrix/*`; `tools/finish.py`, `tools/docs_text.py`; `CHANGES-FROM-v1.4.md`, `CHANGES-FROM-v1.3.md` |
| The 405 specimen | Kept. Its note adds that a proxy in front of the server may answer differently. | `options/*/screens/g08-account-states` |

## Checked after the changes

- `tools/build.py` and its checks pass: structure, links, tokens, evidence against badge, and markup inside `main` that is identical in all three options on 68 of the 69 screens.
- Every source anchor resolves at both epic tips.
- Every role statement in the matrix, the crosswalk and the interaction contract was read again against the page's own role conditions.
- The tree was searched again for statements about access control. What remains is what issue #268 says, cited to it.
- The render check was run again on this tree. Its results are in the package index.

## Not done

No visual review of every page, and no keyboard, screen-reader, zoom or accessibility check. The publisher captures this version after delivery.
