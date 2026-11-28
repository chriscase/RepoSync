# Tools

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
