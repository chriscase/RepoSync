"""Finish the #263 package: copy the tools, write the index, validate everything, write SHA256SUMS.
Run after build.py and docs.py:  python3 tools/finish.py <out>
"""
import hashlib
import pathlib
import re
import shutil
import sys

GEN = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(GEN))
import build  # noqa: E402
import docs  # noqa: E402
import docs_text  # noqa: E402
import mdhtml  # noqa: E402
import tokens  # noqa: E402

OUT = pathlib.Path(sys.argv[1]).resolve()
ALLOWED_EXTERNAL = "https://github.com/chriscase/RepoSync/"


def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def copy_tools():
    d = OUT / "tools"
    d.mkdir(parents=True, exist_ok=True)
    for src in sorted(GEN.iterdir()):
        if src.suffix in (".py", ".css", ".js", ".html") and src.is_file():
            shutil.copyfile(src, d / src.name)


def validate():
    """Structure and links for every HTML file except the tools sources."""
    problems, pages = [], []
    for p in sorted(OUT.rglob("*.html")):
        rel = p.relative_to(OUT)
        if rel.parts[0] == "tools" and p.name != "README.html":
            continue
        pages.append(p)
        proto = rel.parts[0] == "prototypes" or (rel.parts[0] == "options" and len(rel.parts) > 3)
        c = build.Check()
        text = p.read_text()
        c.feed(text)
        for x in c.errs[:4] + [f"unclosed <{t}>" for t in c.stack if t not in build.IMPL][:3]:
            problems.append(f"{rel}: {x}")
        for r in c.refs:
            if r.startswith("#"):
                continue
            if re.match(r"^[a-z][a-z0-9+.-]*:", r):
                if proto or not r.startswith(ALLOWED_EXTERNAL):
                    problems.append(f"{rel}: external reference {r}")
                continue
            if not (p.parent / r.split("#")[0]).resolve().exists():
                problems.append(f"{rel}: missing {r}")
        for use in set(re.findall(r'href="#(i-[a-z]+)"', text)):
            if f'id="{use}"' not in text:
                problems.append(f"{rel}: missing icon {use}")
        if proto and (re.search(r'(?:src|href|action|srcset|poster|data|formaction)\s*=\s*"(?:[a-z][a-z0-9+.-]*:)?//', text)
                      or re.search(r'style\s*=\s*"[^"]*url\(', text) or re.search(r"<style[^>]*>[^<]*(@import|url\()", text)
                      or re.search(r"<(iframe|object|embed|base|meta[^>]+http-equiv)", text)):
            problems.append(f"{rel}: outside reference in a prototype page")
    for css in list(OUT.glob("prototypes/assets/*.css")) + list(OUT.glob("options/*/*.css")):
        if re.search(r"https?://|@import|url\(", css.read_text()):
            problems.append(f"{css.relative_to(OUT)}: network or url() reference")
    js = (OUT / "prototypes/assets/proto.js").read_text()
    if re.search(r"https?://|fetch\(|XMLHttpRequest|import\(|WebSocket", js):
        problems.append("proto.js: network reference")
    _pages, token_problems = build.validate()
    problems += [x for x in token_problems if x.startswith("tokens ")]
    return pages, problems


def md_links():
    """Every relative link in a Markdown file resolves."""
    bad = []
    for p in sorted(OUT.rglob("*.md")):
        for url in re.findall(r"\]\(([^)\s]+)\)", p.read_text()):
            if re.match(r"^[a-z][a-z0-9+.-]*:", url) or url.startswith("#"):
                continue
            if not (p.parent / url.split("#")[0]).resolve().exists():
                bad.append(f"{p.relative_to(OUT)}: missing {url}")
    return bad


if __name__ == "__main__":
    import bridge
    import v11
    copy_tools()
    R = docs.rows()
    counts = {k: sum(1 for r in R if r["status"] == k) for k in ("covered", "explicit gap", "not applicable")}
    docs.write_md("history/README.md", v11.history_pointer_md(), "History")
    contrast = tokens.contrast_rows()
    assert all(r[7] for r in contrast)
    legacy = tokens.legacy_contrast_rows()
    assert all(r[7] for r in legacy if r[1] == "light")
    classes, brows, prow, _strings = bridge.analyse()
    low = sum(1 for p_ in prow for _k, v in p_[5] if v is not None and v < 4.5)
    extra = dict(pages=0, problems=0, contrast=len(contrast), legacy_light=sum(1 for r in legacy if r[1] == "light" and r[7]),
                 bridge=dict(classes=len(brows), undecided=sum(1 for r in brows if r[7] == "per-use decision"), pairs=len(prow), low=low))
    docs.write_md("README.md", docs_text.readme(R, counts, None, extra), "Package index")
    pages, problems = validate()
    extra.update(pages=len(pages), problems=len(problems))
    docs.write_md("README.md", docs_text.readme(R, counts, None, extra), "Package index")
    pages, problems = validate()
    problems += md_links()
    sums = OUT / "SHA256SUMS"
    if sums.exists():
        sums.unlink()
    files = sorted(p_ for p_ in OUT.rglob("*") if p_.is_file())
    sums.write_text("".join(f"{sha(p_)}  {p_.relative_to(OUT).as_posix()}\n" for p_ in files))
    total = sum(p_.stat().st_size for p_ in files) + sums.stat().st_size
    print(f"files (with SHA256SUMS): {len(files) + 1}; bytes: {total}; html checked: {len(pages)}; problems: {len(problems)}")
    for x in problems[:40]:
        print("  ", x)
    print("SHA256SUMS sha256:", sha(sums))
    print("coverage", counts, {k: sum(1 for r in R if r["delivery"] == k) for k in docs_text.DELIVERY})
    print("bridge", extra["bridge"], "existing layouts light pairs", extra["legacy_light"])
