"""A small Markdown to HTML converter for the #263 package documents.

Handles only what these documents use: headings, paragraphs, one-level bullet and numbered lists,
pipe tables, fenced code, inline code, bold, links and <br>. No dependencies.
Relative links ending in .md are rewritten to the .html rendering beside them.
"""
import html
import re

CSS = """
:root{color-scheme:light dark;--bg:#fbfbf8;--fg:#16181d;--fg2:#4a4f59;--line:#d6d6cc;--soft:#efefe8;--link:#1a4fa3;--code:#f0f0e9}
@media (prefers-color-scheme:dark){:root{--bg:#14161a;--fg:#e9e9e2;--fg2:#a9adb6;--line:#3a3e46;--soft:#1d2026;--link:#8fb4ff;--code:#1d2026}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.55 system-ui,-apple-system,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif}
main{max-width:1180px;margin:0 auto;padding:24px max(16px,env(safe-area-inset-right,0px)) 64px max(16px,env(safe-area-inset-left,0px))}
.top{font-size:.88em;color:var(--fg2);border-bottom:1px solid var(--line);padding-bottom:10px;margin-bottom:18px;display:flex;flex-wrap:wrap;gap:6px 16px}
h1{font:600 1.8rem/1.2 "Iowan Old Style","Palatino Linotype",Palatino,Georgia,serif;margin:0 0 .5em;text-wrap:balance}
h2{font-size:1.2rem;margin:2em 0 .5em;padding-top:.7em;border-top:1px solid var(--fg)}
h3{font-size:1rem;margin:1.5em 0 .4em}
p,li{max-width:78ch;overflow-wrap:anywhere}
h1,h2,h3{overflow-wrap:anywhere}
a{color:var(--link)}
a:focus-visible{outline:2px solid var(--link);outline-offset:2px}
code{font:.88em ui-monospace,"SF Mono","Cascadia Mono",Menlo,Consolas,monospace;background:var(--code);padding:.08em .3em;border-radius:3px;overflow-wrap:anywhere}
pre{background:var(--code);padding:12px;overflow-x:auto;border:1px solid var(--line)}
pre code{background:none;padding:0}
.tw{overflow-x:auto;margin:1em 0}
table{border-collapse:collapse;font-size:.88em;min-width:100%}
th,td{border-bottom:1px solid var(--line);padding:6px 10px 6px 0;text-align:left;vertical-align:top}
th{border-bottom-color:var(--fg);font-weight:650;white-space:nowrap}
td{min-width:8ch}
table.wide td{min-width:14ch}
"""


def _link(m, md_to_html):
    text, url = m.group(1), m.group(2)
    if md_to_html and not re.match(r"^[a-z][a-z0-9+.-]*:", url):
        url = re.sub(r"\.md(#|$)", r".html\1", url)
    return f'<a href="{url}">{text}</a>'


def inline(s, md_to_html=True):
    codes = []

    def keep(m):
        codes.append(html.escape(m.group(1).strip() if m.group(0).startswith("``") else m.group(1), quote=False))
        return f"\x00{len(codes) - 1}\x00"
    s = re.sub(r"``(.+?)``", keep, s)
    s = re.sub(r"`([^`]+)`", keep, s)
    s = re.sub(r"&(?![a-zA-Z]+;|#\d+;)", "&amp;", s).replace("<", "&lt;").replace(">", "&gt;")
    s = s.replace("&lt;br&gt;", "<br>")
    s = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", s)
    s = re.sub(r"(?<![*\w])\*([^*\s][^*]*?)\*(?![*\w])", r"<em>\1</em>", s)
    s = re.sub(r"\[([^\]]+)\]\(([^)\s]+)\)", lambda m: _link(m, md_to_html), s)
    return re.sub(r"\x00(\d+)\x00", lambda m: f"<code>{codes[int(m.group(1))]}</code>", s)


def cells(line):
    parts = re.split(r"(?<!\\)\|", line.strip().strip("|"))
    return [p.strip().replace("\\|", "|") for p in parts]


def convert(md):
    out, lines, i = [], md.split("\n"), 0
    while i < len(lines):
        ln = lines[i]
        if ln.startswith("```"):
            buf = []
            i += 1
            while i < len(lines) and not lines[i].startswith("```"):
                buf.append(lines[i])
                i += 1
            out.append("<pre><code>" + html.escape("\n".join(buf), quote=False) + "</code></pre>")
            i += 1
            continue
        m = re.match(r"^(#{1,4}) (.*)$", ln)
        if m:
            n = len(m.group(1))
            ident = re.sub(r"[^a-z0-9]+", "-", m.group(2).lower()).strip("-")
            out.append(f'<h{n} id="{ident}">{inline(m.group(2))}</h{n}>')
            i += 1
            continue
        if ln.startswith("|"):
            head = cells(ln)
            i += 2
            rows = []
            while i < len(lines) and lines[i].startswith("|"):
                rows.append(cells(lines[i]))
                i += 1
            cls = ' class="wide"' if len(head) > 6 else ""
            out.append(f'<div class="tw"><table{cls}><thead><tr>' + "".join(f"<th>{inline(c)}</th>" for c in head) + "</tr></thead><tbody>"
                       + "".join("<tr>" + "".join(f"<td>{inline(c)}</td>" for c in r) + "</tr>" for r in rows) + "</tbody></table></div>")
            continue
        if re.match(r"^(- |\d+\. )", ln):
            tag = "ul" if ln.startswith("- ") else "ol"
            items = []
            while i < len(lines) and re.match(r"^(- |\d+\. )", lines[i]):
                item = re.sub(r"^(- |\d+\. )", "", lines[i])
                i += 1
                while i < len(lines) and lines[i].startswith("  ") and lines[i].strip():
                    item += " " + lines[i].strip()
                    i += 1
                items.append(f"<li>{inline(item)}</li>")
            out.append(f"<{tag}>" + "".join(items) + f"</{tag}>")
            continue
        if not ln.strip():
            i += 1
            continue
        buf = []
        while i < len(lines) and lines[i].strip() and not re.match(r"^(#{1,4} |\||- |\d+\. |```)", lines[i]):
            buf.append(lines[i].strip())
            i += 1
        out.append("<p>" + inline(" ".join(buf)) + "</p>")
    return "\n".join(out)


def page(md, title, up):
    """Full HTML document for a Markdown source. `up` is the relative path to the package root."""
    return (f'<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">\n'
            f"<title>{html.escape(title)} · RepoSync #263 design package v1.6</title>\n<style>{CSS}</style>\n</head>\n<body>\n<main>\n"
            f'<p class="top"><a href="{up}README.html">Package index</a><a href="{up}DECISIONS.html">Decisions</a><a href="{up}prototypes/index.html">Prototypes</a>'
            f'<a href="{up}matrix/surface-state-matrix.html">Matrix</a><span>RepoSync #263 design package v1.6 · design only, not the implemented product</span></p>\n'
            f"{convert(md)}\n</main>\n</body>\n</html>\n")
