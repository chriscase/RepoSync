"""Colour bridge analysis for #263 v1.1.

Reads every non-test .tsx file under web-ui/src at the partial-52 tip with `git show` (nothing is checked out or run),
lists the Tailwind colour utilities in use, maps each to a page token by rule, and computes contrast for every
background and text colour that appear in the same class string, under each token set.
"""
import collections
import re
import subprocess
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import resolve as Z  # noqa: E402
import tokens  # noqa: E402

COLOURS = "gray|slate|zinc|neutral|stone|red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose|white|black"
UTIL = re.compile(rf"^((?:[a-z0-9-]+:)*)(bg|text|border(?:-[trblxy])?|ring|ring-offset|divide|placeholder|outline|from|via|to|fill|stroke|accent|caret|decoration)-({COLOURS})(?:-(\d{{2,3}}))?(?:/(\d+))?$")
SOFT = {"red": "danger-soft", "green": "ok-soft", "emerald": "ok-soft", "yellow": "warn-soft", "amber": "warn-soft", "orange": "held-soft",
        "blue": "accent-soft", "indigo": "accent-soft", "purple": "accent-soft", "violet": "accent-soft", "cyan": "run-soft", "sky": "run-soft", "teal": "run-soft"}
TONE = {"red": "danger", "green": "ok", "emerald": "ok", "yellow": "warn", "amber": "warn", "orange": "held", "blue": "link", "indigo": "link",
        "purple": "git", "violet": "git", "cyan": "run", "sky": "run", "teal": "run"}
SOLID = {"red": "danger-solid", "blue": "accent", "indigo": "accent", "green": "ok", "emerald": "ok", "yellow": "warn", "amber": "warn", "orange": "held",
         "purple": "git", "violet": "git", "cyan": "run", "sky": "run", "teal": "run"}
GREYS = {"gray", "slate", "zinc", "neutral", "stone"}


def token_for(util, colour, step, alpha=None):
    """The page token a utility resolves to. `None` means a per-use decision is needed."""
    n = int(step) if step else None
    tint = alpha is not None and int(alpha) <= 50
    kind = "bg" if util in ("bg", "from", "via", "to", "fill") else "text" if util in ("text", "placeholder", "caret", "decoration", "stroke") else "line"
    if colour == "white":
        return {"bg": None, "text": None, "line": "line"}[kind]          # white is both a surface and on-colour text today
    if colour == "black":
        return {"bg": "scrim", "text": "text", "line": "line-strong"}[kind]
    if colour in GREYS:
        if kind == "bg":
            return "bg" if n >= 900 else "surface" if n == 800 else "surface-2" if n == 700 else "surface-3" if n >= 500 else "surface-2" if n >= 200 else "surface"
        if kind == "text":
            return "text" if n <= 200 else "text-2" if n <= 400 else "text-3"
        return "line-strong" if n <= 600 else "line"
    if kind == "bg":
        return SOFT[colour] if tint or n >= 800 or n <= 200 else SOLID[colour]
    if kind == "text":
        return "text" if n <= 200 else TONE[colour]                       # pale tinted text sits on its own tint
    return TONE[colour] if colour != "blue" or util not in ("ring", "outline") else "focus"


def files():
    out = subprocess.run(["git", "-C", Z.REPO, "ls-tree", "-r", "--name-only", Z.P, "web-ui/src"], capture_output=True, text=True).stdout.split("\n")
    return [f for f in out if f.endswith(".tsx") and ".test." not in f]


def literals(text):
    for m in re.finditer(r'"([^"\n]*)"|\'([^\'\n]*)\'|`([^`]*)`', text):
        s = next(g for g in m.groups() if g is not None)
        yield re.sub(r"\$\{[^}]*\}", " ", s)


def scan():
    classes, pairs, strings = collections.Counter(), collections.Counter(), 0
    for f in files():
        text = subprocess.run(["git", "-C", Z.REPO, "show", f"{Z.P}:{f}"], capture_output=True, text=True).stdout
        for lit in literals(text):
            found = [(w, UTIL.match(w)) for w in lit.split()]
            found = [(w, m) for w, m in found if m]
            if not found:
                continue
            strings += 1
            for w, _m in found:
                classes[w] += 1
            base = [(w, m) for w, m in found if not m.group(1)]
            bgs = [(w, m) for w, m in base if m.group(2) == "bg"]
            txts = [(w, m) for w, m in base if m.group(2) == "text"]
            for bw, _b in bgs:
                for tw, _t in txts:
                    pairs[(bw, tw)] += 1
    return classes, pairs, strings


def sets():
    out = {("existing layouts", "light"): tokens.LEGACY["light"]}
    for o in tokens.T:
        for t in ("light", "dark"):
            out[(o, t)] = tokens.T[o][t]
    return out


def analyse():
    classes, pairs, strings = scan()
    rows = []
    for w, n in sorted(classes.items(), key=lambda x: (-x[1], x[0])):
        m = UTIL.match(w)
        rows.append((w, n, m.group(1) or "none", m.group(2), m.group(3), m.group(4) or "none", m.group(5) or "none", token_for(m.group(2), m.group(3), m.group(4), m.group(5)) or "per-use decision"))
    prow = []
    for (bw, tw), n in sorted(pairs.items(), key=lambda x: (-x[1], x[0])):
        b, t = UTIL.match(bw), UTIL.match(tw)
        bt, tt = token_for("bg", b.group(3), b.group(4), b.group(5)), token_for("text", t.group(3), t.group(4))
        # white text on a solid colour is that colour's on-colour text; white on anything else is body text
        if t.group(3) == "white":
            tt = {"accent": "accent-text", "danger-solid": "danger-solid-text"}.get(bt, "surface" if bt in ("ok", "warn", "held", "git", "run") else "text")
        if b.group(3) == "white":
            bt = "surface"
        res = []
        for key, tk in sets().items():
            if tt == "per-use decision" or bt == "scrim":
                res.append((key, None))
            else:
                res.append((key, round(tokens.ratio(tk[tt], tk[bt]), 2)))
        prow.append((bw, tw, n, bt, tt, res))
    return classes, rows, prow, strings


if __name__ == "__main__":
    classes, rows, prow, strings = analyse()
    print("files", len(files()), "class strings with colour", strings, "distinct colour classes", len(rows), "uses", sum(classes.values()))
    print("per-use decision classes:", [(r[0], r[1]) for r in rows if r[7] == "per-use decision"])
    print("distinct co-located bg+text pairs:", len(prow), "uses", sum(p[2] for p in prow))
    undecided = [(p[0], p[1], p[2]) for p in prow if p[4] == "per-use decision"]
    print("pairs needing a per-use decision:", undecided)
    for key in sets():
        low = [(p[0], p[1], p[2], dict(p[5])[key]) for p in prow if dict(p[5])[key] is not None and dict(p[5])[key] < 4.5]
        print(key, "pairs below 4.5:", len(low), "uses", sum(x[2] for x in low), low[:6])
    white_text = sum(n for w, n in classes.items() if UTIL.match(w).group(2) == "text" and UTIL.match(w).group(3) == "white" and not UTIL.match(w).group(1))
    white_with_bg = sum(p[2] for p in prow if UTIL.match(p[1]).group(3) == "white")
    print("base text-white uses", white_text, "of which in a string that also sets a background", white_with_bg)
