#!/usr/bin/env python3
"""Build the #263 design package prototypes (static HTML, CSS and one small script).

Standard library only. Reads this directory; writes only under the output directory given as
argv[1]. No network, no installs, no application code. Run: python3 build.py <out-dir>
"""
import pathlib, re, sys
from html.parser import HTMLParser

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import kit, tokens, screens_a as A, screens_b as B
from kit import OPTIONS, NAV, GROUP_LABELS, ic, e

GEN = pathlib.Path(__file__).resolve().parent
OUT = pathlib.Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else GEN.parent / "v1"

# slug, matrix id, index title, function, active route, flags
S = [
    ("g01-login", "G01", "Sign in", A.g01_login, None, "bare legacy"),
    ("g01-login-states", "G01", "Sign in: states", A.g01_states, None, "bare legacy"),
    ("g02-dashboard", "G02", "Dashboard (team server)", A.g02_dashboard, "/", ""),
    ("g02-dashboard-single", "G02", "Dashboard (one repository)", A.g02_single, "/", ""),
    ("g02-dashboard-states", "G02", "Dashboard: states", A.g02_states, "/", ""),
    ("g03-repositories", "G03", "Repositories", A.g03_list, "/repos", ""),
    ("g03-add-repository", "G03", "Add repository", A.g03_add, "/repos", ""),
    ("g03-save-recovery", "G03", "Add repository: save and recovery", A.g03_save, "/repos", ""),
    ("g03-states", "G03", "Repositories: states and receipts", A.g03_states, "/repos", ""),
    ("g04-overview", "G04", "Repository: overview", A.g04_overview, "/repos", ""),
    ("g04-activity", "G04", "Repository: activity (both epics)", A.g04_activity, "/repos", ""),
    ("g04-branches", "G04", "Repository: branch pairs", A.g04_branches, "/repos", ""),
    ("g04-pairing", "G04", "Repository: add branch pair", A.g04_pairing, "/repos", ""),
    ("g04-refresh-preview", "G04", "Repository: update preview, re-anchor boundary", A.g04_refresh, "/repos", ""),
    ("g04-operations", "G04", "Repository: operations and import states", A.g04_operations, "/repos", ""),
    ("g04-settings", "G04", "Repository: settings", A.g04_settings, "/repos", ""),
    ("g04-settings-edit", "G04", "Repository: edit and save states", A.g04_settings_edit, "/repos", ""),
    ("g04-policy", "G04", "Repository: path policy (both epics)", A.g04_policy, "/repos", ""),
    ("g04-skip", "G04", "Repository: paused, skip and resume", A.g04_skip, "/repos", ""),
    ("g04-pause", "G04", "Repository: pause sync", A.g04_pause, "/repos", ""),
    ("g04-remove-root", "G04", "Repository: remove from RepoSync", A.g04_remove_root, "/repos", ""),
    ("g04-remove-pair", "G04", "Repository: remove branch pair", A.g04_remove_pair, "/repos", ""),
    ("g04-removal-recovery", "G04", "Repository: removal receipts and recovery", A.g04_recovery, "/repos", ""),
    ("g04-states", "G04", "Repository: page states", A.g04_states, "/repos", ""),
    ("g05-conflicts", "G05", "Conflicts", B.g05_list, "/conflicts", ""),
    ("g05-conflicts-states", "G05", "Conflicts: states", B.g05_states, "/conflicts", ""),
    ("g06-conflict-diff", "G06", "Conflict: diff", B.g06_diff, "/conflicts", ""),
    ("g06-conflict-edit", "G06", "Conflict: manual edit", B.g06_edit, "/conflicts", ""),
    ("g06-conflict-states", "G06", "Conflict: states", B.g06_states, "/conflicts", ""),
    ("g07-audit", "G07", "Audit log: stream", B.g07_stream, "/audit", ""),
    ("g07-audit-table", "G07", "Audit log: table", B.g07_table, "/audit", ""),
    ("g07-audit-states", "G07", "Audit log: states", B.g07_states, "/audit", ""),
    ("g08-account", "G08", "My account", B.g08_account, "/settings", ""),
    ("g08-account-states", "G08", "My account: states", B.g08_states, "/settings", ""),
    ("g09-users", "G09", "Users", B.g09_users, "/users", ""),
    ("g09-user-dialogs", "G09", "Users: dialogs", B.g09_dialogs, "/users", ""),
    ("g09-users-states", "G09", "Users: states", B.g09_states, "/users", ""),
    ("g10-directory", "G10", "Directory (LDAP)", B.g10_directory, "/ldap", ""),
    ("g10-directory-states", "G10", "Directory (LDAP): states", B.g10_states, "/ldap", ""),
    ("g11-identity-notifications", "G11", "Identity and notifications", B.g11_main, "/config", ""),
    ("g11-states", "G11", "Identity and notifications: states", B.g11_states, "/config", ""),
] + [(f"g12-setup-{i + 1}", "G12", f"Server setup {i + 1}: {B.WIZ[i]}", B.g12_step(i), "/setup", "") for i in range(8)] + [
    ("g12-setup-states", "G12", "Server setup: states", B.g12_states, "/setup", ""),
] + [(f"g13-docs-{d[0]}", "G13", f"Help: {d[1]}", B.g13_doc(i), "/docs", "") for i, d in enumerate(B.DOCS)] + [
    ("s01-shell-roles", "S", "Shell: navigation and roles (administrator)", B.s01_shell, None, ""),
    ("s01-shell-member", "S", "Shell: navigation and roles (non-administrator)", B.s01_shell, None, "member"),
    ("s02-chooser", "S", "Shell: layout and theme chooser", B.s02_chooser, None, ""),
    ("s03-dialogs-transient", "S", "Dialogs and transient interface", B.s03_dialogs, None, ""),
    ("s04-recoverable-saves", "S", "Recoverable saves", B.s04_saves, "/repos", ""),
    ("s05-operations", "S", "Operations: one vocabulary", B.s05_ops, "/repos", ""),
    ("s06-credentials-identity", "S", "Credentials and identity", B.s06_creds, "/settings", ""),
    ("s07-exceptional", "S", "Exceptional paths", B.s07_exceptional, None, ""),
    ("s08-existing-layouts-light", "S", "Existing layouts in light", B.s08_legacy, None, "bare legacy"),
]
SLUGS = [s[0] for s in S]

FLOWS = [
    ("first-repository", "Add a repository and run its first import", [("g03-repositories", "Start from the list"), ("g03-add-repository", "Enter SVN, Git, import mode and sync settings"),
        ("g03-save-recovery", "If a stage is not confirmed, resume on the same repository"), ("g04-operations", "Start the import; follow it; stop only deliberately"), ("g04-overview", "Land on the repository once the import completes")]),
    ("stop-import-recover", "Stop an import and deal with the hold", [("g04-operations", "Stop, stop requested, stopped and held, verify remote, resume"), ("g04-states", "What a held repository refuses"), ("s05-operations", "The same words for every durable operation")]),
    ("paused-skip-resume", "Sync paused after an error", [("g02-dashboard", "The dashboard names the pair and the reason"), ("g04-skip", "Skip an observed prefix, or resume"), ("g04-states", "Paused and unknown states")]),
    ("add-branch-pair", "Add a branch pair", [("g04-branches", "Open Add branch pair"), ("g04-pairing", "Review the plan, publish, follow or recover the operation"), ("g04-refresh-preview", "Later: read-only update preview; re-anchor stays held")]),
    ("remove-pair", "Remove a branch pair, with or without deleting a remote branch", [("g04-branches", "Row menu: Remove pair"), ("g04-remove-pair", "Exact targets, remote deletion off by default, typed confirmation"), ("g04-removal-recovery", "Stages, failure, lost reply, restore"), ("g03-states", "Receipts on the list")]),
    ("remove-root-restore", "Pause, remove and restore a repository", [("g04-settings", "Stop or remove"), ("g04-pause", "Pause keeps everything"), ("g04-remove-root", "Remove: review, dependency refusal"), ("g04-removal-recovery", "Completed receipt and Restore (stays disabled)")]),
    ("conflict-decision", "Review and record a conflict decision", [("g05-conflicts", "Find it"), ("g06-conflict-diff", "Compare SVN, Git and the base"), ("g06-conflict-edit", "Or edit a result"), ("g06-conflict-states", "Confirm; see what was recorded and what is not available")]),
    ("edit-settings-recover", "Edit repository settings and recover a save", [("g04-settings", "Read"), ("g04-settings-edit", "Edit one section; save states"), ("g04-policy", "Path policy on each epic"), ("s04-recoverable-saves", "Every save state")]),
    ("account-credentials", "Manage my account and credentials", [("g08-account", "Profile, password, credentials"), ("g08-account-states", "Directory-controlled profile, tests, failures"), ("s06-credentials-identity", "How secrets and identity are shown everywhere")]),
    ("admin-users", "Administer users", [("g09-users", "List"), ("g09-user-dialogs", "Create, edit, delete"), ("g09-users-states", "Denied, failed, own account")]),
    ("admin-directory", "Configure directory sign-in", [("g10-directory", "Connection, mapping, service account"), ("g10-directory-states", "Test, save, denied")]),
    ("admin-identity-notifications", "Identity mappings and notifications", [("g11-identity-notifications", "Mappings and Teams"), ("g11-states", "Unreadable, failed, confirmations")]),
    ("server-setup", "Server setup, all eight steps", [(f"g12-setup-{i + 1}", B.WIZ[i]) for i in range(8)] + [("g12-setup-states", "Validation, save and import outcomes")]),
    ("sign-in-session", "Sign in, lose a session, be refused", [("g01-login", "Sign in"), ("g01-login-states", "Pending, invalid, session ended, setup required"), ("s07-exceptional", "Not allowed, not found, held, malformed")]),
    ("layout-theme", "Switch layout and theme", [("s02-chooser", "Chooser and its notices"), ("s01-shell-roles", "Navigation for an administrator"), ("s01-shell-member", "Navigation for everyone else")]),
    ("help", "Read the documentation", [(f"g13-docs-{d[0]}", d[1]) for d in B.DOCS]),
]


def sprite():
    raw = (GEN / "icons.html").read_text()
    raw = raw.replace('stroke="#0b1220"', 'style="stroke:var(--rs-ic-cut,#fff)"').replace('fill="#0b1220"', 'style="fill:var(--rs-ic-cut,#fff)"')
    grid = ('<symbol id="i-grid" viewBox="0 0 16 16"><rect x="2" y="2" width="5" height="5" rx="1" fill="none" stroke="currentColor" stroke-width="1.5"/>'
            '<rect x="9" y="2" width="5" height="5" rx="1" fill="none" stroke="currentColor" stroke-width="1.5"/><rect x="2" y="9" width="5" height="5" rx="1" fill="none" stroke="currentColor" stroke-width="1.5"/>'
            '<rect x="9" y="9" width="5" height="5" rx="1" fill="none" stroke="currentColor" stroke-width="1.5"/></symbol>')
    return raw.replace("</defs>", grid + "</defs>")


LOGO = ('<svg class="shell-logo" viewBox="0 0 32 32" aria-hidden="true"><path d="M8 14A8 8 0 0 1 24 14" fill="none" stroke="var(--rs-svn)" stroke-width="2.6" stroke-linecap="round"/>'
        '<path d="M22 10l2 4h-4z" fill="var(--rs-svn)"/><path d="M24 18A8 8 0 0 1 8 18" fill="none" stroke="var(--rs-git)" stroke-width="2.6" stroke-linecap="round"/><path d="M10 22l-2-4h4z" fill="var(--rs-git)"/></svg>')


def shell(opt, slug, active, member, main):
    groups = []
    for gid in ("workspace", "account", "administration"):
        items = [n for n in NAV if n[2] == gid and not (member and n[3])]
        if not items:
            continue
        links = []
        for to, label, _g, _adm, target, icon in items:
            act = " rs-shell-link--active" if to == active else ""
            cur = ' aria-current="page"' if to == active else ""
            count = '<span class="shell-count" aria-label="1 active conflict">1</span>' if to == "/conflicts" else ""
            links.append(f'<a class="rs-shell-link{act}" href="{target}.html"{cur}>{ic(icon, "nav-ic")}{label}{count}</a>')
        if gid == "workspace":
            # The one added shell control. It follows the last workspace link, so tab order matches reading order at every width.
            what = "account" if member else "account and administration"
            long = ("Account" if member else "Account and admin") if opt == "guide" else "More"
            here = next((n[1] for n in NAV if n[0] == active and n[2] != "workspace"), None)
            cur = " is-current" if here else ""
            if here and opt == "guide":
                long = f"{long}: {here}"
            links.append(f'<button type="button" class="shell-menu{cur}" aria-expanded="false" aria-controls="{"nav-account" if member else "nav-account nav-administration"}" '
                         f'aria-label="More: {what}">{ic("menu")}<span class="lbl-long">{long}</span><span class="lbl-short">More</span></button>')
        groups.append(f'<div class="shell-group" role="group" aria-labelledby="g-{gid}" data-group="{gid}" id="nav-{gid}"><p id="g-{gid}" class="shell-group-label">'
                      f'{GROUP_LABELS[gid]}</p><div class="shell-group-links">{"".join(links)}</div></div>')
    layouts = "".join(f"<option disabled>{n}</option>" for n in ("Classic", "Individual", "Team")) + "".join(
        f'<option value="{k}" data-href="../../{k}/screens/{slug}.html"{" selected" if k == opt else ""}>{v["name"]}</option>' for k, v in OPTIONS.items())
    who = "Tomás Rivera" if member else "Ines Okafor"
    status = f'<div class="shell-status" role="status">{ic("held")}<span>Held</span></div>'
    return (f'<div class="rs-layout-shell" data-rs-layout="{opt}" data-rs-theme="dark"><nav aria-label="Primary" class="shell-nav"><div class="shell-nav-inner"><div class="shell-nav-row">'
            f'<div class="shell-brand">{LOGO}<span class="shell-brand-name">RepoSync</span><div class="shell-links" id="shell-links">{"".join(groups)}</div></div>'
            f'<div class="shell-account">{status}<span class="shell-user">{who}</span>'
            f'<div class="shell-chooser"><label class="sr" for="lay">Layout</label><select id="lay" data-layout-select data-current="{opt}">{layouts}</select>'
            f'<label class="sr" for="thm">Theme</label><select id="thm" data-theme-select><option value="dark">Dark</option><option value="light">Light</option></select>'
            f'<p class="notice" role="status"></p></div><button type="button" class="shell-logout">Sign out</button></div>'
            f'</div></div></nav><main tabindex="-1" class="shell-main">{main}</main></div>')


def document(opt, title, body, depth, bar, legacy=False):
    up = "../" * depth
    if legacy:
        return (f'<!doctype html>\n<html lang="en" data-rs-layout="legacy" data-rs-theme="dark">\n<head>\n<meta charset="utf-8">\n'
                f'<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">\n<title>{e(title)} · RepoSync #263 prototype</title>\n'
                f'<link rel="stylesheet" href="{up}prototypes/assets/base.css">\n<link rel="stylesheet" href="{up}prototypes/assets/legacy.css">\n</head>\n<body>\n{sprite()}\n{bar}\n{body}\n'
                f'<script src="{up}prototypes/assets/proto.js"></script>\n</body>\n</html>\n')
    return (f'<!doctype html>\n<html lang="en" data-rs-layout="{opt}" data-rs-theme="dark">\n<head>\n<meta charset="utf-8">\n'
            f'<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">\n<title>{e(title)} · {OPTIONS[opt]["name"]} · RepoSync #263 prototype</title>\n'
            f'<link rel="stylesheet" href="{up}prototypes/assets/base.css">\n<link rel="stylesheet" href="{up}options/{opt}/tokens.css">\n'
            f'<link rel="stylesheet" href="{up}options/{opt}/option.css">\n</head>\n<body>\n{sprite()}\n{bar}\n{body}\n'
            f'<script src="{up}prototypes/assets/proto.js"></script>\n</body>\n</html>\n')


def proto_bar(opt, slug, gid, title, i):
    prev_s, next_s = SLUGS[i - 1], SLUGS[(i + 1) % len(SLUGS)]
    return (f'<div class="proto-bar" data-proto-bar><b>Prototype</b><span>{OPTIONS[opt]["name"]} · {gid} · {e(title)}</span>'
            f'<a href="../../../prototypes/index.html">All screens</a><a href="{prev_s}.html">Previous</a><a href="{next_s}.html">Next</a>'
            f'<span class="seg" role="group" aria-label="Theme"><button type="button" data-theme-set="dark">Dark</button>'
            f'<button type="button" data-theme-set="light">Light</button></span>'
            f'<span>Synthetic data. Controls do not act. This bar is not part of the design.</span></div>')


def build():
    (OUT / "prototypes/assets").mkdir(parents=True, exist_ok=True)
    (OUT / "prototypes/assets/base.css").write_text((GEN / "base.css").read_text())
    (OUT / "prototypes/assets/proto.js").write_text((GEN / "proto.js").read_text())
    (OUT / "prototypes/assets/legacy.css").write_text(tokens.legacy_css())
    for opt in OPTIONS:
        d = OUT / "options" / opt
        (d / "screens").mkdir(parents=True, exist_ok=True)
        (d / "flows").mkdir(parents=True, exist_ok=True)
        (d / "tokens.css").write_text(tokens.css(opt))
        (d / "option.css").write_text((GEN / f"opt_{opt}.css").read_text())
        for i, (slug, gid, title, fn, active, flags) in enumerate(S):
            kit._fid[0] = 0
            _t, body = fn(opt)
            legacy = "legacy" in flags
            if slug.startswith("g01"):
                body += kit.note("Sign-in has one appearance for everyone: dark, as today. The saved layout and theme belong to an account, and nobody is signed in here. "
                                 "This page is the same file under each option, and the theme buttons do nothing on it.")
            if legacy:
                page = f'<div class="rs-layout-shell bare" data-rs-layout="legacy" data-rs-theme="dark"><main tabindex="-1" class="shell-main">{body}</main></div>'
            elif "bare" in flags:
                page = f'<div class="rs-layout-shell bare" data-rs-layout="{opt}" data-rs-theme="dark"><main tabindex="-1" class="shell-main">{body}</main></div>'
            else:
                page = shell(opt, slug, active, "member" in flags, body)
            (d / "screens" / f"{slug}.html").write_text(document(opt, title, page, 3, proto_bar(opt, slug, gid, title, i), legacy))
        # flows
        sections = []
        for fid, ftitle, fsteps in FLOWS:
            items = "".join(f'<li><a href="../screens/{s}.html">{e(dict((x[0], x[2]) for x in S)[s])}</a><span class="t3"> · {e(c)}</span></li>' for s, c in fsteps)
            sections.append(f'<section class="sec" id="{fid}"><div class="sec-h"><h2>{e(ftitle)}</h2></div><ol>{items}</ol></section>')
        body = (f'<div class="rs-layout-shell bare" data-rs-layout="{opt}" data-rs-theme="dark"><main class="shell-main"><div class="pg"><header class="pg-head">'
                f'<h1>{OPTIONS[opt]["name"]}: state flows</h1><span class="t3 sm">{len(FLOWS)} flows through the prototype screens, in order</span></header>'
                f'<div class="pg-body">{"".join(sections)}</div></div></main></div>')
        bar = (f'<div class="proto-bar" data-proto-bar><b>Prototype</b><span>{OPTIONS[opt]["name"]} · state flows</span><a href="../../../prototypes/index.html">All screens</a>'
               '<span class="seg" role="group" aria-label="Theme"><button type="button" data-theme-set="dark">Dark</button><button type="button" data-theme-set="light">Light</button></span></div>')
        (d / "flows" / "index.html").write_text(document(opt, "State flows", body, 3, bar))
    # index
    rows, last = [], None
    names = {"G01": "G01 /login", "G02": "G02 /", "G03": "G03 /repos", "G04": "G04 /repos/:id", "G05": "G05 /conflicts", "G06": "G06 /conflicts/:id", "G07": "G07 /audit",
             "G08": "G08 /settings", "G09": "G09 /users", "G10": "G10 /ldap", "G11": "G11 /config", "G12": "G12 /setup", "G13": "G13 /docs", "S": "Non-route surfaces"}
    for slug, gid, title, *_ in S:
        if gid != last:
            rows.append(f'<tr class="sub"><td colspan="4"><b>{names[gid]}</b></td></tr>')
            last = gid
        rows.append(f"<tr><td>{e(title)}<br><span class='mono t3'>{slug}</span></td>" + "".join(f'<td><a href="../options/{o}/screens/{slug}.html">{OPTIONS[o]["name"]}</a></td>' for o in OPTIONS) + "</tr>")
    cards = "".join(f'<div class="card"><h3>{v["name"]}</h3><span class="t2 sm">{v["tag"]}</span><span class="t3 sm">Primary theme: {v["primary"]}. Both themes are complete.</span>'
                    f'<span><a href="../options/{k}/screens/g02-dashboard.html">Open the dashboard</a> · <a href="../options/{k}/flows/index.html">State flows</a></span></div>' for k, v in OPTIONS.items())
    body = (f'<div class="rs-layout-shell bare" data-rs-layout="ledger" data-rs-theme="dark"><main class="shell-main"><div class="pg"><header class="pg-head"><h1>RepoSync #263 prototypes</h1>'
            f'<span class="t3 sm">{len(S)} screens in each of three options · light and dark · wide and narrow · synthetic data</span></header><div class="pg-body">'
            f'<section class="sec"><div class="sec-h"><h2>Options</h2></div><div class="cards">{cards}</div>'
            '<p class="t2 sm">Dark is the default. Use the Dark and Light buttons at the top of any page. Add <span class="mono">#dark</span> or <span class="mono">#light</span> to an address to force a theme, and '
            '<span class="mono">#dark-nobar</span> to hide the prototype bar for a capture. Resize the window, or use 1440 by 900 and 390 by 844, for the wide and narrow layouts.</p></section>'
            f'<section class="sec"><div class="sec-h"><h2>Every screen, every option</h2></div><div class="tw"><table class="tbl"><thead><tr><th scope="col">Screen</th>'
            + "".join(f'<th scope="col">{v["name"]}</th>' for v in OPTIONS.values()) + f'</tr></thead><tbody>{"".join(rows)}</tbody></table></div></section></div></div></main></div>')
    bar = ('<div class="proto-bar" data-proto-bar><b>Prototype index</b><span class="seg" role="group" aria-label="Theme"><button type="button" data-theme-set="dark">Dark</button>'
           '<button type="button" data-theme-set="light">Light</button></span><span>Synthetic data. Not the product.</span></div>')
    (OUT / "prototypes" / "index.html").write_text(document("ledger", "Index", body, 1, bar).replace("../prototypes/assets/", "assets/"))


VOID = {"br", "hr", "img", "input", "meta", "link", "use", "path", "circle", "rect", "ellipse", "col", "wbr"}
IMPL = {"li", "p", "dt", "dd", "tr", "td", "th", "tbody", "thead", "option"}


class Check(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.stack, self.errs, self.refs, self.ids = [], [], [], set()

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        for k in ("href", "src"):
            if k in a and a[k]:
                self.refs.append(a[k])
        if "id" in a:
            if a["id"] in self.ids and not a["id"].startswith("i-"):
                self.errs.append(f"duplicate id {a['id']}")
            self.ids.add(a["id"])
        if tag not in VOID:
            self.stack.append(tag)

    def handle_endtag(self, tag):
        if tag in VOID:
            return
        tmp = list(self.stack)
        while tmp and tmp[-1] in IMPL and tmp[-1] != tag:
            tmp.pop()
        if tmp and tmp[-1] == tag:
            tmp.pop()
            self.stack = tmp
        else:
            self.errs.append(f"line {self.getpos()[0]}: </{tag}> with open <{self.stack[-1] if self.stack else None}>")


def validate():
    problems, pages = [], sorted(OUT.rglob("*.html"))
    for p in pages:
        rel = p.relative_to(OUT)
        if not (rel.parts[0] == "prototypes" or (rel.parts[0] == "options" and len(rel.parts) > 3)):
            continue   # documents, history and tools are checked by finish.py
        c = Check()
        text = p.read_text()
        c.feed(text)
        left = [t for t in c.stack if t not in IMPL]
        for x in c.errs[:4] + [f"unclosed <{t}>" for t in left[:3]]:
            problems.append(f"{p.relative_to(OUT)}: {x}")
        for r in c.refs:
            if r.startswith("#"):
                continue
            if re.match(r"^[a-z][a-z0-9+.-]*:", r):
                problems.append(f"{p.relative_to(OUT)}: external reference {r}")
                continue
            target = (p.parent / r.split("#")[0]).resolve()
            if not target.exists():
                problems.append(f"{p.relative_to(OUT)}: missing {r}")
        for use in set(re.findall(r'href="#(i-[a-z]+)"', text)):
            if f'id="{use}"' not in text:
                problems.append(f"{p.relative_to(OUT)}: missing icon {use}")
    css = (GEN / "base.css").read_text() + "".join((GEN / f"opt_{o}.css").read_text() for o in OPTIONS)
    used = set(re.findall(r"var\(--(rs-[a-z0-9-]+)", css))
    for opt in OPTIONS:
        defined = set(re.findall(r"--(rs-[a-z0-9-]+)\s*:", tokens.css(opt))) | {"rs-ic-cut"}
        for theme in ("light", "dark"):
            block = set("rs-" + k for k in tokens.T[opt][theme] if k != "shell") | set("rs-shell-" + n for n in tokens.SHELL) | set("rs-" + k for k in tokens.T[opt]["type"]) | {"rs-ic-cut"}
            miss = sorted(used - block)
            if miss:
                problems.append(f"tokens {opt}/{theme}: undefined {miss}")
    # F2: evidence must describe the same health as the status line of its page; a specimen must open with its own label
    labels = {k: v[2] for k, v in kit.STATES.items()}
    for p in pages:
        rel = p.relative_to(OUT)
        if not (rel.parts[0] == "options" and len(rel.parts) > 3):
            continue
        text = p.read_text()
        lines = re.findall(r'data-statline="([a-z]+)"', text)
        for ev in re.findall(r'data-evidence="([a-z]+)"', text):
            if not lines or ev != lines[0]:
                problems.append(f"{rel}: evidence for {ev} under a status line for {lines[:1]}")
        for ev, first in re.findall(r'data-evidence-specimen="([a-z]+)">(?:<b>[^<]*</b>)?<span>([^<]*)', text):
            if not first.startswith(labels[ev]):
                problems.append(f"{rel}: evidence specimen for {ev} opens with {first[:30]!r}")
    # F11: the markup inside main is the same in every option, apart from the option's own name
    def inner(o, slug):
        t = (OUT / "options" / o / "screens" / f"{slug}.html").read_text()
        m = re.search(r"<main[^>]*>(.*)</main>", t, re.S).group(1)
        for k, v in OPTIONS.items():
            m = m.replace(v["name"], "OPTION")
        return m
    first = next(iter(OPTIONS))
    for slug, *_ in S:
        ref = inner(first, slug)
        for o in OPTIONS:
            if inner(o, slug) != ref:
                problems.append(f"{slug}: markup inside main differs between {first} and {o}")
    base_used = set(re.findall(r"var\(--(rs-[a-z0-9-]+)", (GEN / "base.css").read_text() + tokens.legacy_css()))
    for theme in ("light", "dark"):
        have = set("rs-" + k for k in tokens.LEGACY[theme] if k != "shell") | set("rs-shell-" + n for n in tokens.SHELL) | set("rs-" + k for k in tokens.LEGACY["type"]) | {"rs-ic-cut"}
        if base_used - have:
            problems.append(f"tokens existing layouts/{theme}: undefined {sorted(base_used - have)}")
    return pages, problems


if __name__ == "__main__":
    build()
    pages, problems = validate()
    print(f"built {len(pages)} html files for {len(S)} screens x {len(OPTIONS)} options; {len(problems)} structural problems")
    for x in problems[:40]:
        print("  ", x)
