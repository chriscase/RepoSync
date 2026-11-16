"""Write the Artifact cover page for the #263 package. The cover sits outside the checksummed tree.
Run last:  python3 tools/cover.py <out> <cover file>
"""
import hashlib
import pathlib
import sys

GEN = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(GEN))
import build  # noqa: E402
import docs  # noqa: E402
import docs_text  # noqa: E402
import resolve as Z  # noqa: E402

OUT = pathlib.Path(sys.argv[1]).resolve()
DEST = pathlib.Path(sys.argv[2]).resolve()


def sha(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()


WIDE = {
    "ledger": '<rect class="c" x="1" y="1" width="158" height="9"/><rect class="c" x="1" y="10" width="158" height="7"/><path class="r" d="M1 17.5h158M1 19.5h158"/>'
              '<rect class="k" x="8" y="4" width="14" height="3"/><rect class="b" x="30" y="4" width="46" height="3"/><rect class="b" x="8" y="12" width="60" height="2.4"/>'
              '<rect class="b" x="8" y="28" width="26" height="3"/><rect class="b" x="8" y="35" width="22" height="3"/><rect class="b" x="8" y="42" width="24" height="3"/>'
              '<rect class="k" x="44" y="27" width="50" height="5"/><path class="r" d="M44 38h108M44 60h108M44 82h108"/>'
              '<rect class="b" x="44" y="43" width="108" height="3"/><rect class="b" x="44" y="50" width="88" height="3"/><rect class="b" x="44" y="65" width="108" height="3"/><rect class="b" x="44" y="72" width="70" height="3"/><rect class="b" x="44" y="87" width="96" height="3"/>',
    "console": '<rect class="c" x="1" y="1" width="34" height="98"/><rect class="k" x="6" y="6" width="16" height="3"/><rect class="b" x="6" y="18" width="22" height="2.6"/><rect class="k" x="6" y="24" width="24" height="2.6"/>'
               '<rect class="b" x="6" y="30" width="20" height="2.6"/><rect class="b" x="6" y="36" width="22" height="2.6"/><rect class="b" x="6" y="50" width="18" height="2.6"/><rect class="b" x="6" y="64" width="20" height="2.6"/><rect class="b" x="6" y="70" width="24" height="2.6"/>'
               '<rect class="k" x="42" y="7" width="40" height="4"/><rect class="s" x="42" y="18" width="74" height="76"/><path class="r" d="M42 26h74M42 34h74M42 42h74M42 50h74M42 58h74M42 66h74M42 74h74M42 82h74"/>'
               '<rect class="s" x="122" y="18" width="32" height="46"/><rect class="b" x="126" y="23" width="20" height="2.6"/><rect class="b" x="126" y="31" width="24" height="2.6"/><rect class="b" x="126" y="39" width="18" height="2.6"/><rect class="b" x="126" y="47" width="22" height="2.6"/>',
    "guide": '<rect class="c" x="1" y="1" width="158" height="14"/><rect class="k" x="8" y="6" width="14" height="3.4"/><rect class="s" x="46" y="4" width="68" height="8" rx="4"/><rect class="k" x="48" y="5.2" width="16" height="5.6" rx="2.8"/>'
             '<rect class="b" x="68" y="6.6" width="11" height="2.8"/><rect class="b" x="84" y="6.6" width="11" height="2.8"/><rect class="b" x="100" y="6.6" width="11" height="2.8"/><rect class="s" x="130" y="4" width="24" height="8" rx="4"/>'
             '<rect class="k" x="20" y="24" width="52" height="5"/><rect class="s" x="20" y="36" width="57" height="26" rx="4"/><rect class="s" x="83" y="36" width="57" height="26" rx="4"/><rect class="s" x="20" y="68" width="57" height="26" rx="4"/><rect class="s" x="83" y="68" width="57" height="26" rx="4"/>'
             '<rect class="b" x="26" y="42" width="30" height="3"/><rect class="b" x="89" y="42" width="30" height="3"/><rect class="b" x="26" y="74" width="30" height="3"/><rect class="b" x="89" y="74" width="30" height="3"/>',
}
PHONE = {
    "ledger": '<rect class="c" x="1" y="1" width="44" height="19.5"/><path class="r" d="M1 20.5h44M1 22.5h44"/><rect class="k" x="5" y="4.5" width="14" height="3"/><rect class="b" x="5" y="16.5" width="7" height="2.4"/><rect class="b" x="14" y="16.5" width="7" height="2.4"/><rect class="b" x="23" y="16.5" width="7" height="2.4"/><rect class="b" x="32" y="16.5" width="7" height="2.4"/>'
              '<rect class="k" x="5" y="24" width="24" height="4"/><path class="r" d="M5 34h36M5 56h36M5 78h36"/><rect class="b" x="5" y="39" width="36" height="3"/><rect class="b" x="5" y="46" width="26" height="3"/><rect class="b" x="5" y="61" width="36" height="3"/><rect class="b" x="5" y="68" width="22" height="3"/><rect class="b" x="5" y="83" width="30" height="3"/>',
    "console": '<rect class="c" x="1" y="1" width="44" height="10"/><rect class="k" x="5" y="4" width="5" height="4"/><rect class="b" x="14" y="4.5" width="14" height="3"/><rect class="s" x="5" y="18" width="36" height="60"/>'
               '<path class="r" d="M5 26h36M5 34h36M5 42h36M5 50h36M5 58h36M5 66h36"/><rect class="c" x="1" y="87" width="44" height="12"/><rect class="k" x="4" y="90.5" width="5" height="5"/><rect class="b" x="12.5" y="90.5" width="5" height="5"/><rect class="b" x="21" y="90.5" width="5" height="5"/><rect class="b" x="29.5" y="90.5" width="5" height="5"/><rect class="b" x="38" y="90.5" width="5" height="5"/>',
    "guide": '<rect class="c" x="1" y="1" width="44" height="11"/><rect class="k" x="5" y="5" width="12" height="3"/><rect class="s" x="29" y="3.5" width="12" height="6" rx="3"/><rect class="k" x="5" y="19" width="24" height="4"/>'
             '<rect class="s" x="5" y="28" width="36" height="24" rx="4"/><rect class="s" x="5" y="56" width="36" height="24" rx="4"/><rect class="b" x="9" y="33" width="20" height="3"/><rect class="b" x="9" y="61" width="20" height="3"/>'
             '<rect class="c" x="1" y="86" width="44" height="13"/><rect class="k" x="4" y="89.5" width="6" height="6" rx="3"/><rect class="b" x="12.5" y="89.5" width="5.5" height="6" rx="2.7"/><rect class="b" x="21" y="89.5" width="5.5" height="6" rx="2.7"/><rect class="b" x="29.5" y="89.5" width="5.5" height="6" rx="2.7"/><rect class="b" x="38" y="89.5" width="5.5" height="6" rx="2.7"/>',
}
SKETCH = {
    "ledger": "Sketch of Ledger: a two-row masthead above a contents list and ruled sections; on a phone, a row of workspace links and one ruled column.",
    "console": "Sketch of Console: a sidebar, a striped table and a status panel at the right; on a phone, a top strip, a table and a bottom bar with four destinations.",
    "guide": "Sketch of Guide: a top bar with four pill tabs and one menu control above a grid of cards; on a phone, stacked cards and a bottom bar with four destinations.",
}
BLURB = {
    "ledger": ("A record to read", "Two-row masthead, ruled sections, small stamps for state, ink as the only accent. 15 px text, serif titles. On a phone the four workspace links stay in view.", "For reading what happened and being sure of it."),
    "console": ("An operations console", "Grouped sidebar, dense striped tables, status kept beside the work. 13 px text, 28 px controls.", "For watching several repositories through the day."),
    "guide": ("A task-led layout", "Four large destinations, everything else behind one control, roomy sections and big targets. 16 px text, 42 px controls.", "For opening RepoSync now and then to see what needs you."),
}

CSS = """
/* Layout: a reviewer's cover sheet. One left-aligned column; the three options side by side as sketches; documents as a plain table. */
:root {
  --bg: #f2f4f7; --surface: #ffffff; --ink: #131a22; --ink-2: #47525e; --line: #cdd5dd; --line-2: #8793a0;
  --svn: #1b5fa8; --git: #6d3fc4; --note: #7a4f00; --note-bg: #f8eed2; --block: #dfe5eb; --chrome: #e7ecf1; --link: #1b5fa8;
  --sans: system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
  --mono: ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace;
}
@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) {
  --bg: #0f141a; --surface: #171e26; --ink: #e8edf2; --ink-2: #a3afbb; --line: #2c3742; --line-2: #62707e;
  --svn: #7db4f0; --git: #b79bf5; --note: #e9c46a; --note-bg: #33290f; --block: #2a3541; --chrome: #212b35; --link: #8fbdf2; color-scheme: dark } }
:root[data-theme="dark"] {
  --bg: #0f141a; --surface: #171e26; --ink: #e8edf2; --ink-2: #a3afbb; --line: #2c3742; --line-2: #62707e;
  --svn: #7db4f0; --git: #b79bf5; --note: #e9c46a; --note-bg: #33290f; --block: #2a3541; --chrome: #212b35; --link: #8fbdf2; color-scheme: dark }
*, *::before, *::after { box-sizing: border-box; }
body { background: var(--bg); color: var(--ink); font: 15px/1.55 var(--sans); }
.wrap { max-width: 1080px; margin: 0 auto; padding-block: 28px 72px; padding-left: max(16px, env(safe-area-inset-left, 0px)); padding-right: max(16px, env(safe-area-inset-right, 0px)); display: grid; gap: 36px; }
.wrap > * { min-width: 0; }
a { color: var(--link); text-underline-offset: 2px; }
a:focus-visible { outline: 2px solid var(--link); outline-offset: 2px; border-radius: 2px; }
h1 { font: 680 clamp(1.7rem, 4.4vw, 2.4rem)/1.12 var(--sans); letter-spacing: -0.015em; margin: 0; text-wrap: balance; }
h2 { font: 650 1.05rem/1.3 var(--sans); margin: 0; text-wrap: balance; }
h3 { font: 650 1.15rem/1.25 var(--sans); margin: 0; }
p { margin: 0; max-width: 68ch; }
.head { display: grid; gap: 14px; }
.eyebrow { display: flex; flex-wrap: wrap; gap: 6px 14px; align-items: center; font: 600 0.8rem/1.4 var(--mono); color: var(--ink-2); }
.eyebrow .dot { width: 9px; height: 9px; border-radius: 50%; display: inline-block; }
.pairmark { display: inline-flex; gap: 3px; }
.lede { font-size: 1.08rem; color: var(--ink-2); }
.status { background: var(--note-bg); color: var(--ink); border-left: 4px solid var(--note); padding: 10px 14px; max-width: 72ch; }
.status strong { color: var(--note); }
.start { display: flex; flex-wrap: wrap; gap: 10px 12px; list-style: none; padding: 0; margin: 0; }
.start a { display: inline-block; padding: 9px 16px; border: 1px solid var(--line-2); background: var(--surface); color: var(--ink); text-decoration: none; font-weight: 620; }
.start a:hover { border-color: var(--ink); }
.start li:first-child a { background: var(--ink); color: var(--bg); border-color: var(--ink); }
section { display: grid; gap: 14px; }
.sec-h { display: flex; flex-wrap: wrap; gap: 4px 14px; align-items: baseline; border-top: 2px solid var(--ink); padding-top: 10px; }
.sec-h span { color: var(--ink-2); font-size: 0.9rem; }
.opts { display: grid; gap: 16px; grid-template-columns: repeat(auto-fit, minmax(min(100%, 290px), 1fr)); }
.opt { background: var(--surface); border: 1px solid var(--line); padding: 16px; display: grid; gap: 12px; align-content: start; min-width: 0; }
.opt .id { font: 600 0.78rem var(--mono); color: var(--ink-2); }
.opt p { color: var(--ink-2); font-size: 0.94rem; }
.sk { display: flex; gap: 10px; align-items: flex-end; }
.sk svg { display: block; height: auto; max-width: 100%; }
.sk svg:first-child { flex: 160 1 0; min-width: 0; } .sk svg:last-child { flex: 46 1 0; min-width: 0; max-width: 62px; }
.sk .f { fill: var(--bg); stroke: var(--line-2); stroke-width: 1; } .sk .c { fill: var(--chrome); } .sk .b { fill: var(--block); } .sk .k { fill: var(--ink); } .sk .s { fill: var(--surface); stroke: var(--line); stroke-width: 0.8; }
.sk .r { stroke: var(--line-2); stroke-width: 0.7; fill: none; }
.links { display: flex; flex-wrap: wrap; gap: 4px 14px; list-style: none; padding: 0; margin: 0; font-size: 0.92rem; }
.facts { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 230px), 1fr)); gap: 12px 28px; margin: 0; }
.facts div { border-top: 1px solid var(--line); padding-top: 8px; }
.facts dt { color: var(--ink-2); font-size: 0.86rem; }
.facts dd { margin: 0; font-weight: 620; font-variant-numeric: tabular-nums; }
.tw { overflow-x: auto; }
table { border-collapse: collapse; width: 100%; font-size: 0.93rem; }
th, td { text-align: left; vertical-align: top; padding: 8px 14px 8px 0; border-bottom: 1px solid var(--line); }
th { font-weight: 650; border-bottom-color: var(--ink); white-space: nowrap; }
td:first-child { font-weight: 620; }
td .sub { display: block; font-weight: 400; color: var(--ink-2); }
ol.dec { margin: 0; padding-left: 1.4em; display: grid; gap: 8px; }
ol.dec li { max-width: 74ch; padding-left: 4px; }
.two { display: grid; gap: 16px 32px; grid-template-columns: repeat(auto-fit, minmax(min(100%, 320px), 1fr)); }
.two > div { display: grid; gap: 8px; align-content: start; min-width: 0; }
.two ul { margin: 0; padding-left: 1.2em; display: grid; gap: 6px; color: var(--ink-2); }
.two h3 { font-size: 0.98rem; }
.ident { display: grid; gap: 0; margin: 0; }
.ident div { display: grid; grid-template-columns: minmax(0, 15rem) minmax(0, 1fr); gap: 4px 18px; padding: 8px 0; border-bottom: 1px solid var(--line); }
.ident dt { color: var(--ink-2); }
.ident dd { margin: 0; font: 0.84rem/1.5 var(--mono); overflow-wrap: anywhere; }
@media (max-width: 560px) { .ident div { grid-template-columns: minmax(0, 1fr); } }
code { font: 0.88em var(--mono); background: var(--chrome); padding: 0.05em 0.3em; overflow-wrap: anywhere; }
footer { color: var(--ink-2); font-size: 0.88rem; border-top: 1px solid var(--line); padding-top: 12px; }
"""


def sketch(o):
    return (f'<div class="sk" role="img" aria-label="{SKETCH[o]}">'
            f'<svg viewBox="0 0 160 100" width="160" height="100" aria-hidden="true"><rect class="f" x="0.5" y="0.5" width="159" height="99"/>{WIDE[o]}</svg>'
            f'<svg viewBox="0 0 46 100" width="46" height="100" aria-hidden="true"><rect class="f" x="0.5" y="0.5" width="45" height="99" rx="4"/>{PHONE[o]}</svg></div>')


def option(o):
    name = docs_text.kit.OPTIONS[o]["name"]
    idea, what, who = BLURB[o]
    base = f"options/{o}"
    return (f'<article class="opt"><div><h3>{name}</h3><span class="id">layout id: {o}</span></div>{sketch(o)}'
            f'<p><strong>{idea}.</strong> {what} {who}</p>'
            f'<ul class="links"><li><a href="{base}/screens/g02-dashboard.html#light">Dashboard, light</a></li><li><a href="{base}/screens/g02-dashboard.html#dark">Dashboard, dark</a></li>'
            f'<li><a href="{base}/screens/g04-overview.html">Repository</a></li><li><a href="{base}/flows/index.html">Flows</a></li><li><a href="{base}/README.html">About {name}</a></li></ul></article>')


def page():
    R = docs.rows()
    counts = {k: sum(1 for r in R if r["status"] == k) for k in ("covered", "explicit gap", "not applicable")}
    by = {k: sum(1 for r in R if r["delivery"] == k) for k in docs_text.DELIVERY}
    files = [p for p in OUT.rglob("*") if p.is_file()]
    sums = sha(OUT / "SHA256SUMS")
    n, flows = len(build.S), len(build.FLOWS)
    docs_rows = [
        ("Surface and state matrix", f"{len(R)} rows: every route and every surface without a route, with source lines at both epics", "matrix/surface-state-matrix.html", [("CSV", "matrix/surface-state-matrix.csv"), ("JSON", "matrix/surface-state-matrix.json"), ("Markdown", "matrix/surface-state-matrix.md")]),
        ("Capability parity map", "every capability and how each option carries it", "matrix/capability-parity.html", [("Markdown", "matrix/capability-parity.md")]),
        ("Legacy navigation and settings crosswalk", "today's navigation, sections, settings and labels, and where each sits in the new options", "crosswalk/legacy-navigation-settings.html", [("Markdown", "crosswalk/legacy-navigation-settings.md")]),
        ("Design-system contract", "tokens on the #262 seam, type, responsive rules, components, focus, computed contrast", "contracts/design-system.html", [("Markdown", "contracts/design-system.md"), ("Contrast CSV", "contracts/contrast-computed.csv")]),
        ("Interaction contract", "kinds of action, state tables, validation, permissions, receipts, held capabilities", "contracts/interaction.html", [("Markdown", "contracts/interaction.md")]),
        ("Implementation map for #264", "what to build on, shell changes E1 to E4 and E6, order of work, test oracles", "contracts/implementation-map.html", [("Markdown", "contracts/implementation-map.md")]),
        ("Colour bridge", "how existing pages take colour from the tokens; every colour class in the source and its contrast", "contracts/colour-bridge.html", [("Markdown", "contracts/colour-bridge.md"), ("Classes CSV", "contracts/colour-bridge-classes.csv"), ("Pairs CSV", "contracts/colour-bridge-pairs.csv")]),
        ("Page regions", "the fixed region tree that lets a layout rearrange a page without losing work", "contracts/page-regions.html", [("Markdown", "contracts/page-regions.md"), ("JSON", "contracts/page-regions.json")]),
        ("Decisions", "what was decided on 2026-11-16, by whom, and what is still open", "DECISIONS.html", [("Markdown", "DECISIONS.md")]),
        ("Changes from v1.5, by item", "the final check's items K1 to K6, with what changed and where", "CHANGES-FROM-v1.5.html", [("Markdown", "CHANGES-FROM-v1.5.md")]),
        ("Changes from v1.4, by item", "the closing check's items M1 to M8, with what changed and where", "CHANGES-FROM-v1.4.html", [("Markdown", "CHANGES-FROM-v1.4.md")]),
        ("Changes from v1.3, by item", "the follow-up review's items N1 to N13 and the residuals, with what changed and where", "CHANGES-FROM-v1.3.html", [("Markdown", "CHANGES-FROM-v1.3.md")]),
        ("Changes from v1, by finding", "the review findings F1 to F14 and O1 to O5, with what changed and where", "CHANGES-FROM-v1.html", [("Markdown", "CHANGES-FROM-v1.md")]),
        ("What changed", "from each earlier version", "CHANGES.html", [("Markdown", "CHANGES.md")]),
        ("Package index", "contents, provenance and limits in full", "README.html", [("Markdown", "README.md"), ("SHA256SUMS", "SHA256SUMS")]),
        ("Earlier versions", "v1 to v1.5, private Artifacts recorded by checksum; none is in the repository", "history/README.html", [("Markdown", "history/README.md")]),
        ("Tools", "the scripts that generated and checked the tree", "tools/README.html", []),
    ]
    rows_html = "".join(
        f'<tr><td><a href="{href}">{t}</a><span class="sub">{d}</span></td><td>' + (", ".join(f'<a href="{h}">{l}</a>' for l, h in extra) or "none") + "</td></tr>"
        for t, d, href, extra in docs_rows)
    decisions = [
        "<strong>All three options go to #264</strong>, to the full extent the prototypes show.",
        "<strong>Dark until chosen</strong>, in every layout. Light is offered in all six, including Classic, Individual and Team.",
        "<strong>Five changes to the #262 shell are accepted</strong> (E1 to E4 and E6): a “More” control after the last workspace link, a saved theme, wording per layout family, a Help link in the new layouts, and navigation icons.",
        "<strong>The sign-in page stays as it is</strong>: dark, for everyone.",
        "<strong>Every shared change the prototypes draw is adopted</strong>, in every layout.",
        "<strong>The markup inside every page is the same in all three options.</strong> A layout arranges a page and never renders a different one.",
        "<strong>Three labels that misstate what a control does are corrected in every layout.</strong> Every other label in Classic, Individual and Team stays.",
        "<strong>Pages keep today's order of regions</strong> in every layout. The new options fit that order and add read-only summaries at the top, so Classic's keyboard order does not change.",
        "<strong>Repository sections are addressable</strong> with a parameter on the existing route.",
        "<strong>Held and missing items stay as they are</strong>: #69 refresh and re-anchor, #73 conflict application, credential origin, resuming a stopped import.",
    ]
    return f"""<title>RepoSync #263 design package v1.6</title>
<meta name="claude:viewport-fit-sides" content="cover">
<style>{CSS}</style>
<div class="wrap">
<header class="head">
  <p class="eyebrow"><span class="pairmark" aria-hidden="true"><span class="dot" style="background:var(--svn)"></span><span class="dot" style="background:var(--git)"></span></span><span>issue #263</span><span>v1.6</span><span>2026-11-16</span><span>the accepted design</span></p>
  <h1>RepoSync #263 design package v1.6</h1>
  <p class="lede">Three new whole-interface options for RepoSync, all accepted for #264. Each covers every page in dark and light, on a wide screen and a phone, with the matrix, crosswalk and contracts #264 builds from.</p>
  <p class="status"><strong>Design only.</strong> Nothing here is in the product. Classic, Individual and Team stay available and stay the default; what changes for them is listed in the decisions. #263 cannot close #264. The publisher rendered v1 to v1.5, and v1, v1.3, v1.4 and v1.5 were reviewed independently; this version makes the changes those reviews required. The publisher captures this version after delivery.</p>
  <ul class="start"><li><a href="prototypes/index.html">Open all screens</a></li><li><a href="DECISIONS.html">Decisions</a></li><li><a href="matrix/surface-state-matrix.html">Surface and state matrix</a></li><li><a href="CHANGES-FROM-v1.5.html">Changes since v1.5</a></li></ul>
</header>

<section aria-labelledby="h-opt">
  <div class="sec-h"><h2 id="h-opt">The three options</h2><span>Same pages, labels, controls and states. Different navigation, density, type and phone pattern.</span></div>
  <div class="opts">{option("ledger")}{option("console")}{option("guide")}</div>
</section>

<section aria-labelledby="h-cov">
  <div class="sec-h"><h2 id="h-cov">Coverage</h2><span>From the matrix</span></div>
  <dl class="facts">
    <div><dt>Matrix rows</dt><dd>{len(R)}: {counts["covered"]} covered, {counts["explicit gap"]} explicit gap, {counts["not applicable"]} not applicable</dd></div>
    <div><dt>Prototype pages</dt><dd>{n} per option, {n * 3} in all, plus {flows} flows per option</dd></div>
    <div><dt>For #264</dt><dd>{by["layout only"]} layout only, {by["shell extension"]} shell extension, {by["shared change, with the layouts"] + by["shared change, own change"]} shared change, {by["waits for an owner"]} waiting for an owner, {by["feature work"]} feature work outside #264</dd></div>
    <div><dt>Where the two epics differ</dt><dd>3 rows, each describing both</dd></div>
  </dl>
</section>

<section aria-labelledby="h-doc">
  <div class="sec-h"><h2 id="h-doc">Documents</h2><span>Each opens as a page; the data files are beside it</span></div>
  <div class="tw"><table><thead><tr><th scope="col">Document</th><th scope="col">Other formats and files</th></tr></thead><tbody>{rows_html}</tbody></table></div>
</section>

<section aria-labelledby="h-dec">
  <div class="sec-h"><h2 id="h-dec">Decided</h2><span>2026-11-16. The full record is in <a href="DECISIONS.html">Decisions</a></span></div>
  <ol class="dec">{"".join(f"<li>{d}</li>" for d in decisions)}</ol>
</section>

<section aria-labelledby="h-chk">
  <div class="sec-h"><h2 id="h-chk">What was checked, and what was not</h2></div>
  <div class="two">
    <div><h3>Checked on the files</h3><ul>
      <li>Every page: balanced tags, no duplicate ids, every link resolves inside the tree, nothing loaded from outside it.</li>
      <li>Every token used has a value in each option and theme.</li>
      <li>Contrast computed from token values: 180 pairs for the three options and 30 for the existing layouts in light, all at or above target.</li>
      <li>Colour bridge: 211 colour classes in the source resolved by rule except 3; 49 background and text pairs computed in seven token sets, none below 4.5:1.</li>
      <li>Every matrix row: source text found at both epic revisions, every referenced page exists, no blank cell.</li></ul></div>
    <div><h3>Not done</h3><ul>
      <li>No visual review of every page. The designer's sandboxed render check measures errors, page width and selected elements against their boxes, and found no fault; it does not judge how a page looks. The publisher rendered v1 to v1.5 and captures this version after delivery.</li>
      <li>No build, test or application launch. No real application evidence.</li>
      <li>No screen reader, keyboard walk-through or accessibility scan. No accessibility pass is claimed. “Tab order equals reading order” describes the markup as written; it was not tested.</li>
      <li>No usability testing.</li></ul></div>
  </div>
</section>

<section aria-labelledby="h-id">
  <div class="sec-h"><h2 id="h-id">Identity</h2><span>For the record in #264</span></div>
  <dl class="ident">
    <div><dt>Source read, partial-52</dt><dd>{Z.P}</dd></div>
    <div><dt>Source read, reliability-61</dt><dd>{Z.R}</dd></div>
    <div><dt>Files in the tree</dt><dd>{len(files)}, including SHA256SUMS</dd></div>
    <div><dt>SHA-256 of SHA256SUMS</dt><dd>{sums}</dd></div>
    <div><dt>Earlier versions</dt><dd>v1 to v1.5 are private Artifacts, listed with their checksums in <a href="history/README.html">history/README</a>. None is in the repository.</dd></div>
    <div><dt>Repository path proposed</dt><dd>docs/design/263/v1.6/, with the publisher's captures at docs/design/263/captures/v1.6/</dd></div>
  </dl>
</section>

<footer>This cover page is the Artifact's front page. It is not part of the checksummed tree; <a href="README.html">the package index</a> is. All data in the prototypes is invented.</footer>
</div>
"""


if __name__ == "__main__":
    html = page()
    DEST.write_text(html)
    c = build.Check()
    c.feed(html)
    left = [t for t in c.stack if t not in build.IMPL]
    missing = [r for r in c.refs if not r.startswith("#") and not (OUT / r.split("#")[0]).exists()]
    print("cover bytes", len(html), "structure problems", c.errs[:5], left[:5], "missing links", missing)
