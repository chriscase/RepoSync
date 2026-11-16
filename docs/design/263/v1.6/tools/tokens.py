"""Token values for the three options, light and dark.

The first twelve names per theme are exactly the --rs-shell-* tokens of the #262 seam
(web-ui/src/layouts/layoutTokens.css:17-28). The rest are the proposed extension to page content.
"""
SHELL = ["canvas", "chrome", "border", "hover", "text-strong", "text-muted", "accent", "accent-text",
         "alert", "alert-text", "notice-text", "focus"]

T = {
 "ledger": {
  "light": {
   "shell": ["#fbfbf8", "#fbfbf8", "#16181d", "#e7e7df", "#16181d", "#4a4f59", "#16181d", "#ffffff", "#b3261e", "#ffffff", "#7a4f00", "#1a4fa3"],
   "bg": "#fbfbf8", "surface": "#ffffff", "surface-2": "#f3f3ee", "surface-3": "#e7e7df", "line": "#d6d6cd", "line-strong": "#8f8f85",
   "text": "#16181d", "text-2": "#3d424b", "text-3": "#5a606b", "accent": "#16181d", "accent-text": "#ffffff", "accent-soft": "#ecece5",
   "link": "#1a4fa3", "focus": "#1a4fa3", "ok": "#17663c", "run": "#1b5fa8", "warn": "#855600", "held": "#a3460a", "danger": "#b3261e", "off": "#5a606b",
   "ok-soft": "#e3f1e7", "run-soft": "#e3edf8", "warn-soft": "#f8eed2", "held-soft": "#fbe6d6", "danger-soft": "#fbe4e1",
   "svn": "#1b5fa8", "git": "#6d3fc4", "code-bg": "#f1f1ec", "code-text": "#16181d", "add-bg": "#dff3e4", "del-bg": "#fbe1de",
   "scrim": "rgba(22,24,29,.45)", "shadow": "0 8px 28px rgba(22,24,29,.18)", "danger-solid": "#b3261e", "danger-solid-text": "#ffffff"},
  "dark": {
   "shell": ["#131417", "#131417", "#7d7b76", "#2a2d34", "#eceae4", "#b4b2ab", "#eceae4", "#131417", "#ff8a80", "#2a0806", "#e6b450", "#8fb8ff"],
   "bg": "#131417", "surface": "#1a1c20", "surface-2": "#212329", "surface-3": "#2a2d34", "line": "#353841", "line-strong": "#6b6f7a",
   "text": "#eceae4", "text-2": "#c5c3bc", "text-3": "#a09f98", "accent": "#eceae4", "accent-text": "#131417", "accent-soft": "#2a2d34",
   "link": "#8fb8ff", "focus": "#8fb8ff", "ok": "#6fd39a", "run": "#7fb4f5", "warn": "#e6b450", "held": "#f0935a", "danger": "#ff8a80", "off": "#a09f98",
   "ok-soft": "#16301f", "run-soft": "#152a44", "warn-soft": "#3a2d0c", "held-soft": "#3d2212", "danger-soft": "#421a17",
   "svn": "#7fb4f5", "git": "#c3a2ff", "code-bg": "#0e0f12", "code-text": "#eceae4", "add-bg": "#153220", "del-bg": "#45201c",
   "scrim": "rgba(0,0,0,.6)", "shadow": "0 10px 32px rgba(0,0,0,.55)", "danger-solid": "#c9372c", "danger-solid-text": "#ffffff"},
  "type": {"font-ui": 'system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif',
           "font-display": '"Iowan Old Style", "Palatino Linotype", Palatino, "Book Antiqua", Georgia, serif',
           "font-mono": 'ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace',
           "display-weight": "600", "fs": "15px", "lh": "1.55", "h1": "1.85rem", "h2": "1.15rem",
           "radius": "2px", "radius-lg": "2px", "gap": "12px", "gap-lg": "22px", "pad": "18px", "pad-sm": "12px",
           "cell": "9px 10px", "control-h": "34px", "btn-pad": "14px"},
 },
 "console": {
  "dark": {
   "shell": ["#0f141a", "#0a0e13", "#2a3644", "#1b242e", "#e6edf3", "#9aa7b4", "#2fb7c6", "#04161a", "#f47067", "#1a0605", "#e3b341", "#6fd3de"],
   "bg": "#0f141a", "surface": "#151c24", "surface-2": "#1b242e", "surface-3": "#24303c", "line": "#2a3644", "line-strong": "#5a6b7d",
   "text": "#e6edf3", "text-2": "#b7c2cd", "text-3": "#8f9caa", "accent": "#2fb7c6", "accent-text": "#04161a", "accent-soft": "#0f3038",
   "link": "#6fd3de", "focus": "#6fd3de", "ok": "#4cc38a", "run": "#5bb8f0", "warn": "#e3b341", "held": "#f0883e", "danger": "#f47067", "off": "#8f9caa",
   "ok-soft": "#12301f", "run-soft": "#10283b", "warn-soft": "#3b2f0b", "held-soft": "#3d230f", "danger-soft": "#46191a",
   "svn": "#6cb6ff", "git": "#c297ff", "code-bg": "#0a0e13", "code-text": "#e6edf3", "add-bg": "#12301f", "del-bg": "#46191a",
   "scrim": "rgba(0,0,0,.62)", "shadow": "0 10px 30px rgba(0,0,0,.6)", "danger-solid": "#c93c37", "danger-solid-text": "#ffffff"},
  "light": {
   "shell": ["#f4f6f8", "#e9edf1", "#c5ced6", "#dde3e9", "#111820", "#4a5764", "#0b6f7c", "#ffffff", "#b42318", "#ffffff", "#7a4f00", "#0b6f7c"],
   "bg": "#f4f6f8", "surface": "#ffffff", "surface-2": "#eef1f4", "surface-3": "#e2e7ec", "line": "#cfd6dd", "line-strong": "#7f8c99",
   "text": "#111820", "text-2": "#36424e", "text-3": "#566371", "accent": "#0b6f7c", "accent-text": "#ffffff", "accent-soft": "#d9eef1",
   "link": "#0b6370", "focus": "#0b6f7c", "ok": "#1a7446", "run": "#1766b3", "warn": "#855600", "held": "#a3460a", "danger": "#b42318", "off": "#566371",
   "ok-soft": "#dff2e6", "run-soft": "#e0ecf8", "warn-soft": "#f7edcf", "held-soft": "#fbe5d5", "danger-soft": "#fbe3e0",
   "svn": "#1766b3", "git": "#6f42c1", "code-bg": "#eaeef2", "code-text": "#111820", "add-bg": "#dbf1e2", "del-bg": "#fbe0dd",
   "scrim": "rgba(17,24,32,.45)", "shadow": "0 8px 26px rgba(17,24,32,.2)", "danger-solid": "#b42318", "danger-solid-text": "#ffffff"},
  "type": {"font-ui": '"Segoe UI", system-ui, -apple-system, "Helvetica Neue", Arial, sans-serif',
           "font-display": '"Segoe UI", system-ui, -apple-system, "Helvetica Neue", Arial, sans-serif',
           "font-mono": 'ui-monospace, "Cascadia Mono", "SF Mono", Menlo, Consolas, monospace',
           "display-weight": "650", "fs": "13px", "lh": "1.45", "h1": "1.35rem", "h2": "0.98rem",
           "radius": "4px", "radius-lg": "6px", "gap": "8px", "gap-lg": "14px", "pad": "12px", "pad-sm": "9px",
           "cell": "5px 9px", "control-h": "28px", "btn-pad": "10px"},
 },
 "guide": {
  "light": {
   "shell": ["#f2f4f8", "#ffffff", "#dbe0ea", "#eaeef5", "#131722", "#4b556a", "#2456e6", "#ffffff", "#b42318", "#ffffff", "#7a4f00", "#2456e6"],
   "bg": "#f2f4f8", "surface": "#ffffff", "surface-2": "#f6f8fb", "surface-3": "#e6eaf2", "line": "#dbe0ea", "line-strong": "#8792a6",
   "text": "#131722", "text-2": "#3b4354", "text-3": "#586176", "accent": "#2456e6", "accent-text": "#ffffff", "accent-soft": "#e3ebff",
   "link": "#1f4fd6", "focus": "#2456e6", "ok": "#157040", "run": "#1f4fd6", "warn": "#855600", "held": "#a3460a", "danger": "#b42318", "off": "#586176",
   "ok-soft": "#e0f3e7", "run-soft": "#e3ebff", "warn-soft": "#f8eed0", "held-soft": "#fbe6d6", "danger-soft": "#fbe3e0",
   "svn": "#1b66ab", "git": "#7438c9", "code-bg": "#eef1f6", "code-text": "#131722", "add-bg": "#dcf2e3", "del-bg": "#fbe0dd",
   "scrim": "rgba(19,23,34,.45)", "shadow": "0 12px 34px rgba(19,23,34,.16)", "danger-solid": "#b42318", "danger-solid-text": "#ffffff"},
  "dark": {
   "shell": ["#12151c", "#171b24", "#313848", "#252b3a", "#eef1f7", "#a6afc0", "#7da2ff", "#0b1020", "#ff8f85", "#2a0806", "#e8b84e", "#9bb8ff"],
   "bg": "#12151c", "surface": "#1a1e28", "surface-2": "#212634", "surface-3": "#2b3142", "line": "#313848", "line-strong": "#66708a",
   "text": "#eef1f7", "text-2": "#c3c9d6", "text-3": "#9aa3b5", "accent": "#7da2ff", "accent-text": "#0b1020", "accent-soft": "#1c2747",
   "link": "#9bb8ff", "focus": "#9bb8ff", "ok": "#63d19a", "run": "#7da2ff", "warn": "#e8b84e", "held": "#f29a62", "danger": "#ff8f85", "off": "#9aa3b5",
   "ok-soft": "#143023", "run-soft": "#1c2747", "warn-soft": "#3a2e0d", "held-soft": "#3e2413", "danger-soft": "#431b18",
   "svn": "#79b8f7", "git": "#c7a4ff", "code-bg": "#0d1016", "code-text": "#eef1f7", "add-bg": "#143023", "del-bg": "#431b18",
   "scrim": "rgba(0,0,0,.6)", "shadow": "0 14px 36px rgba(0,0,0,.55)", "danger-solid": "#c93c37", "danger-solid-text": "#ffffff"},
  "type": {"font-ui": 'ui-rounded, "SF Pro Rounded", "Hiragino Maru Gothic ProN", "Segoe UI", system-ui, sans-serif',
           "font-display": 'ui-rounded, "SF Pro Rounded", "Hiragino Maru Gothic ProN", "Segoe UI", system-ui, sans-serif',
           "font-mono": 'ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace',
           "display-weight": "700", "fs": "16px", "lh": "1.5", "h1": "1.7rem", "h2": "1.12rem",
           "radius": "10px", "radius-lg": "16px", "gap": "12px", "gap-lg": "20px", "pad": "20px", "pad-sm": "14px",
           "cell": "12px 14px", "control-h": "42px", "btn-pad": "18px"},
 },
}


def css(opt):
    """options/<opt>/tokens.css"""
    d = T[opt]
    out = [f"/* RepoSync #263 option \"{opt}\": tokens.\n"
           " * The --rs-shell-* names are the existing #262 seam. The other names are the proposed\n"
           " * extension of that seam to page content. In the product the selector is\n"
           f" * .rs-layout-shell[data-rs-layout='{opt}'][data-rs-theme='<theme>']; the html[...] form is for these prototypes only. */"]
    sel0 = f".rs-layout-shell[data-rs-layout='{opt}'], html[data-rs-layout='{opt}']"
    out.append(sel0 + " {\n" + "".join(f"  --rs-{k}: {v};\n" for k, v in d["type"].items()) + "}")
    for theme in ("light", "dark"):
        v = d[theme]
        sel = (f".rs-layout-shell[data-rs-layout='{opt}'][data-rs-theme='{theme}'],\n"
               f"html[data-rs-layout='{opt}'][data-rs-theme='{theme}']")
        body = "".join(f"  --rs-shell-{n}: {val};\n" for n, val in zip(SHELL, v["shell"]))
        body += "".join(f"  --rs-{k}: {val};\n" for k, val in v.items() if k != "shell")
        body += f"  color-scheme: {theme};\n"
        out.append(sel + " {\n" + body + "}")
    return "\n\n".join(out) + "\n"


def _lum(hexv):
    hexv = hexv.lstrip("#")
    ch = [int(hexv[i:i + 2], 16) / 255 for i in (0, 2, 4)]
    lin = [c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4 for c in ch]
    return 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2]


def ratio(a, b):
    la, lb = _lum(a), _lum(b)
    hi, lo = max(la, lb), min(la, lb)
    return (hi + 0.05) / (lo + 0.05)


# (label, foreground token, background token, minimum). "shell:" prefixes refer to the shell list.
PAIRS = [
    ("Body text on page", "text", "bg", 4.5), ("Body text on surface", "text", "surface", 4.5),
    ("Secondary text on surface", "text-2", "surface", 4.5), ("Muted text on page", "text-3", "bg", 4.5),
    ("Muted text on surface", "text-3", "surface", 4.5), ("Muted text on inset", "text-3", "surface-2", 4.5),
    ("Link on surface", "link", "surface", 4.5), ("Primary button label", "accent-text", "accent", 4.5),
    ("Danger button label", "danger-solid-text", "danger-solid", 4.5),
    ("OK state on surface", "ok", "surface", 4.5), ("Running state on surface", "run", "surface", 4.5),
    ("Warning state on surface", "warn", "surface", 4.5), ("Held state on surface", "held", "surface", 4.5),
    ("Danger state on surface", "danger", "surface", 4.5), ("Neutral state on surface", "off", "surface", 4.5),
    ("OK text on its tint", "text", "ok-soft", 4.5), ("Warning state on its tint", "warn", "warn-soft", 4.5),
    ("Held state on its tint", "held", "held-soft", 4.5), ("Danger state on its tint", "danger", "danger-soft", 4.5),
    ("SVN reference on surface", "svn", "surface", 4.5), ("Git reference on surface", "git", "surface", 4.5),
    ("Code text on code background", "code-text", "code-bg", 4.5),
    ("Control border on surface", "line-strong", "surface", 3.0), ("Focus ring on surface", "focus", "surface", 3.0),
    ("Shell link on chrome", "shell:text-muted", "shell:chrome", 4.5), ("Shell active text on chrome", "shell:text-strong", "shell:chrome", 4.5),
    ("Shell active label on accent", "shell:accent-text", "shell:accent", 4.5), ("Conflict count", "shell:alert-text", "shell:alert", 4.5),
    ("Chooser notice on chrome", "shell:notice-text", "shell:chrome", 4.5), ("Shell focus ring on chrome", "shell:focus", "shell:chrome", 3.0),
]


def _val(theme, name):
    if name.startswith("shell:"):
        return theme["shell"][SHELL.index(name[6:])]
    return theme[name]


def contrast_rows():
    rows = []
    for opt in T:
        for theme in ("light", "dark"):
            for label, fg, bg, need in PAIRS:
                r = ratio(_val(T[opt][theme], fg), _val(T[opt][theme], bg))
                rows.append((opt, theme, label, fg, bg, round(r, 2), need, r >= need))
    return rows


# ---------------------------------------------------------------- the existing three layouts
# Classic, Individual and Team share one palette. "dark" approximates what they show today (Tailwind greys and the
# #262 shell values); it is used only to draw the sign-in page and a comparison panel, and nothing in the product
# changes for it. "light" is the proposed light theme for those three layouts.
LEGACY = {
 "type": {"font-ui": '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif',
          "font-display": '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif',
          "font-mono": 'ui-monospace, "SF Mono", Menlo, Consolas, monospace', "display-weight": "700", "fs": "14px", "lh": "1.5", "h1": "1.5rem", "h2": "1.125rem",
          "radius": "6px", "radius-lg": "8px", "gap": "10px", "gap-lg": "18px", "pad": "16px", "pad-sm": "12px", "cell": "8px 12px", "control-h": "36px", "btn-pad": "14px"},
 "dark": {
  "shell": ["#111827", "#030712", "#1f2937", "#374151", "#ffffff", "#9ca3af", "#2563eb", "#ffffff", "#dc2626", "#fee2e2", "#fde68a", "#93c5fd"],
  "bg": "#111827", "surface": "#1f2937", "surface-2": "#374151", "surface-3": "#4b5563", "line": "#374151", "line-strong": "#4b5563",
  "text": "#f3f4f6", "text-2": "#d1d5db", "text-3": "#9ca3af", "accent": "#2563eb", "accent-text": "#ffffff", "accent-soft": "#1e3a8a",
  "link": "#60a5fa", "focus": "#93c5fd", "ok": "#4ade80", "run": "#60a5fa", "warn": "#facc15", "held": "#fb923c", "danger": "#f87171", "off": "#9ca3af",
  "ok-soft": "#173b2a", "run-soft": "#1e2f57", "warn-soft": "#3b3413", "held-soft": "#3f2a17", "danger-soft": "#4a1f24",
  "svn": "#60a5fa", "git": "#c084fc", "code-bg": "#030712", "code-text": "#e5e7eb", "add-bg": "#173b2a", "del-bg": "#4a1f24",
  "scrim": "rgba(0,0,0,.65)", "shadow": "0 14px 36px rgba(0,0,0,.5)", "danger-solid": "#dc2626", "danger-solid-text": "#ffffff"},
 "light": {
  "shell": ["#f3f4f6", "#ffffff", "#d1d5db", "#e5e7eb", "#111827", "#4b5563", "#2563eb", "#ffffff", "#b91c1c", "#ffffff", "#92400e", "#1d4ed8"],
  "bg": "#f3f4f6", "surface": "#ffffff", "surface-2": "#f3f4f6", "surface-3": "#e5e7eb", "line": "#e5e7eb", "line-strong": "#7f8a9a",
  "text": "#111827", "text-2": "#374151", "text-3": "#4b5563", "accent": "#2563eb", "accent-text": "#ffffff", "accent-soft": "#dbeafe",
  "link": "#1d4ed8", "focus": "#1d4ed8", "ok": "#15803d", "run": "#1d4ed8", "warn": "#925207", "held": "#b93c0c", "danger": "#b91c1c", "off": "#4b5563",
  "ok-soft": "#dcfce7", "run-soft": "#dbeafe", "warn-soft": "#fef9c3", "held-soft": "#ffedd5", "danger-soft": "#fee2e2",
  "svn": "#1d4ed8", "git": "#7e22ce", "code-bg": "#f3f4f6", "code-text": "#111827", "add-bg": "#dcfce7", "del-bg": "#fee2e2",
  "scrim": "rgba(17,24,39,.5)", "shadow": "0 10px 30px rgba(17,24,39,.18)", "danger-solid": "#b91c1c", "danger-solid-text": "#ffffff"},
}


def _block(v, scheme):
    return ("".join(f"  --rs-shell-{n}: {val};\n" for n, val in zip(SHELL, v["shell"])) + "".join(f"  --rs-{k}: {val};\n" for k, val in v.items() if k != "shell")
            + f"  color-scheme: {scheme};\n")


def legacy_css():
    """prototypes/assets/legacy.css: the sign-in page and the comparison panels for the existing three layouts."""
    typ = "".join(f"  --rs-{k}: {v};\n" for k, v in LEGACY["type"].items())
    base = "html[data-rs-layout='legacy'], .rs-layout-shell[data-rs-layout='legacy']"
    return ("/* RepoSync #263: palette of the existing three layouts (Classic, Individual, Team).\n"
            " * Dark approximates today's colours and is for drawing only. Light is the proposed light theme.\n"
            " * The sign-in page uses the dark set whatever theme is chosen: it has one appearance for everyone. */\n"
            f"{base}, .legacy-scope {{\n{typ}}}\n\n{base}, .legacy-scope[data-scope='dark'] {{\n{_block(LEGACY['dark'], 'dark')}}}\n\n"
            f".legacy-scope[data-scope='light'] {{\n{_block(LEGACY['light'], 'light')}}}\n\n"
            ".legacy-scope { background: var(--rs-bg); color: var(--rs-text); font: var(--rs-fs)/var(--rs-lh) var(--rs-font-ui); border: 1px solid var(--rs-line-strong); border-radius: var(--rs-radius-lg); min-width: 0; overflow: hidden; }\n"
            ".legacy-bar { background: var(--rs-shell-chrome); border-bottom: 1px solid var(--rs-shell-border); color: var(--rs-shell-text-muted); display: flex; flex-wrap: wrap; gap: 4px 14px; align-items: center; padding: 10px 14px; font-weight: 600; }\n"
            ".legacy-bar b { color: var(--rs-shell-text-strong); letter-spacing: 0.05em; }\n"
            ".legacy-bar .on { background: var(--rs-shell-accent); color: var(--rs-shell-accent-text); padding: 2px 10px; border-radius: 6px; }\n"
            ".legacy-bar .count { background: var(--rs-shell-alert); color: var(--rs-shell-alert-text); border-radius: 999px; padding: 0 0.5em; font-size: 0.8em; }\n"
            ".legacy-body { padding: var(--rs-pad); display: grid; gap: var(--rs-gap-lg); min-width: 0; }\n.legacy-body > * { min-width: 0; }\n")


def legacy_contrast_rows():
    rows = []
    for theme in ("light", "dark"):
        for label, fg, bg, need in PAIRS:
            r = ratio(_val(LEGACY[theme], fg), _val(LEGACY[theme], bg))
            rows.append(("existing layouts", theme, label, fg, bg, round(r, 2), need, r >= need))
    return rows
