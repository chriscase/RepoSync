"""Shared helpers for the #263 prototype generator.

Every screen is written once, as semantic HTML built from these helpers, and rendered inside
each option's shell. Options differ in tokens, shell arrangement and component styling (CSS),
plus a few explicit composition hooks. That keeps capability parity structural: an option
cannot lose a control that the shared screen contains.
"""
import html as _h
import re

VERSION = "v1.6"
# The earlier versions are private Artifacts: the links open only for their owner. The SHA-256 values identify them.
V15_ARTIFACT = "https://claude.ai/artifact/4aJnJVnfadwoLUhL5Vov9p"
V15_SUMS = "3d4562774d3a19365982def3042dc3b336273ffee73d22f8d39b663e103cf37f"
V14_ARTIFACT = "https://claude.ai/artifact/GVVT4zmw43gCmDiiwLEqiB"
V14_SUMS = "671a9cbf59a8e90e800339607524d07831004d85864794c2d818df8b38dbd769"
V1_ARCHIVE_SHA = "6b63b9dc4b0667a86bf1accd4b2f62034655952eca6b04ee8858843dee5df6de"
V13_ARTIFACT = "https://claude.ai/artifact/522LiBWfRtC2X8b16azYBd"
V13_SUMS = "3c0ea6696929d637cd53af41466833ad3645cfb17debc1f11533f8d103625127"
V13_WITHDRAWN_SUMS = "e5432433b808dbeda75319eccf443558cca97a116efd4b13c081887c2810c921"
V12_ARTIFACT = "https://claude.ai/artifact/H4hqT7suswfojB3jk3ke5A"
V12_SUMS = "e1085be16de128e4ce376edac0da62996a4505077d8cb0103eabc5580fff2c45"
ISSUE_268 = "https://github.com/chriscase/RepoSync/issues/268"
V11_ARTIFACT = "https://claude.ai/artifact/5aKdwSu3jSJgBQ1LYZ59RA"
V11_SUMS = "46d58debfd6b453bf184233ceea3daeb4679d6d20370cae718119ef17742ff65"
V1_ARTIFACT = "https://claude.ai/artifact/U4qoeNzph9x32eey55Hjg1"
V1_SUMS = "5eb5812a4548774f9c33103c8a3b3521d0df434c397537cfad538649f8f8d814"
PIN_P = "b2b055a72657824fd6662b451f69718d73405aa5"   # epic/reposync-partial-52
PIN_R = "5853a8884496d294ce21898415d7d2718b73bf7c"   # epic/reposync-reliability-61

OPTIONS = {
    "ledger": {"name": "Ledger", "primary": "light",
               "tag": "A reading layout. Two-tier masthead, ruled sections, ink-only accent."},
    "console": {"name": "Console", "primary": "dark",
                "tag": "An operations layout. Grouped sidebar, dense tables, status inspector."},
    "guide": {"name": "Guide", "primary": "light",
              "tag": "A task-led layout. Few large destinations, plain-language cards, big targets."},
}

# One proposed label set for all three new options (switching between them never renames a destination).
# Legacy labels stay on Classic, Individual and Team. Order, targets, groups and admin-only flags are
# exactly PRIMARY_NAVIGATION in web-ui/src/layouts/layoutCatalog.ts; "/docs" is the one proposed addition.
NAV = [
    ("/", "Dashboard", "workspace", False, "g02-dashboard", "grid"),
    ("/repos", "Repositories", "workspace", False, "g03-repositories", "svn"),
    ("/conflicts", "Conflicts", "workspace", False, "g05-conflicts", "conflict"),
    ("/audit", "Audit log", "workspace", False, "g07-audit", "file"),
    ("/settings", "My account", "account", False, "g08-account", "user"),
    ("/docs", "Help", "account", False, "g13-docs-overview", "info"),
    ("/users", "Users", "administration", True, "g09-users", "user"),
    ("/ldap", "Directory (LDAP)", "administration", True, "g10-directory", "key"),
    ("/config", "Identity &amp; notifications", "administration", True, "g11-identity-notifications", "swap"),
    ("/setup", "Server setup", "administration", True, "g12-setup-1", "run"),
]
GROUP_LABELS = {"workspace": "Workspace", "account": "Account", "administration": "Administration"}

STATES = {  # health value -> (class, icon, label). Words are the server's ten health values.
    "idle": ("st-ok", "idle", "Idle"), "running": ("st-run", "run", "Running"),
    "initializing": ("st-run", "init", "Initializing"), "busy": ("st-run", "busy", "Busy"),
    "disabled": ("st-off", "off", "Disabled"), "paused": ("st-warn", "paused", "Paused after error"),
    "conflict": ("st-warn", "conflict", "Conflict"), "held": ("st-held", "held", "Held"),
    "error": ("st-err", "error", "Error"), "unknown": ("st-unk", "unknown", "Unknown"),
}


def e(text):
    return _h.escape(str(text), quote=True)


def ic(name, cls=""):
    c = ("ic " + cls).strip()
    return f'<svg class="{c}" aria-hidden="true"><use href="#i-{name}"/></svg>'


def pill(state, label=None, big=False):
    cls, icon, default = STATES[state]
    return f'<span class="pill {cls}{" big" if big else ""}">{ic(icon)}{label or default}</span>'


def tone(kind, icon, label):
    """A pill for non-health states (conflict status, operation lifecycle, outcomes)."""
    return f'<span class="pill st-{kind}">{ic(icon)}{label}</span>'


def stamp(text, kind=""):
    icon = {"stale": "stale", "bad": "error", "none": "minus"}.get(kind, "clock")
    return f'<span class="stamp {kind}">{ic(icon)}{text}</span>'


def chip(text, cls=""):
    return f'<span class="chip {cls}">{text}</span>'


def needs(text="Needs feature work"):
    return f'<span class="tag-needs">{text}</span>'


def btn(label, kind="", icon=None, disabled=False, href=None):
    dis = ' aria-disabled="true"' if disabled else ""
    inner = (ic(icon) if icon else "") + label
    if href:
        return f'<a class="btn {kind}" href="{href}"{dis}>{inner}</a>'
    return f'<button type="button" class="btn {kind}"{dis}>{inner}</button>'


def link(label, href="#", cls=""):
    c = f' class="{cls}"' if cls else ""
    return f'<a{c} href="{href}">{label}</a>'


def _ident(text):
    """(extra class, html) for an identifier.

    Up to 24 characters: one piece. It stays whole while its container can hold it, and breaks after an
    underscore or a full stop, then anywhere, only when it cannot. Longer: one piece per path segment, each
    ending at its slash, so a line breaks between segments before it breaks inside one. The stylesheet does the rest.
    """
    t = str(text)
    if "<" in t or " " in t:
        return "", t
    soft = lambda x: x.replace("_", "_<wbr>").replace(".", ".<wbr>")
    if len(t) <= 24:
        return " tok", soft(t)
    parts = [p for p in re.split(r"(?<=/)(?=[^/])", t) if p]
    return "", "<wbr>".join(f'<span class="sg">{soft(p)}</span>' for p in parts)


def mono(text, cls=""):
    tok, inner = _ident(text)
    return f'<span class="mono {cls}{tok}">{inner}</span>'


def svn(text):
    tok, inner = _ident(e(text))
    return f'<span class="mono svn{tok}">{inner}</span>'


def git(text):
    tok, inner = _ident(e(text))
    return f'<span class="mono git{tok}">{inner}</span>'


def pos(rev, sha):
    return f'{svn(rev)} {ic("swap")} {git(sha)}'


def row(*items, cls=""):
    return f'<div class="row {cls}">{"".join(items)}</div>'


def stack(*items, cls=""):
    return f'<div class="stack {cls}">{"".join(items)}</div>'


def cols(*pairs):
    return '<div class="cols">' + "".join(f'<div class="c{n}">{body}</div>' for n, body in pairs) + "</div>"


def sp():
    return '<span class="sp"></span>'


def kv(*pairs, cls=""):
    return f'<dl class="kv {cls}">' + "".join(f"<dt>{k}</dt><dd>{v}</dd>" for k, v in pairs) + "</dl>"


def table(heads, rows, cap=None, cls=""):
    label = re.sub(r"<[^>]+>", "", cap) if cap else "Table"
    out = [f'<div class="tw" role="region" aria-label="{label}" tabindex="0"><table class="tbl {cls}">']
    if cap:
        out.append(f'<caption class="sr">{cap}</caption>')
    out.append("<thead><tr>" + "".join(f'<th scope="col">{h}</th>' for h in heads) + "</tr></thead><tbody>")
    for r in rows:
        attrs = ""
        if isinstance(r, dict):
            attrs, r = f' class="{r["cls"]}"', r["cells"]
        out.append(f"<tr{attrs}>" + "".join(c if c.startswith("<td") else f"<td>{c}</td>" for c in r) + "</tr>")
    out.append("</tbody></table></div>")
    return "".join(out)


def sec(title, *body, acts="", cls="", note=""):
    head = f'<div class="sec-h"><h2>{title}</h2>{note}<div class="acts">{acts}</div></div>' if title else ""
    return f'<section class="sec {cls}">{head}{"".join(body)}</section>'


_fid = [0]


def field(label, value="", kind="text", help=None, err=None, cls="", ph="", disabled=False, required=False, rows=3):
    _fid[0] += 1
    fid = f"f{_fid[0]}"
    star = ' <span aria-hidden="true" class="req">*</span>' if required else ""
    desc = f' aria-describedby="{fid}-d"' if (help or err) else ""
    inv = ' aria-invalid="true"' if err else ""
    dis = " disabled" if disabled else ""
    req = ' aria-required="true"' if required else ""
    if kind == "textarea":
        ctl = f'<textarea id="{fid}" class="in {cls}" rows="{rows}" placeholder="{e(ph)}"{desc}{inv}{dis}>{e(value)}</textarea>'
    elif kind == "select":
        opts = "".join(f"<option>{e(o)}</option>" for o in value)
        ctl = f'<select id="{fid}" class="in {cls}"{desc}{dis}>{opts}</select>'
    else:
        ctl = f'<input id="{fid}" class="in {cls}" type="{kind}" value="{e(value)}" placeholder="{e(ph)}"{desc}{inv}{dis}{req}>'
    tail = ""
    if err:
        tail = f'<p id="{fid}-d" class="err" role="alert">{ic("error")}{err}</p>'
    elif help:
        tail = f'<p id="{fid}-d" class="help">{help}</p>'
    wide = " wide" if "wide" in cls else ""
    return f'<div class="fld{wide}"><label for="{fid}">{label}{star}</label>{ctl}{tail}</div>'


def check(label, on=False, kind="checkbox", name="", disabled=False, help=""):
    _fid[0] += 1
    fid = f"f{_fid[0]}"
    role = ' role="switch"' if kind == "switch" else ""
    typ = "checkbox" if kind == "switch" else kind
    hp = f'<span class="help">{help}</span>' if help else ""
    return (f'<label class="chk" for="{fid}"><input id="{fid}" type="{typ}"{role} name="{name or fid}"'
            f'{" checked" if on else ""}{" disabled" if disabled else ""}><span>{label}{hp}</span></label>')


def choice(title, help="", on=False, name="c", extra=""):
    """A radio card. Anything in `extra` (for example a dependent field) sits beside the label, never inside it."""
    _fid[0] += 1
    fid = f"f{_fid[0]}"
    hp = f'<span class="help">{help}</span>' if help else ""
    lab = (f'<label class="choice{" on" if on else ""}" for="{fid}"><input id="{fid}" type="radio" name="{name}"'
           f'{" checked" if on else ""}><span><b>{title}</b>{hp}</span></label>')
    return f'<div class="choice-wrap">{lab}<div class="choice-extra">{extra}</div></div>' if extra else lab


def form(*fields, cls=""):
    return f'<div class="form {cls}">{"".join(fields)}</div>'


def banner(kind, title, *body, acts="", role=None, icon=None):
    icons = {"ok": "check", "warn": "paused", "held": "held", "err": "error", "run": "run", "": "info"}
    r = f' role="{role}"' if role else ""
    a = f'<div class="row">{acts}</div>' if acts else ""
    return (f'<div class="bn {kind}"{r}><div class="bn-t">{ic(icon or icons.get(kind, "info"))}<span>{title}</span></div>'
            f'{"".join(f"<div class=bn-b>{b}</div>" for b in body)}{a}</div>').replace("class=bn-b", 'class="bn-b"')


def dialog(title, desc, *body, foot="", wide=False, label=None):
    d = f'<p class="t2 sm">{desc}</p>' if desc else ""
    return (f'<div class="scrim"><div class="dlg{" wide" if wide else ""}" role="dialog" aria-modal="true" '
            f'aria-label="{e(label or title)}"><header><h2>{title}</h2>{d}</header>'
            f'<div class="dlg-b">{"".join(body)}</div><footer>{foot}</footer></div></div>')


def board(*items, one=False):
    return f'<div class="board{" one" if one else ""}">' + "".join(f"<figure><figcaption>{c}</figcaption>{b}</figure>" for c, b in items) + "</div>"


def empty(title, text="", act="", role="status", icon=None):
    i = f'<span class="empty-ic">{ic(icon, "lg")}</span>' if icon else ""
    return f'<div class="empty" role="{role}">{i}<b>{title}</b><span>{text}</span>{act}</div>'


def loading(text):
    return (f'<div class="stack" role="status"><span class="t2">{text}</span><div class="skel" style="width:82%"></div>'
            '<div class="skel" style="width:64%"></div><div class="skel" style="width:73%"></div></div>')


def conseq(*lines):
    return '<ul class="conseq">' + "".join(f"<li>{ic(i)}<span>{t}</span></li>" for i, t in lines) + "</ul>"


def evid(*lines, title=None, health=None, specimen=False):
    t = f"<b>{title}</b>" if title else ""
    mark = f' data-evidence{"-specimen" if specimen else ""}="{health}"' if health else ""
    return f'<div class="evid"{mark}>{t}{"".join(f"<span>{x}</span>" for x in lines)}</div>'


def op(kind, state_html, *body, opid=None, foot="", icon="run", meta=""):
    oid = f'<span class="opid mono">op {opid} {ic("copy")}</span>' if opid else ""
    f = f'<div class="op-f">{foot}</div>' if foot else ""
    return (f'<div class="op"><div class="op-h">{ic(icon)}<b>{kind}</b>{state_html}{oid}<span class="sp"></span>{meta}</div>'
            f'<div class="op-b">{"".join(body)}</div>{f}</div>')


def meter(pct=None, label="Progress"):
    if pct is None:
        return f'<div class="meter unk" role="progressbar" aria-label="{label}" aria-valuetext="Progress not yet measurable"><i></i></div>'
    return (f'<div class="meter" role="progressbar" aria-label="{label}" aria-valuemin="0" aria-valuemax="100" '
            f'aria-valuenow="{pct}"><i style="width:{pct}%"></i></div>')


def bar(label, pct, text):
    """A labelled gauge, the only chart form the product has (server monitor, import progress)."""
    return (f'<div class="gauge"><div class="row"><span class="t2">{label}</span>{sp()}<span class="mono">{text}</span></div>'
            f'{meter(pct, label)}</div>')


def steps(names, current):
    out = ['<ol class="steps" aria-label="Progress">']
    for i, n in enumerate(names):
        cls = "done" if i < current else "now" if i == current else ""
        cur = ' aria-current="step"' if i == current else ""
        num = ic("check") if i < current else str(i + 1)
        out.append(f'<li class="{cls}"{cur}><span class="n">{num}</span>{n}</li>')
    return "".join(out) + "</ol>"


def seg(*labels, on=0, label="Filter"):
    return (f'<div class="seg" role="group" aria-label="{label}">' + "".join(
        f'<button type="button" aria-pressed="{"true" if i == on else "false"}">{l}</button>' for i, l in enumerate(labels)) + "</div>")


def select(label, *opts):
    _fid[0] += 1
    return (f'<label class="sr" for="f{_fid[0]}">{label}</label><select id="f{_fid[0]}" class="in auto">'
            + "".join(f"<option>{o}</option>" for o in opts) + "</select>")


def pager(text, prev=False, nxt=True):
    return (f'<div class="pager" role="status"><span>{text}</span>{sp()}{btn("Previous", "sm", disabled=not prev)}'
            f'{btn("Next", "sm", disabled=not nxt)}</div>')


def menu(*items):
    return '<div class="menu" role="menu">' + "".join(
        f'<button type="button" role="menuitem" class="{c}">{t}</button>' for t, c in items) + "</div>"


def diff(title, *lines):
    label = re.sub(r"\s+", " ", re.sub(r"<svg.*?</svg>|<[^>]+>", "", title, flags=re.S)).strip()
    out = [f'<div class="diff" role="region" aria-label="{label}" tabindex="0"><div class="hd">{title}</div>']
    for l in lines:
        cls = "add" if l.startswith("+") else "del" if l.startswith("-") else "ctx"
        out.append(f'<div class="{cls}">{e(l)}</div>')
    return "".join(out) + "</div>"


def log(*lines):
    return f'<pre class="log" role="region" aria-label="Log (not announced)" tabindex="0">{e(chr(10).join(lines))}</pre>'


def note(text):
    """A designer's note inside a prototype page. Not part of the design."""
    return f'<p class="proto-note">{ic("info")}<span>{text}</span></p>'


def pg(title, *body, crumbs=None, acts="", meta="", status="", sections="", cls="", aside=""):
    """Page scaffold. With `sections`, the option decides where the section navigation and status sit."""
    c = ""
    if crumbs:
        c = '<div class="crumbs">' + " / ".join(link(t, h) if h else t for t, h in crumbs) + "</div>"
    head = (f'<header class="pg-head">{c}<h1>{title}</h1>{meta}'
            f'{f"<div class=pg-actions>{acts}</div>" if acts else ""}</header>').replace("class=pg-actions", 'class="pg-actions"')
    st = f'<div class="pg-status">{status}</div>' if status or sections else ""
    nav = sections or ""
    kind = " has-sections" if sections else ""
    return f'<div class="pg{kind} {cls}">{head}{st}{nav}<div class="pg-body">{"".join(body)}</div></div>'
