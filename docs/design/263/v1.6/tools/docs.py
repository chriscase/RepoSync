"""Write the #263 package documents: matrix (CSV, JSON, Markdown, HTML), parity map, crosswalk,
contracts and READMEs. Run after build.py:  python3 gen/docs.py v1
"""
import csv
import io
import json
import pathlib
import re
import subprocess
import sys

GEN = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(GEN))
import build  # noqa: E402
import docs_text  # noqa: E402
import v11  # noqa: E402
import kit  # noqa: E402
import matrix_data as M  # noqa: E402
import mdhtml  # noqa: E402
import resolve as Z  # noqa: E402
import tokens  # noqa: E402

OUT = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "v1").resolve()
OPTS = list(kit.OPTIONS)
THEMES = ["light", "dark"]
VIEWS = ["1440x900", "390x844"]
SHORT_P, SHORT_R = Z.P[:7], Z.R[:7]
SLUGS = {s[0] for s in build.S}
KIND_WORDS = {
    M.E: "Kept as it is",
    M.D: "Kept. The page draws a correction that is separate work; without it the option shows today's behaviour",
    M.P: "Kept, with the wording or presentation proposal in the note",
    M.H: "Shown as held or unavailable, with no control that acts",
    M.N: "A gap. Drawn and marked, and not counted as parity",
}


def identical(path):
    a = subprocess.run(["git", "-C", Z.REPO, "rev-parse", f"{Z.P}:{path}"], capture_output=True, text=True).stdout.strip()
    b = subprocess.run(["git", "-C", Z.REPO, "rev-parse", f"{Z.R}:{path}"], capture_output=True, text=True).stdout.strip()
    return bool(a) and a == b


def rows():
    anchors, bad = Z.resolve()
    assert not bad, bad
    out = []
    for r in M.ROWS:
        a = anchors[r["id"]]
        same = identical(a["path"])
        for s in r["slugs"]:
            assert s in SLUGS, (r["id"], s)
        if r["status"] == "not applicable":
            assert not r["slugs"] and r["note"] != "none", r["id"]
        else:
            assert r["slugs"], r["id"]
        if r["status"] == "explicit gap":
            assert r["note"] != "none", r["id"]
        if r["epic"] == "both":
            epic = "both epics; file identical at both revisions" if same else "both epics; the file differs between them, this surface does not"
        else:
            epic = r["epic"]
        refs = {}
        for o in OPTS:
            refs[o] = {}
            for t in THEMES:
                refs[o][t] = {}
                for v in VIEWS:
                    refs[o][t][v] = ([f"options/{o}/screens/{s}.html#{t}" for s in r["slugs"]] if r["slugs"] else [f"not applicable: {r['note']}"])
        reason = {"covered": "covered: shown in the listed prototype pages", "explicit gap": f"explicit gap: {r['note']}",
                  "not applicable": f"not applicable: {r['note']}"}[r["status"]]
        out.append(dict(r, path=a["path"], line_p=a["p"], line_r=a["r"], identical=same, epic_text=epic, refs=refs, reason=reason, delivery=docs_text.delivery(r)))
    return out


def source_cell(r):
    if r["line_p"] == r["line_r"] and r["anchor"] == r["anchor_r"]:
        where = f"`{r['path']}:{r['line_p']}` at both"
    else:
        where = f"`{r['path']}:{r['line_p']}` at partial-52, `:{r['line_r']}` at reliability-61"
    anchor = code(r["anchor"]) if r["anchor"] == r["anchor_r"] else f"{code(r['anchor'])} (partial-52), {code(r['anchor_r'])} (reliability-61)"
    return f"{where}<br>symbol or text: {anchor}"


def code(s):
    return f"`` {s} ``" if "`" in s else f"`{s}`"


def esc(s):
    """Text for a Markdown table cell: pipes escaped, angle brackets outside code spans written as entities."""
    parts = re.split(r"(``.+?``|`[^`]+`|<br>|\[[^\]]+\]\([^)\s]+\))", str(s).replace("\n", " "))
    out = [p if i % 2 else p.replace("<", "&lt;").replace(">", "&gt;") for i, p in enumerate(parts)]
    return "".join(out).replace("|", "\\|")


def page_links(r, base="../"):
    if not r["slugs"]:
        return "none"
    return "<br>".join(f"`{s}` " + " ".join(f"[{o[0].upper()}]({base}options/{o}/screens/{s}.html)" for o in OPTS) for s in r["slugs"])


def write(rel, text):
    p = OUT / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)
    return p


def write_md(rel, md, title):
    write(rel, md)
    up = "../" * rel.count("/")
    write(rel[:-3] + ".html", mdhtml.page(md, title, up))


# ---------------------------------------------------------------- matrix
def matrix(R):
    counts = {k: sum(1 for r in R if r["status"] == k) for k in ("covered", "explicit gap", "not applicable")}
    kinds = {k: sum(1 for r in R if r["kind"] == k) for k in (M.E, M.D, M.P, M.H, M.N)}
    cols = ["id", "route_or_scope", "surface", "epic_scope", f"source_partial52_{SHORT_P}", f"source_reliability61_{SHORT_R}", "source_symbol_or_text",
            "current_label_and_path", "role_and_mode_conditions", "action_and_api_boundary", "applicable_states", "kind", "delivery_for_264", "status", "status_reason", "note"]
    refcols = [(o, t, v) for o in OPTS for t in THEMES for v in VIEWS]
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\n")
    w.writerow(cols + [f"{o}_{t}_{v}" for o, t, v in refcols])
    for r in R:
        line = [r["id"], r["route"], r["surface"], r["epic_text"], f"{r['path']}:{r['line_p']}", f"{r['path']}:{r['line_r']}",
                r["anchor"] if r["anchor"] == r["anchor_r"] else f"{r['anchor']} (partial-52); {r['anchor_r']} (reliability-61)",
                r["label"], r["role"], r["api"], r["states"], r["kind"], r["delivery"], r["status"], r["reason"], r["note"]]
        line += [" ; ".join(r["refs"][o][t][v]) for o, t, v in refcols]
        assert all(str(c).strip() for c in line), r["id"]
        w.writerow(line)
    write("matrix/surface-state-matrix.csv", buf.getvalue())
    doc = {"package": "RepoSync #263 design package v1.6", "date": "2026-11-16",
           "revisions": {"epic/reposync-partial-52": Z.P, "epic/reposync-reliability-61": Z.R},
           "options": OPTS, "themes": THEMES, "viewports": VIEWS,
           "reference": "Each reference is one responsive prototype page and a theme. Open it at the stated viewport. The package carries no image captures; the publisher makes them.",
           "counts": {"rows": len(R), **counts}, "kinds": kinds,
           "rows": [{"id": r["id"], "route_or_scope": r["route"], "surface": r["surface"], "epic_scope": r["epic_text"],
                     "source": {"file": r["path"], "line_partial52": r["line_p"], "line_reliability61": r["line_r"], "symbol_or_text_partial52": r["anchor"],
                                "symbol_or_text_reliability61": r["anchor_r"], "file_identical_between_epics": r["identical"]},
                     "current_label_and_path": r["label"], "role_and_mode_conditions": r["role"], "action_and_api_boundary": r["api"],
                     "applicable_states": [s.strip() for s in r["states"].split(";")], "kind": r["kind"], "delivery_for_264": r["delivery"], "status": r["status"], "status_reason": r["reason"],
                     "note": r["note"], "prototype_pages": r["slugs"], "references": r["refs"]} for r in R]}
    write("matrix/surface-state-matrix.json", json.dumps(doc, indent=1, ensure_ascii=False) + "\n")

    md = [f"# Surface and state matrix", "",
          f"{len(R)} rows: every route G01 to G13 and every surface that has no route of its own. "
          f"**{counts['covered']} covered, {counts['explicit gap']} explicit gap, {counts['not applicable']} not applicable.** No cell is blank.", "",
          "The same rows are in [surface-state-matrix.csv](surface-state-matrix.csv) and [surface-state-matrix.json](surface-state-matrix.json). "
          "Those two files carry the twelve reference columns in full: three options, light and dark, 1440×900 and 390×844.", "",
          "## How to read it", "",
          f"- **Epic and source.** Line numbers were resolved by script against `{Z.P}` (epic/reposync-partial-52) and `{Z.R}` (epic/reposync-reliability-61). "
          "The line is the first occurrence of the quoted symbol or text in the file at that revision. Paths without a root are under `web-ui/src/`.",
          "- **Where the epics differ.** Three rows: G02.8 and G04.16 (commit map target cell) and G04.6 (path policy editor). Each describes both.",
          "- **Prototype pages.** Each page name exists under all three options. L, C and G open the Ledger, Console and Guide page. "
          "Add `#light` or `#dark` to the address to fix the theme; add `-nobar` (for example `#dark-nobar`) to hide the grey prototype bar.",
          "- **References are pages.** Each reference is one responsive page. The 1440×900 and 390×844 columns hold the same address because the width is the viewport, not a different file. "
          "The publisher's captures of a page are named `<option>_<page>__<theme>__<wide|narrow>.png`, for example `ledger_g04-overview__dark__narrow.png`; the package itself carries no pictures.",
          "- **Kind** says how the drawn page relates to today's product:", ""]
    for k in (M.E, M.D, M.P, M.H, M.N):
        md.append(f"  **{k}** ({kinds[k]} rows). {KIND_WORDS[k]}.")
        md.append("")
    md += ["- **For #264** says what kind of work the row is: *layout only*, *shell extension* (one of four accepted changes to the #262 shell), *shared change, with the layouts* (display only), "
           "*shared change, own change* (changes when a call is made or adds a step), *waits for an owner*, or *feature work* (outside #264). See the implementation map.",
           "- **Status.** *Covered*: the surface and its states are drawn in every option, theme and size. *Explicit gap*: the product cannot do this today, or a decision is open; "
           "the page marks it and the note says why. *Not applicable*: nothing to draw, with the reason.", "",
           "## Rows", "",
           "| ID | Route or scope | Surface | Epic | Source | Current label and path | Role and mode | Action and API boundary | States | Kind | For #264 | Prototype pages | Status | Note |",
           "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for r in R:
        md.append("| " + " | ".join([r["id"], f"`{r['route']}`" if r["route"].startswith("/") else r["route"], esc(r["surface"]), esc(r["epic_text"]), esc(source_cell(r)),
                                     esc(r["label"]), esc(r["role"]), esc(r["api"]), esc(r["states"]), r["kind"], r["delivery"], page_links(r), f"**{r['status']}**", esc(r["note"])]) + " |")
    md += ["", "## Pages no row points to", ""]
    used = {s for r in R for s in r["slugs"]}
    spare = sorted(SLUGS - used)
    md.append("None. Every prototype page is referenced by at least one row." if not spare else "These pages are extra views: " + ", ".join(f"`{s}`" for s in spare) + ".")
    write_md("matrix/surface-state-matrix.md", "\n".join(md) + "\n", "Surface and state matrix")
    return counts, kinds, spare


# ---------------------------------------------------------------- parity
VARIANT = {
    "G02.3": ("Ruled table", "Dense table in the right-hand column", "Roomy table"),
    "G02.4": ("After Repositories, as today", "After Repositories, in the right-hand column", "After Repositories, as today"),
    "G02.11": ("First, as a ruled list", "First in the document; a left column from 1101 px", "First, as a roomy list"),
    "G03.1": ("Ruled table", "Dense table", "Roomy table"),
    "G04.1": ("Header, status band, contents list at the left", "Header, section tabs, status inspector at the right", "Header, status card, section chips"),
    "G04.11": ("Status band under the header", "Status inspector fixed at the right (from 1101 px)", "Status card under the header"),
    "S01": ("Two-row masthead", "Left sidebar with three labelled groups", "Four pill tabs and one “Account and admin” control"),
    "S03": ("Top row, right", "Foot of the sidebar; top strip on a phone", "Top bar"),
    "S05": ("Second row, right; behind “More” on a phone", "Foot of the sidebar; behind “More” on a phone", "Behind “Account and admin”"),
    "S06": ("Second row, right; behind “More” on a phone", "Foot of the sidebar; behind “More” on a phone", "Behind “Account and admin”"),
    "S10": ("“More” at the end of the workspace row, up to 860 px", "“More” as the fifth item of the bottom bar, up to 900 px", "“Account and admin” at the end of the tab row; “More” in the bottom bar up to 760 px"),
}


def parity(R):
    md = ["# Capability parity map", "",
          "Every capability the product has at the two epic tips, and where each option carries it. The three options render the same page content with the same markup: "
          "each page is written once and placed in each option's shell, and the build checks that the markup inside every page is identical. So a capability is either in all three or in none. "
          "What differs between options is navigation, density, type and how a page's sections are arranged, never what a person can do.", "",
          "This is parity of the design. It is not evidence about the implemented product; #264 has to prove it there.", "",
          "## Reading the table", "",
          "- **Result** is the same for all three options and comes from the matrix row's kind."]
    for k in (M.E, M.D, M.P, M.H, M.N):
        md.append(f"- *{k}*. {KIND_WORDS[k]}.")
    md += ["- **Ledger, Console, Guide** say how that option presents it when the options differ; otherwise “same”. The link opens the first prototype page for the row.", "",
           "## Summary", ""]
    by = {}
    for r in R:
        by.setdefault(r["kind"], []).append(r["id"])
    md += ["| Result | Rows | IDs |", "|---|---|---|"]
    for k in (M.E, M.P, M.D, M.H, M.N):
        md.append(f"| {k} | {len(by.get(k, []))} | {', '.join(by.get(k, []))} |")
    md += ["", "No capability is dropped, merged away or moved to another route in any option. Two capabilities move within their page or shell in all three options: "
           "the server monitor sits collapsed at the end of the repository overview (G04.29), and for a repository the one-click sync switch in the header becomes “Pause sync…” using the explicit call, while a branch pair keeps the switch (G04.23).", "",
           "## Capabilities", "",
           "| ID | Capability | Today: control and call | Result in all three options | Ledger | Console | Guide |", "|---|---|---|---|---|---|---|"]
    for r in R:
        cells = []
        for i, o in enumerate(OPTS):
            v = VARIANT.get(r["id"], ("same", "same", "same"))[i]
            if r["status"] == "not applicable":
                cells.append("not applicable")
            else:
                cells.append(f"{v} · [open](../options/{o}/screens/{r['slugs'][0]}.html)")
        result = r["kind"] + ("" if r["note"] == "none" else f": {r['note']}")
        md.append("| " + " | ".join([r["id"], esc(r["surface"]), esc(f"{r['label']} · {r['api']}"), esc(result)] + cells) + " |")
    write_md("matrix/capability-parity.md", "\n".join(md) + "\n", "Capability parity map")


# ---------------------------------------------------------------- crosswalk
NAVX = [  # legacy label, route, legacy group, who, new label, new group
    ("Dashboard", "/", "Workspace", "everyone", "Dashboard", "Workspace"),
    ("Repositories", "/repos", "Workspace", "everyone", "Repositories", "Workspace"),
    ("Conflicts (with count)", "/conflicts", "Workspace", "everyone", "Conflicts (with count)", "Workspace"),
    ("Audit Log", "/audit", "Workspace", "everyone", "Audit log", "Workspace"),
    ("Settings", "/settings", "Account", "everyone", "My account", "Account"),
    ("none: no link exists", "/docs", "none", "everyone", "Help", "Account"),
    ("Users", "/users", "Administration", "administrators", "Users", "Administration"),
    ("LDAP", "/ldap", "Administration", "administrators", "Directory (LDAP)", "Administration"),
    ("Config", "/config", "Administration", "administrators", "Identity & notifications", "Administration"),
    ("Setup", "/setup", "Administration", "administrators", "Server setup", "Administration"),
]
PLACE = {  # group -> (ledger wide, ledger narrow, console wide, console narrow, guide wide, guide narrow)
    "Workspace": ("Masthead, first row", "Second row, always visible", "Sidebar, first group", "Bottom bar", "Pill tabs in the top bar", "Bottom bar"),
    "Account": ("Masthead, second row", "Behind “More”, first group", "Sidebar, second group", "Behind “More”", "Behind “Account and admin”, left column", "Behind “More”"),
    "Administration": ("Masthead, second row, after the rule", "Behind “More”, second group", "Sidebar, third group", "Behind “More”", "Behind “Account and admin”, right column", "Behind “More”"),
}
SHELLX = [
    ("Logo and “RepoSync”", "Start of the bar", "Start of the masthead", "Head of the sidebar; top strip on a phone", "Start of the top bar", "Unchanged"),
    ("Sync status (dot and sentence)", "Account side of the bar", "First row, right", "Foot of the sidebar; top strip on a phone", "Top bar, beside the menu control", "Mark and word instead of a dot; same read"),
    ("Account name", "Account side", "Second row, right; behind “More” on a phone", "Foot of the sidebar; behind “More” on a phone", "Behind “Account and admin”", "Unchanged"),
    ("Layout select (label “Layout”)", "Account side", "Second row, right; behind “More” on a phone", "Foot of the sidebar; behind “More” on a phone", "Behind “Account and admin”", "Three entries added; existing three unchanged"),
    ("none: no theme control", "none", "Beside the layout select", "Beside the layout select", "Beside the layout select", "New control in all six layouts: Dark, Light. Dark until chosen. Needs a preference record change"),
    ("“Logout”", "Account side", "Second row, far right; behind “More” on a phone", "Foot of the sidebar; behind “More” on a phone", "Behind “Account and admin”", "“Sign out” in the plain vocabulary; Classic, Individual and Team keep “Logout”"),
    ("none: links scroll sideways or overflow on a phone", "none", "“More” at the end of the workspace row, up to 860 px", "“More” as the fifth item of the bottom bar, up to 900 px", "“Account and admin” at the end of the tab row; “More” in the bottom bar up to 760 px",
     "New shell control after the last workspace link, present in every layout and hidden where unused"),
]
SECTX = [
    ("/", "Repository scope select; “All Repositories”", "Repository scope select; “All repositories”", "Top of the page", "Same `?repo_id=` parameter"),
    ("/", "Import progress cards (top of page)", "“In flight”", "After Needs attention; first of today's regions, as now", "Also lists running operations from the status read"),
    ("/", "“Repositories” with “Manage Repositories →”", "“Repositories” with “Manage repositories”", "After In flight, as today", "A table in every option, with State as the second column"),
    ("/", "“Effective sync health” panel and the cards “Total Syncs”, “Active Conflicts”, “Errors (24h)”, “Uptime”, “Last Sync”", "“Sync health”: the same five values and the acknowledgement", "After Repositories, as today", "Uptime shows “Unavailable” when the server does not report it, as today"),
    ("/", "none: attention is spread across cards", "“Needs attention”, headed by the count and the observed time", "First, under the page head", "New and read-only, from the same status reads. Hidden in Classic, Individual and Team"),
    ("/", "“Sync Position” (one repository in scope)", "“Sync health and position”: “Last SVN revision”, “Last Git hash”, “Last sync”", "After the repository, as today", "Same values"),
    ("/", "“Recent Sync Records”", "“Recent sync records”", "Lower block", "Failed read is drawn as failed, not as empty"),
    ("/", "“Commit Map (SVN ↔ Git)”", "“Commit map (SVN ⇄ Git)”", "Lower block", "No-target wording applies on partial-52 only"),
    ("/", "“Recent Activity”", "“Recent activity” with “Open audit log”", "After Commit map, as today", "Same read"),
    ("/", "Server panel (collapsible)", "“Server”", "Last, as today", "Also on the repository page, collapsed"),
    ("/repos", "“Repositories” with “Add Repository”", "“Repositories” with “Add repository”", "Page head", "Opens the same dialog"),
    ("/repos", "Dialog “Add Repository”", "“Add repository”: Repository, SVN source, Git target, Initial SVN import, Sync settings", "Same route; the list stays behind it", "Field names unchanged; see the settings table"),
    ("/repos/:id", "Header: name, “Sync configured enabled/disabled”, “Turn sync off/on”, “Edit”, “Git Hook”, “Trigger Sync”", "Header: name, “Sync enabled/paused”, “Request sync”, “Pause sync…”", "Page head", "“Edit” moves to each settings section; “Git Hook” moves to Path policy"),
    ("/repos/:id", "One long page", "Five sections: Overview, Activity, Branch pairs, Operations, Settings", "Section navigation under the header", "Same route; sections are views of one page, addressed with `?section=`"),
    ("/repos/:id", "Status panel and cards", "A read-only status line on every section; “What this status proves” and “Errors” on Overview", "Under the header; Overview", "Same evidence text; the acknowledgement stays with the panel"),
    ("/repos/:id", "“SVN Configuration”", "Settings: “SVN source”", "Settings", "Edit; “Test SVN connection” in the edit view, as today"),
    ("/repos/:id", "“Git Configuration”", "Settings: “Git target”", "Settings", "Edit; “Test Git connection” in the edit view, as today"),
    ("/repos/:id", "Password and token rows inside the two configuration cards", "Settings: “Credentials”", "Settings", "State and origin columns; origin is a gap today"),
    ("/repos/:id", "“Sync Settings”", "Settings: “Sync policy”", "Settings", "Same four values"),
    ("/repos/:id", "Path policy (editor on partial-52; two text areas on reliability-61)", "Settings: “Path policy”", "Settings", "Both variants drawn"),
    ("/repos/:id", "“Quick Stats”", "Settings: “About”", "Settings", "Created, Last updated, Created by, LFS threshold"),
    ("/repos/:id", "“Danger Zone”", "Settings: “Stop or remove”", "Settings, last", "Pause sync, Remove from RepoSync, Reset and reimport (unavailable)"),
    ("/repos/:id", "Paused-after-error banner with “Skip Commit”, “Retry”", "Banner with “Skip commits…”, “Resume sync…”", "Top of Overview while paused", "Same conditions for showing it"),
    ("/repos/:id", "“SVN Import” card", "Operations: “SVN import” and “Current operation”", "Operations", "Start, Stop import…, Verify remote"),
    ("/repos/:id", "“Sync Records”", "Activity: “Sync records”", "Activity; a short list on Overview", "Same read"),
    ("/repos/:id", "“Commit Map (SVN ↔ Git)”", "Activity: “Commit map (SVN ⇄ Git)”", "Activity", "Same read"),
    ("/repos/:id", "“Recent Audit Log”", "Activity: “Recent audit log”", "Activity", "Same read"),
    ("/repos/:id", "“Update pair from parent”", "Branch pairs: “Update pair from parent · preview only”", "Branch pairs", "Held under #69; preview only"),
    ("/repos/:id", "“Re-anchor pair (not implemented)”", "Branch pairs: “Re-anchor”, shown as held", "Branch pairs", "No control that acts"),
    ("/repos/:id", "“Branch Pairs” with “Add Branch Pair”", "“Branch pairs” with “Add branch pair”", "Branch pairs; a short list on Overview", "Row actions in a menu: Open pair, Preview update from parent, Remove pair…"),
    ("/repos/:id", "Dialog “Review Branch Pair”", "“Review branch pair”", "Branch pairs", "Same fields and buttons; “Cancel operation” reads “Stop operation…”"),
    ("/repos/:id", "Server panel", "Overview: “Server”, collapsed", "Overview, last", "Still on this page"),
    ("/conflicts", "Repository select and status buttons", "Same", "Page head", "Same URL parameters"),
    ("/conflicts/:id", "“Side-by-Side Diff”, “Manual Edit” tabs", "“Side-by-side diff”, “Manual edit”, and a “Common base” panel", "Page body", "The base text is already in the read (`base_content`) and is not shown today"),
    ("/conflicts/:id", "“Accept SVN”, “Accept Git”, “Apply Manual Edit”, “Defer”", "“Resolution”: the same three choices, then “Record decision…”, and “Defer”", "Foot of the page", "One confirmation added"),
    ("/audit", "Filters, “Stream” / “Table”", "Same", "Page head", "Same URL parameters"),
    ("/settings", "“My Profile”, “Change Password”, “My Credentials”", "“My profile”, “Change password”, “My credentials”", "One page, three sections", "Page title reads “My account”"),
    ("/users", "Table with Create, Edit, Delete", "Same", "One page", "Delete opens the shared confirmation, as today"),
    ("/ldap", "One form", "“Directory sign-in”, “Connection”, “User mapping”, “Service account (optional)”", "One page, four sections", "Same fields"),
    ("/config", "“Author Identity Mappings”, “Notifications — Microsoft Teams”", "“Author identity mappings”, “Notifications: Microsoft Teams”", "One page, two sections", "Page title reads “Identity & notifications”"),
    ("/setup", "Eight steps: Welcome, SVN Repository, Git Provider, Sync Settings, Identity Mapping, Server & Auth, Review, Import", "Same eight steps, same order", "Step header; the page title reads “Server setup”", "Same fields per step"),
    ("/docs", "Ten sections in a left list", "Same ten sections", "Section list: left in Ledger and Console, chips in Guide", "Page title reads “Help”"),
]
SETX = [  # legacy label, where today, new label, where in the new options, scope, field
    ("Name", "Add Repository dialog; SVN Configuration (edit)", "Name", "Add repository: Repository; Settings: SVN source", "repository; as rows G03.2 and G04.2", "`name`"),
    ("SVN URL", "Add Repository; SVN Configuration", "URL", "Add repository: SVN source; Settings: SVN source", "repository; as rows G03.2 and G04.2", "`svn_url`"),
    ("SVN Branch", "Add Repository; SVN Configuration", "Branch", "same two places", "repository; as rows G03.2 and G04.2", "`svn_branch`"),
    ("SVN Username", "Add Repository; SVN Configuration", "Username", "same two places", "repository; as rows G03.2 and G04.2", "`svn_username`"),
    ("SVN Password", "Add Repository; SVN Configuration", "SVN password", "Add repository: SVN source; Settings: Credentials", "repository; as rows G03.2 and G04.4", "credentials call: `svn_password`"),
    ("Git Provider", "Add Repository; Git Configuration", "Provider", "Add repository: Git target; Settings: Git target", "repository; as rows G03.2 and G04.3", "`git_provider`"),
    ("Git API URL", "Add Repository; Git Configuration", "API URL", "same two places", "repository; as rows G03.2 and G04.3", "`git_api_url`"),
    ("Git Repository (owner/repo)", "Add Repository; Git Configuration", "Repository", "same two places", "repository; as rows G03.2 and G04.3", "`git_repo`"),
    ("Default Branch", "Add Repository; Git Configuration", "Branch", "same two places", "repository; as rows G03.2 and G04.3", "`git_branch`"),
    ("Git Token", "Add Repository; Git Configuration", "Git token", "Add repository: Git target; Settings: Credentials", "repository; as rows G03.2 and G04.4", "credentials call: `git_token`"),
    ("Initial SVN import: Full SVN history / Snapshot at pinned SVN revision", "Add Repository; import card", "Initial SVN import (same two choices)", "Add repository: Initial SVN import; Operations", "repository; as rows G03.4 and G04.13", "import call: `mode`, `revision`"),
    ("Sync Mode", "Add Repository; Sync Settings", "Sync mode", "Add repository: Sync settings; Settings: Sync policy", "repository; as rows G03.2 and G04.5", "`sync_mode`"),
    ("Poll Interval", "Add Repository; Sync Settings", "Poll interval", "same two places", "repository; as rows G03.2 and G04.5", "`poll_interval_secs`"),
    ("LFS Threshold", "Add Repository; Sync Settings; Quick Stats", "LFS threshold", "same two places; Settings: About", "repository; as rows G03.2 and G04.5", "`lfs_threshold_mb`"),
    ("Auto Merge", "Add Repository; Sync Settings", "Auto merge", "same two places", "repository; as rows G03.2 and G04.5", "`auto_merge`"),
    ("Enabled", "Add Repository; header switch “Turn sync off/on”; Danger Zone “Pause / disable sync”", "Enabled (on create); “Pause sync…” and “Turn sync on”", "Add repository: Sync settings; page header; Settings: Stop or remove", "repository; as rows G03.2, G04.22 and G04.23", "`enabled`; disable call"),
    ("Allowed SVN Paths (reliability-61) / Path policy lists (partial-52)", "Edit mode on the repository page", "Allowed paths", "Settings: Path policy", "repository; as row G04.6", "`allowed_paths`"),
    ("Blocked Patterns (reliability-61) / Path policy lists (partial-52)", "Edit mode on the repository page", "Blocked patterns", "Settings: Path policy", "repository; as row G04.6", "`blocked_patterns`"),
    ("Branch Name; SVN Branch Path; include parent; pairing mode", "Dialog “Review Branch Pair”", "Same four", "Branch pairs: Review branch pair", "branch pair; as row G04.21", "branches call"),
    ("Delete Git branch; Delete SVN branch; typed name", "Dialog “Delete branch pair”", "Same three", "Branch pairs: Remove branch pair", "branch pair; as row G04.25", "removal approve call: `delete_git`, `delete_svn`"),
    ("Display Name; Email", "Settings: My Profile", "Display name; Email", "My account: My profile", "own account; as row G08.1", "user update: `display_name`, `email`"),
    ("Current Password; New Password; Confirm", "Settings: Change Password", "Same three", "My account: Change password", "own account; as row G08.3", "user update: `password`, `current_password`"),
    ("SVN Password; SVN Token; Git Token (cards)", "Settings: My Credentials", "SVN password; SVN token; Git token", "My account: My credentials", "own account; as row G08.4", "user credentials calls"),
    ("Username; Display Name; Email; Password; Role", "Users: Create User", "Same five", "Users: Create user", "server; as row G09.2", "user create"),
    ("Display Name; Email; Role; Account enabled", "Users: Edit User", "Same four", "Users: Edit user", "server; as row G09.3", "user update"),
    ("Enable LDAP; LDAP URL; Base DN; Search filter; display name, email and group attributes; Bind DN; Bind password", "LDAP", "Same fields under four headings", "Directory (LDAP)", "server; as rows G10.1 and G10.2", "directory settings call"),
    ("SVN Username; Git Name; Git Email; GitHub", "Config: Author Identity Mappings", "Same four", "Identity & notifications: Author identity mappings", "server; as row G11.1", "identity call (whole list)"),
    ("Teams Webhook URL", "Config: Notifications", "Teams webhook, shown as Set or Not set", "Identity & notifications: Notifications", "server; as rows G11.2 and G11.3", "notifications call: `teams_webhook_url`"),
    ("Setup, step 2: Repository URL; Username; Password; Repository Layout; Trunk, Branches and Tags paths", "Setup: SVN Repository", "Same", "Server setup, step 2", "server; as row G12.2", "setup apply"),
    ("Setup, step 3: Provider; API URL; Repository; Default Branch; Personal Access Token", "Setup: Git Provider", "Same", "Server setup, step 3", "server; as row G12.3", "setup apply"),
    ("Setup, step 4: Sync Mode; Auto Merge; Sync Tags; LFS Threshold; Max File Size; LFS Patterns; Ignore Patterns; pull-request fields", "Setup: Sync Settings", "Same, under “Sync settings”, “File policy and large file storage (LFS)”, “Pull request settings”", "Server setup, step 4", "server; as row G12.4", "setup apply; the page collects the pull-request fields and does not send them today"),
    ("Setup, step 5: Fallback Email Domain; Author Mapping File; inline mappings", "Setup: Identity Mapping", "Same", "Server setup, step 5", "server; as row G12.5", "setup apply"),
    ("Setup, step 6: Listen Address; Authentication Mode; Admin Password Env Var; OAuth fields; Poll Interval; Log Level; Data Directory; Slack and email fields", "Setup: Server & Auth", "Same, under “Web server”, “Daemon settings”, “Notifications”", "Server setup, step 6", "server; as row G12.6", "setup apply; the page collects the authentication mode, OAuth and notification fields and does not send them today"),
    ("Layout", "Shell select", "Layout", "Shell chooser (see the shell table)", "this browser, per account and site; as row S06", "browser storage only"),
]
ACTX = [
    ("Trigger Sync", "Request sync", "The call records a request; nothing in the tree was found to act on it. Corrected in every layout once the sync owner confirms that (decision D11)."),
    ("Turn sync off / Pause / disable sync (explicit API)", "Pause sync…", "One control, using the explicit call and its dialog."),
    ("Turn sync on", "Turn sync on", "Stays one click; a failure is shown."),
    ("Retry (paused after error)", "Resume sync…", "It clears the pause; it does not retry a commit. Corrected in every layout (decision D11)."),
    ("Skip Commit", "Skip commits…", "Opens the same review."),
    ("Cancel Import; Cancel operation", "Stop import…; Stop operation…", "“Cancel operation” is corrected in every layout (decision D11). The wizard's “Cancel Import” changes with #64. “Cancel” is kept for closing dialogs."),
    ("Delete Branch Pair (confirm button)", "Remove pair; “Remove pair and delete Git branch” when a remote deletion is ticked", "The label states what will happen."),
    ("Accept SVN / Accept Git / Apply Manual Edit", "Same choices, then “Record decision…”", "The server records the decision and writes no content (issue #73)."),
    ("Acknowledge", "Acknowledge…", "Same prompt text, in the shared dialog."),
    ("Git Hook", "Download Git pre-commit hook", "A button that fetches with the session."),
    ("Logout", "Sign out", "Plain vocabulary only."),
    ("SVN Configuration", "SVN source", "Plain vocabulary only."),
    ("Git Configuration", "Git target", "Plain vocabulary only."),
    ("Sync Settings", "Sync policy", "Plain vocabulary only."),
    ("Quick Stats", "About", "Plain vocabulary only."),
    ("Danger Zone", "Stop or remove", "Plain vocabulary only."),
    ("You need admin privileges to view this page.", "This needs an administrator", "Plain vocabulary only. Users and Directory (LDAP) show it; Server setup and Identity & notifications show no refusal today (issue #268)."),
    ("Corporate login enabled", "Corporate login enabled", "Unchanged: sign-in keeps the existing wording."),
    ("Sync configured enabled / Sync configured disabled", "the same", "Truthful-status wording; identical in both vocabularies."),
    ("Save Configuration to Server", "Save configuration to server", "Case only."),
    ("Start Full Import", "Start full import", "Case only."),
]
URLX = [
    ("/", "`?repo_id=`", "Kept. An unknown repository shows the notice that exists today."),
    ("/conflicts", "`?status=`, `?repo_id=`, `?page=`", "Kept."),
    ("/audit", "`?repo_id=`, `?action=`, `?success=`, `?page=`", "Kept. Stream or Table is not in the address today and stays that way."),
    ("/repos/:id", "none today", "`?section=overview`, `activity`, `branches`, `operations` or `settings` (decision D7). An unknown value shows Overview. In Classic, Individual and Team it scrolls to that part of the page."),
    ("/conflicts/:id, /repos/:id", "path parameter", "Kept."),
    ("/login", "none", "Kept. It stays outside the shell."),
]


def crosswalk():
    md = ["# Legacy navigation and settings crosswalk", "",
          "Where every navigation entry, page section, setting and action label that exists today sits in the three new options. "
          "Classic, Individual and Team keep their labels, order and grouping exactly as #262 left them: they use the existing vocabulary of one text catalogue (decision D3), apart from three corrected labels (decision D11). "
          "The labels in the right-hand columns are the plain vocabulary, used when a person chooses Ledger, Console or Guide.", "",
          "All three new options use one label set and one grouping, so a person who switches between them finds the same words. Routes do not change in any option, "
          "so every saved address and every link in an email or a ticket keeps working.", "",
          "## Navigation", "",
          "Legacy group is the Team layout's grouping; Classic and Individual show one flat list.", "",
          "| Legacy label | Route | Legacy group | Who sees it | Label in the new options | Group | Ledger, wide | Ledger, narrow | Console, wide | Console, narrow | Guide, wide | Guide, narrow |",
          "|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for old, route, og, who, new, ng in NAVX:
        md.append("| " + " | ".join([old, f"`{route}`", og, who, new, ng] + list(PLACE[ng])) + " |")
    md += ["", "Wide and narrow change at 860 px for Ledger, 900 px for Console and 760 px for Guide. Detail routes (`/repos/:id`, `/conflicts/:id`) and `/login` have no navigation entry today and none is added.", "",
           "Why the four renamed entries: “Settings” holds only the signed-in person's profile, password and credentials, so it reads “My account”. "
           "“LDAP” reads “Directory (LDAP)” so the acronym is kept for people who search for it. “Config” holds author identity mappings and the Teams webhook, so it says so. "
           "“Setup” is the server's first-run wizard, not a place for settings, so it reads “Server setup”.", "",
           "## Shell items", "",
           "| Today | Where today | Ledger | Console | Guide | Note |", "|---|---|---|---|---|---|"]
    for row in SHELLX:
        md.append("| " + " | ".join(row) + " |")
    md += ["", "## Page sections", "",
           "Page content is the same in all three options, so one column covers them.", "",
           "| Route | Today | In the new options | Where on the page | Note |", "|---|---|---|---|---|"]
    for route, old, new, where, note in SECTX:
        md.append(f"| `{route}` | {old} | {new} | {where} | {note} |")
    md += ["", "## Settings", "",
           "Every value a person can set, with its scope. Field names sent to the server are unchanged.", "",
           "| Legacy label | Where today | Label in the new options | Where in the new options | Scope, and the matrix rows that say who sees it | Field or call |", "|---|---|---|---|---|---|"]
    for row in SETX:
        md.append("| " + " | ".join(row) + " |")
    md += ["", "## Action labels", "", "| Today | In the new options | Why |", "|---|---|---|"]
    for row in ACTX:
        md.append("| " + " | ".join(row) + " |")
    md += ["", "Safety and truthful-status sentences are the same in both vocabularies and are taken from the source: “Stopping does not undo commits already published…”, “Restore registration (stays disabled)”, "
           "“Removal completed; verified cleanup recorded”, “This ID is retained; no new registration is silently created.”, “Cancellation after intent retains the hold; it does not undo remote work.”, "
           "“Sync configured enabled”, and the sentences of the save notices and the status evidence. Where the design wants different words for one of these, the row says so and it is a proposal. "
           "v1 said “word for word” while changing four of them; that is corrected.", "",
           "## Addresses and parameters", "", "| Route | Parameters today | In the new options |", "|---|---|---|"]
    for row in URLX:
        md.append("| " + " | ".join(row) + " |")
    md += ["", "## Familiarity", "",
           "- Order is kept: Dashboard, Repositories, Conflicts, Audit log, then account, then administration, in every option and at every width.",
           "- The conflict count stays on the Conflicts entry.",
           "- Classic stays the default and the fallback, and dark stays the default theme. Nobody is moved to a new option or to light without choosing it.",
           "- Regions keep today's order on every page, in every layout (decision D12). The new options add a read-only “Needs attention” at the top of the dashboard and a read-only status line at the top of the repository page."]
    write_md("crosswalk/legacy-navigation-settings.md", "\n".join(md) + "\n", "Legacy navigation and settings crosswalk")


if __name__ == "__main__":
    R = rows()
    counts, kinds, spare = matrix(R)
    parity(R)
    crosswalk()
    write_md("DECISIONS.md", v11.decisions_md(), "Decisions")
    write_md("CHANGES.md", v11.changes_md(), "What changed")
    write_md("CHANGES-FROM-v1.md", v11.changes_from_v1_md(), "Changes from v1, by finding")
    write_md("CHANGES-FROM-v1.3.md", v11.changes_from_v13_md(), "Changes from v1.3, by item")
    write_md("CHANGES-FROM-v1.4.md", v11.changes_from_v14_md(), "Changes from v1.4, by item")
    write_md("CHANGES-FROM-v1.5.md", v11.changes_from_v15_md(), "Changes from v1.5, by item")
    v11.regions_docs(write, write_md)
    v11.bridge_docs(write, write_md)
    docs_text.write_all(OUT, R, counts, kinds, write, write_md)
    print("rows", len(R), counts, "unreferenced pages:", spare)
