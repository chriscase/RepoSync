"""Screens G01-G04: sign-in, dashboard, repositories, repository detail. Synthetic data only."""
from kit import *

REPOS = [  # name, svn, git, state, observed, last sync, rev, sha, children
    ("kiln-controller", "trunk", "main", "idle", "8s ago", "14:06:12", "r4127", "9f3c2ab", [
        ("pid-tuning", "branches/pid-tuning", "pid-tuning", "running", "13:58:40", "r4125", "77ab01c"),
        ("display-v2", "branches/display-v2", "display-v2", "conflict", "13:41:07", "r4119", "c41d9e0")]),
    ("loom-patterns", "trunk", "main", "held", "8s ago", "Not recorded", "r750", "8a02f5d", []),
    ("greenhouse-telemetry", "trunk", "main", "paused", "8s ago", "11:22:03", "r612", "0be77f1", []),
    ("woodshop-inventory", "trunk", "main", "disabled", "8s ago", "9 Nov, 17:40", "r88", "5aa0c2d", []),
]
EV = {  # health -> (first line, observation, last successful sync, coverage, freshness, extra line)
    "idle": ("Idle (stored state). Status observed.", "14:08:33 local (2026-11-12T19:08:33Z)", "14:06:12", "complete snapshot", "recorded", None),
    "held": ("Held. Status observed.", "14:08:32 local (2026-11-12T19:08:32Z)", "Not recorded", "incomplete / unknown", "never_recorded",
             "loom-patterns: import — reconciliation_required (hold retained)."),
    "paused": ("Paused after error. Status observed.", "14:08:35 local (2026-11-12T19:08:35Z)", "11:22:03", "complete snapshot", "recorded", None),
    "unknown": ("Unknown. Status unavailable.", "Unknown", "Unknown", "unknown", "unknown", None),
}


def evidence(health, specimen=False):
    """The status evidence for one health value. Held, paused and unknown never borrow the idle text."""
    first, obs, last, cover, fresh, extra = EV[health]
    second = ("Sync configuration: unknown. No supported observation is available. Health, freshness and operation evidence remain unknown." if health == "unknown"
              else "Sync configuration: enabled. A status snapshot is not remote-freshness, reconciliation or process-collection proof.")
    lines = [first, second,
             f"Observation: {obs}. Last successful sync: {last}.", f"Supported durable-read coverage: {cover}. This is not a lease or collection proof.",
             f"Source freshness: {fresh}. Remote contents: not verified by this status read."]
    return evid(*(lines + ([extra] if extra else [])), health=health, specimen=specimen)


EVIDENCE = evidence("idle")


def statline(state, observed, last, position, inflight="Nothing recorded", errors="0", label=None, stale=False):
    st = stamp(f"Stale · last observed {observed}", "stale") if stale else stamp(f"observed {observed}")
    cell = lambda k, v: f'<div><span class="k">{k}</span><span class="v">{v}</span></div>'
    return (f'<div class="statline" role="status" data-statline="{state}">' + f'<div class="row">{pill(state, label, big=True)}{st}</div>'
            + cell("Last successful sync", last) + cell("Position recorded", position) + cell("In flight", inflight)
            + cell("Unacknowledged errors (24h)", errors)
            + '<span class="sp"></span>' + link("What this status proves", "g04-overview.html") + "</div>")


def repo_sections(cur, held=False):
    items = [("overview", "Overview", ""), ("activity", "Activity", ""), ("branches", "Branch pairs", "2"),
             ("operations", "Operations", "1 held" if held else "1"), ("settings", "Settings", "")]
    out = ['<nav class="pg-sections" aria-label="Repository sections">']
    for slug, label, n in items:
        c = ' aria-current="page"' if slug == cur else ""
        badge = f'<span class="n">{n}</span>' if n else ""
        out.append(f'<a href="g04-{slug}.html"{c}>{label}{badge}</a>')
    return "".join(out) + "</nav>"


def repo_head_acts():
    return btn("Request sync", icon="run") + btn("Pause sync…", "warn", icon="off")


REPO_META = (chip("Sync configured enabled") + '<span class="mono t2 sm">' + svn("svn.makerspace.example/kiln-controller/trunk")
             + " " + ic("swap") + " " + git("makerspace/kiln-controller · main") + "</span>")
CRUMB = [("Repositories", "g03-repositories.html"), ("kiln-controller", None)]


def repo_page(cur, *body, held=False, status=None, acts=None, title="kiln-controller", meta=REPO_META):
    st = status or statline("idle", "14:08:33 · 6s ago", "14:06:12 · 2m ago", pos("r4127", "9f3c2ab41c07"))
    return pg(title, *body, crumbs=[("Repositories", "g03-repositories.html"), (title, None)], acts=repo_head_acts() if acts is None else acts, meta=meta, status=st,
              sections=repo_sections(cur, held))


# ---------------------------------------------------------------- G01
def g01_login(o):
    card = sec("", '<div class="row">' + ic("swap", "lg") + "<h1>RepoSync</h1></div>",
               '<p class="t2">Sign in to the sync dashboard</p>',
               banner("", "Corporate login enabled (corp.example)", "Use your network username and password.", icon="key"),
               form(field("Username", "ines", ph="Username"), field("Password", "", "password", ph="Network password"), cls="one"),
               row(btn("Sign in", "primary")))
    return "Sign in", f'<div class="login-wrap"><div class="login-card">{card}</div></div>'


def g01_states(o):
    f = lambda **k: form(field("Username", "ines"), field("Password", "", "password", **k), cls="one")
    return "Sign in: states", pg("Sign in: states", board(
        ("Signing in", sec("", f(), row(btn("Signing in…", "primary", disabled=True)), '<p class="t2 sm" role="status">Checking your details…</p>')),
        ("Invalid credentials", sec("", banner("err", "Sign-in failed", "Invalid username or password.", role="alert"),
                                    f(help="Your password was cleared. Your username was kept."), row(btn("Sign in", "primary")))),
        ("Sign-in help unavailable", sec("", f(), row(btn("Sign in", "primary")),
                                         note("Today a failed read of the sign-in options is ignored and the directory hint is simply absent (Login.tsx:17-26). The form still works."))),
        ("Session ended", sec("", banner("warn", "Your session ended", "Sign in again to continue. A change you had just submitted may still have been applied; check it after signing in.", role="status"),
                              f(), row(btn("Sign in", "primary")), row(needs("Needs the reason carried to the sign-in page")))),
        ("Already signed in", sec("", empty("You are signed in", "Opening the dashboard…", icon="check"),
                                  note("Login.tsx redirects a signed-in visitor to the dashboard."))),
        ("Server not set up yet", sec("", banner("held", "Initial setup required", "This server has no administrator yet. Complete server setup first.", role="alert"),
                                      row(btn("Open server setup", href="g12-setup-1.html")), row(needs("The page does not handle this refusal specifically today")))),
    ))


# ---------------------------------------------------------------- G02
def attention():
    a = lambda kind, st, name, text, act: banner(kind, f"{st} <b>{name}</b>", text, acts=act)
    return ('<div class="attn-list stack">'
            + a("held", pill("held"), "loom-patterns", "Full history import stopped by Ines Okafor at 13:52, at r768 of r1871. Written locally through r768; confirmed on the Git remote through r750. Published commits were not undone.", btn("Open operation", "primary sm", href="g04-operations.html"))
            + a("warn", pill("paused"), "greenhouse-telemetry", "Sync paused itself after repeated permanent errors. 2 Git commits are waiting. Last successful sync 11:22:03.", btn("Review pending commits", "sm", href="g04-skip.html"))
            + a("warn", pill("conflict"), "kiln-controller / display-v2", "1 unresolved conflict: " + mono("firmware/display/layout.c") + ".", btn("Open conflict", "sm", href="g06-conflict-diff.html"))
            + "</div>")


def repos_block(o, rows=REPOS):
    out = []
    for name, s, g, st, obs, last, rev, sha, kids in rows:
        out.append([f"<b>{link(name, 'g04-overview.html')}</b>", pill(st), f"{svn(s)} {ic('swap')} {git(g)}", f'<span class="t3 sm">{obs}</span>',
                    last, pos(rev, sha) if sha else svn(rev) + ' <span class="t3">· no Git commit confirmed</span>'])
        for k in kids:
            out.append([f'<td class="ind">{link(k[0], "g04-overview.html")}</td>', pill(k[3]), f"{svn(k[1])} {ic('swap')} {git(k[2])}",
                        f'<span class="t3 sm">{obs}</span>', k[4], pos(k[5], k[6])])
    return table(["Repository / branch pair", "State", "SVN ⇄ Git", "Observed", "Last successful sync", "Position"], out,
                 cap="Repositories and branch pairs")


ACTIVITY = ('<ul class="actlist">'
            + "".join(f"<li>{ic(i)}<span>{t}</span><span class='t3 sm'>{d}</span><time>{tm}</time></li>" for i, t, d, tm in [
                ("check", "Sync cycle", "kiln-controller · SVN → Git · r4127", "14:06:12"),
                ("x", "Sync refused (failed)", "greenhouse-telemetry · Git → SVN", "13:57:44"),
                ("check", "Import stop requested", "loom-patterns · by ines", "13:52:09"),
                ("conflict", "Conflict detected", "kiln-controller / display-v2", "13:41:07")]) + "</ul>")
SERVER = stack(bar("Disk", 61, "61% · 122 GB of 200 GB"), bar("Memory", 38, "38%"), bar("CPU load (1m)", 22, "0.88"),
               kv(("Git push", "none active"), ("SVN", "1 active"), ("Network", "↑ 1.2 MB/s · ↓ 310 KB/s")))
RECORDS = table(["", "Direction", "Message", "Author", "SVN", "Git", "Synced"], [
    [ic("check") + '<span class="sr">Applied</span>', chip("SVN → Git"), "Tune ramp-rate limits", "mei", svn("r4127"), git("9f3c2ab"), "14:06:12"],
    [ic("check") + '<span class="sr">Applied</span>', chip("Git → SVN"), "Fix thermocouple offset", "arjun", svn("r4126"), git("51d0e7a"), "13:40:02"],
    [ic("x") + '<span class="sr">Failed</span>', chip("Git → SVN"), "Recalibrate bay 2 sensors", "arjun", "none", git("a91f3e2"), "13:57:44"]],
    cap="Recent sync records", cls="dense")


RECORDS_ALL = table(["", "Repository", "Direction", "Message", "Author", "SVN", "Git", "Synced"], [
    [ic("check") + '<span class="sr">Applied</span>', "kiln-controller", chip("SVN → Git"), "Tune ramp-rate limits", "mei", svn("r4127"), git("9f3c2ab"), "14:06:12"],
    [ic("check") + '<span class="sr">Applied</span>', "kiln-controller", chip("Git → SVN"), "Fix thermocouple offset", "arjun", svn("r4126"), git("51d0e7a"), "13:40:02"],
    [ic("x") + '<span class="sr">Failed</span>', "greenhouse-telemetry", chip("Git → SVN"), "Recalibrate bay 2 sensors", "arjun", "none", git("a91f3e2"), "13:57:44"]],
    cap="Recent sync records, all repositories")


def commit_map(epic_p=True, repo=False):
    if repo:
        return table(["Repository", "SVN rev", "Git SHA", "Direction", "SVN author", "Git author", "Synced at"], [
            ["kiln-controller", svn("r4127"), git("9f3c2ab41c07"), chip("SVN → Git"), "mei", "Mei-Ling Zhou", "14:06:12"],
            ["kiln-controller", svn("r4126"), git("51d0e7a2b9c4"), chip("Git → SVN"), "arjun", "Arjun Patel", "13:40:02"],
            ["kiln-controller", svn("r4124"), '<span class="t3"><i>No Git commit (filtered)</i></span>', chip("SVN → Git"), "mei", "none", "13:02:10"],
            ["kiln-controller", svn("r4121"), '<span class="t3"><i>No Git target (unverified)</i></span>', chip("SVN → Git"), "mei", "none", "12:44:51"]], cap="Commit map, all repositories")
    extra = [[svn("r4124"), '<span class="t3"><i>No Git commit (filtered)</i></span>', chip("SVN → Git"), "mei", "none", "13:02:10"],
             [svn("r4121"), '<span class="t3"><i>No Git target (unverified)</i></span>', chip("SVN → Git"), "mei", "none", "12:44:51"]] if epic_p else []
    return table(["SVN rev", "Git SHA", "Direction", "SVN author", "Git author", "Synced at"], [
        [svn("r4127"), git("9f3c2ab41c07"), chip("SVN → Git"), "mei", "Mei-Ling Zhou", "14:06:12"],
        [svn("r4126"), git("51d0e7a2b9c4"), chip("Git → SVN"), "arjun", "Arjun Patel", "13:40:02"]] + extra, cap="Commit map")


def fleet(single=False):
    """The health region, where it sits today: after the repositories."""
    if single:
        return sec("Sync health and position", '<div class="fleet">' + pill("idle", big=True) + stamp("observed 14:08:31 · 5s ago") + "</div>",
                   kv(("Last SVN revision", svn("r231")), ("Last Git hash", git("3d90e71a44c2")), ("Last sync", "Today 09:14:50"),
                      ("Total syncs", "412"), ("Active conflicts", "0"), ("Errors (24h)", "0"), ("Uptime", "Unavailable")),
                   '<p class="t3 sm">Stored state. A status read does not re-check the remotes.</p>')
    return sec("Sync health", kv(("Total syncs", "1,902"), ("Active conflicts", link("1", "g05-conflicts.html")),
                                 ("Errors (24h)", link("3", "g07-audit.html") + " " + btn("Acknowledge…", "sm")), ("Uptime", "Unavailable"),
                                 ("Last sync", "See each repository")), note=stamp("Aggregate observed 14:08:31 · 8s ago"))


def needs_you():
    """New, read-only and derived from the same status reads. It makes no request and holds no control that acts."""
    counts = " ".join([pill("idle", "Idle 1"), pill("running", "Running 1"), pill("held", "Held 1"), pill("paused", "Paused after error 1"),
                       pill("conflict", "Conflict 1"), pill("disabled", "Disabled 1")])
    return sec("", '<div class="fleet"><h2>3 pairs need attention</h2>' + stamp("Aggregate observed 14:08:31 · 8s ago") + sp()
               + chip("6 of 6 pairs answered") + "</div>", row(counts),
               '<p class="t3 sm">Counted from the status read this page already makes for each pair. A pair whose read fails or goes stale is counted as Unknown, and the count of pairs that answered drops.</p>',
               attention())


def g02_dashboard(o):
    scope = row("<h1>Dashboard</h1>", select("Repository scope", "All repositories", "kiln-controller", "loom-patterns"), sp(),
                link("Manage repositories", "g03-repositories.html"))
    inflight = sec("In flight", op("Write Git commit to SVN", pill("running"),
                                   f'<span class="t2 sm">kiln-controller / pid-tuning · source Git {git("77ab01c")}; SVN was at {svn("r4125")} before the write. Publication not yet confirmed.</span>',
                                   opid="b71e…09aa", meta=stamp("14:08:19")))
    lower = sec("Recent sync records", RECORDS_ALL) + sec("Commit map (SVN ⇄ Git)", commit_map(repo=True))
    side = sec("Recent activity", ACTIVITY, acts=link("Open audit log", "g07-audit.html")) + sec("Server", SERVER, note=stamp("12s ago"))
    dash = (f'<div class="dash"><div data-b="attn">{needs_you()}</div><div data-b="flight">{inflight}</div>'
            f'<div data-b="repos">{sec("Repositories", repos_block(o))}</div><div data-b="health">{fleet()}</div>'
            f'<div data-b="lower">{lower}</div><div data-b="side">{side}</div></div>')
    return "Dashboard", f'<div class="pg">{scope}{dash}</div>'


def g02_single(o):
    scope = row("<h1>Dashboard</h1>", select("Repository scope", "recipes-site"), sp(), link("Manage repositories", "g03-repositories.html"))
    one = [("recipes-site", "trunk", "main", "idle", "5s ago", "09:14:50", "r231", "3d90e71", [])]
    side = sec("Recent activity", ACTIVITY) + sec("Server", SERVER)
    dash = (f'<div class="dash"><div data-b="attn">{sec("Needs attention", empty("Nothing needs you", "No held, paused, failed or conflicting work is recorded.", icon="check"))}</div>'
            f'<div data-b="repos">{sec("Repository", repos_block(o, one), acts=btn("Request sync", "sm", icon="run"))}</div><div data-b="health">{fleet(True)}</div>'
            f'<div data-b="lower">{sec("Recent sync records", RECORDS)}{sec("Commit map (SVN ⇄ Git)", commit_map())}</div><div data-b="side">{side}</div></div>')
    return "Dashboard: one repository", f'<div class="pg">{scope}{dash}</div>'


def g02_states(o):
    ack = dialog("Acknowledge these errors?", "All repositories (global scope)",
                 conseq(("check", "Marks the 3 displayed recent errors as seen."), ("check", "Original audit evidence will be preserved."),
                        ("info", "New errors will remain visible. Acknowledging does not fix anything or release a hold.")),
                 foot=btn("Cancel", "ghost focus-demo") + btn("Acknowledge 3 errors", "primary"))
    return "Dashboard: states", pg("Dashboard: states", board(
        ("Loading", sec("Repositories", loading("Loading repositories…"))),
        ("No repositories, administrator", sec("Repositories", empty("No repositories yet", "Add an SVN repository and the Git repository it should stay in step with.", btn("Add repository", "primary", icon="plus")))),
        ("No repositories, not an administrator", sec("Repositories", empty("No repositories yet", "Adding one needs an administrator. You are signed in as <b>tomas</b> (user)."))),
        ("Status unavailable", sec("", '<div class="fleet">' + pill("unknown", big=True) + stamp("Unavailable · read failed (502) at 14:08:31", "bad") + "</div>",
                                   '<p class="t2">Health is unknown. This does not authorize Retry, Skip or hold release.</p>', row(btn("Retry read", "sm")))),
        ("Status stale", sec("", '<div class="fleet dim">' + pill("unknown", big=True) + '<span class="t3 sm">last known: Idle</span></div>',
                             stamp("Stale · last observed 14:05:10 · 3m 21s ago", "stale"), '<p class="t2">Last-known data is retained below; it is not a current healthy observation.</p>')),
        ("Recent activity failed", sec("Recent activity", empty("Recent activity could not be loaded", "Results are unknown.", btn("Retry", "sm"), role="alert", icon="error"))),
        ("Sync records failed", sec("Recent sync records", empty("Sync records could not be loaded", "Results are unknown. This is not the same as none.", btn("Retry", "sm"), role="alert", icon="error"),
                                    row(needs("Today this renders as “No sync records yet”")))),
        ("Import in progress", op("Full history import · loom-patterns", pill("running"), meter(41, "SVN import progress — loom-patterns"),
                                  '<span class="t2 sm">41% of reported revisions · r768 of r1871</span>', opid="2b9d…1a44",
                                  foot=link("Open operation", "g04-operations.html"))),
        ("Server monitor unavailable", sec("Server", empty("Server metrics are unavailable", "The read failed. No value is shown as zero.", btn("Retry", "sm"), role="alert", icon="error"))),
    ), sec("Error acknowledgement", ack, note("Replaces the browser prompt used today (ErrorAcknowledgement.tsx:24); the wording is the existing prompt.")))


# ---------------------------------------------------------------- G03
def g03_list(o):
    recv = banner("ok", "Removal completed; verified cleanup recorded", "woodshop-archive is no longer listed. Both remotes are untouched.",
                  acts=link("View removal status and recovery", "g04-removal-recovery.html"), role="status")
    head = row("<h1>Repositories</h1>", chip("4"), sp(), btn("Add repository", "primary", icon="plus", href="g03-add-repository.html"))
    return "Repositories", f'<div class="pg">{recv}{head}{sec("", repos_block(o))}</div>'


def add_form():
    return stack(
        sec("Repository", form(field("Repository name", "bandsaw-jigs", required=True))),
        sec("SVN source", form(field("SVN URL", "https://svn.makerspace.example/bandsaw-jigs", cls="mono"), field("Branch", "trunk", cls="mono"),
                               field("Username", "sync-service"), field("Password", "••••••••", "password", help="Kept in memory until saved. Never stored in this browser.")),
            row(btn("Test SVN connection", "sm"), '<span class="okl" role="status">' + ic("check") + "<span>Repository Root: " + mono("https://svn.makerspace.example/bandsaw-jigs") + " · tested 14:01:40</span></span>")),
        sec("Git target", form(field("Provider", ["GitHub", "Gitea"], "select"), field("API URL", "https://api.github.com", cls="mono"),
                               field("Repository", "makerspace/bandsaw-jigs", cls="mono", ph="owner/repo"), field("Default branch", "main", cls="mono"),
                               field("Git token", "••••••••", "password")),
            row(btn("Test Git connection", "sm"), '<span class="err" role="alert">' + ic("error") + "<span>Authentication required (401 Unauthorized). The API URL is reachable but the repo may be private. · tested 14:02:05</span></span>")),
        sec("Initial SVN import", '<p class="t2 sm">Choose how the first Git baseline is built from SVN. Full history remains the default.</p>',
            stack(choice("Full SVN history", "Replay every SVN revision into Git (recommended default).", on=True, name="imp"),
                  choice("Snapshot at pinned SVN revision", "Pin SVN HEAD or a selected revision once; earlier history is intentionally omitted.", name="imp",
                         extra=form(field("SVN revision (HEAD or number)", "HEAD", cls="mono"))))),
        sec("Sync settings", form(field("Sync mode", ["Direct", "Pull request"], "select"), field("Poll interval (seconds)", "300", "number"),
                                  field("LFS threshold (MB, 0 = disabled)", "0", "number")),
            row(check("Auto merge", kind="switch"), check("Enabled", on=True, kind="switch"))))


def g03_add(o):
    head = ""
    body = dialog("Add repository", "Configure the registered SVN and Git endpoints and import policy. Review saved and pending stages before retrying.",
                  head, add_form(), foot=btn("Cancel", "ghost focus-demo") + btn("Create repository", "primary", icon="plus"), wide=True)
    return "Add repository", pg("Repositories", body, acts=btn("Add repository", "primary", icon="plus", disabled=True))


def save_notice(config, cred, *extra, repo="bandsaw-jigs"):
    return banner("warn", f"Recorded configuration stage: {config}. Credential stage: {cred}.",
                  f'<span>Existing repository: {mono(repo)}. This ID is retained; no new registration is silently created.</span>', *extra)


def g03_save(o):
    return "Add repository: save and recovery", pg("Save and recovery", board(
        ("Saving", banner("run", "Saving in order: configuration, then credentials", "Neither stage is reported complete before confirmation.", role="status")),
        ("Configuration saved, credentials unknown", save_notice("saved", "unknown",
            "Resume confirms the earlier submitted snapshot; newer edits are not included until a new save. Closing or navigating away keeps this recovery record; it does not undo a saved configuration.",
            "Required credential fields: svn_password, git_token. Secrets are kept only in memory; after reopening, re-enter them only if the previous save is not confirmed.",
            row(btn("Resume / Save on Existing ID", "primary"), btn("Close", "ghost")))),
        ("Outcome unknown (timeout)", save_notice("unknown", "pending",
            "The save response timed out; its server outcome is unknown. Resume this request instead of creating another.",
            row(btn("Resume Same Save Request", "primary")))),
        ("Conflict with current settings", save_notice("conflict", "pending",
            "Resume cannot bypass this conflict. Review current settings on the retained ID before a deliberate new save. Earlier receipts and unknown outcomes remain recorded; no rollback is assumed. Re-enter required credentials only if you intend a new credential write.",
            row(btn("Load current settings for review"), btn("Use these reviewed settings for a new save on this ID", disabled=True)))),
        ("Server did not confirm durably", save_notice("failed", "pending",
            "This server did not provide the required durable save confirmation. No write will be blindly retried. Review the existing repository ID and use a matching API version before a new save.")),
        ("Saved", banner("ok", "Repository created", "Configuration and credentials are both confirmed. Opening the repository…", role="status")),
        ("Signed in as someone else", banner("err", "The login session changed", "The save is retained for its original user. Reopen this form to resume as the current user.", role="alert")),
        ("Closed with unsaved edits", banner("", "Draft kept in this browser", "Your entries were kept, without passwords or tokens. Reopen Add repository to continue.")),
    ), note("The stage line, the retained-ID sentence, the sentences inside the four save notices and their button labels are the existing wording (RepositorySaveNotice.tsx, repositorySave.ts, Repositories.tsx). "
          "The board titles, and the first and last three boards, are this design's summary of states the code has."))


def g03_states(o):
    return "Repositories: states", pg("Repositories: states", board(
        ("Loading", sec("", loading("Loading repositories…"))),
        ("Read failed", sec("", empty("Repositories could not be loaded", "API error 502. Results are unknown.", btn("Retry", "sm"), role="alert", icon="error"))),
        ("Empty, administrator", sec("", empty("No repositories configured", "Add your first SVN ⇄ Git sync.", btn("Add repository", "primary", icon="plus")))),
        ("Empty, not an administrator", sec("", empty("No repositories configured", "Adding one needs an administrator."))),
        ("Branch pair removed", banner("ok", "Branch pair removed", "kiln-controller / heater-map", role="status")),
        ("Branch pair removal not finished", banner("warn", "Removal is not complete", "The removal request was accepted and has not finished.",
                                                    mono("Operation 91c0…5e2a"), role="status")),
        ("Removal with cleanup warnings", banner("warn", "Branch pair removed with cleanup warnings", "Local path could not be fully cleaned: 2 files remain.", role="status")),
        ("Managed removal receipt", banner("held", "Removal failed; evidence retained", "Partial cleanup receipt · Remote Git: untouched · SVN: untouched",
                                           mono("Operation 4e07…c3d1"), acts=link("View removal status and recovery", "g04-removal-recovery.html"), role="status")),
    ))


# ---------------------------------------------------------------- G04
def g04_overview(o):
    pairs = table(["Pair", "SVN ⇄ Git", "State", "Last successful sync"], [
        [link("pid-tuning"), f"{svn('branches/pid-tuning')} {ic('swap')} {git('pid-tuning')}", pill("running"), "13:58:40"],
        [link("display-v2"), f"{svn('branches/display-v2')} {ic('swap')} {git('display-v2')}", pill("conflict"), "13:41:07"]], cap="Branch pairs")
    ops = stack(row(ic("check"), "Full history import", chip("Completed"), '<span class="t3 sm">4,127 revisions · tree verification matched · 3 Nov</span>'),
                '<p class="t3 sm">No operation is queued, running or held for this repository.</p>')
    errs = stack('<p class="t2 sm">No unacknowledged errors in the last 24 hours. Acknowledging marks errors as seen; it does not fix them or remove audit entries.</p>')
    body = cols((5, sec("What this status proves", EVIDENCE) + sec("Errors", errs, acts=btn("Acknowledge…", "sm", disabled=True))
                 + sec("Operations", ops, acts=link("All operations", "g04-operations.html"))),
                (7, sec("Recent sync records", RECORDS, acts=link("Activity", "g04-activity.html"))
                 + sec("Branch pairs", pairs, acts=btn("Add branch pair", "sm", icon="plus", href="g04-pairing.html"))))
    body += sec("Server", '<details><summary>Show the server monitor</summary>' + SERVER + '</details>')
    body += sec("This evidence in other states", board(("Held", evidence("held", True)), ("Paused after error", evidence("paused", True)), ("Unknown or unavailable", evidence("unknown", True))),
                note=needs("The sentence pattern is the page's own; the state words are this design's"))
    return "Repository: overview", repo_page("overview", body)


def g04_activity(o):
    audit = table(["Outcome", "Action", "Author", "Details", "Time"], [
        [ic("check") + " Succeeded", "Sync cycle " + chip("×6"), "system", "1 revision applied " + link("Show group"), "14:06:12"],
        [ic("x") + " Failed", "Sync refused", "system", link("Show details"), "13:57:44"]], cap="Recent audit log")
    two = board(("epic/reposync-partial-52: explicit no-target rows", commit_map(True)),
                ("epic/reposync-reliability-61: Git SHA only", commit_map(False)), one=True)
    return "Repository: activity", repo_page("activity", sec("Sync records", RECORDS, note='<span class="t3 sm">Recent commits synced for this repository</span>'),
                                             sec("Commit map (SVN ⇄ Git)", two), sec("Recent audit log", audit, acts=link("Open audit log", "g07-audit.html")))


def g04_branches(o):
    t = table(["Pair", "SVN branch", "Git branch", "Configured", "State", "Last successful sync", "Actions"], [
        [f"<b>{link('pid-tuning')}</b>", svn("branches/pid-tuning"), git("pid-tuning"), chip("Enabled"), pill("running"), "13:58:40", btn("Actions", "sm ghost", icon="down")],
        [f"<b>{link('display-v2')}</b>", svn("branches/display-v2"), git("display-v2"), chip("Enabled"), pill("conflict"), "13:41:07",
         btn("Actions", "sm ghost focus-demo", icon="down") + menu(("Open pair", ""), ("Preview update from parent", ""), ("Remove pair…", "danger"))]],
        cap="Branch pairs of kiln-controller")
    held = (sec("Update pair from parent · preview only", '<p class="t2 sm">A read-only preview of what updating a pair from its parent would do. Nothing is changed.</p>',
                acts=link("Open the preview", "g04-refresh-preview.html"))
            + sec("Re-anchor", banner("held", "Held", "Re-anchoring a pair is not available from this interface.")))
    return "Repository: branch pairs", repo_page("branches", held, sec("Branch pairs", '<p class="t3 sm">SVN branches synced to separate Git branches</p>', t,
                                                                 acts=btn("Add branch pair", "primary sm", icon="plus", href="g04-pairing.html")),
                                                 note("Row actions are the existing controls in a labelled menu. The two parts above the table come first because that is their order in the page today, and every layout keeps that order."))


def g04_pairing(o):
    plan = stack(conseq(("check", f"<b>Admitted.</b> Git branch {git('heater-map')} descends from a verified SVN import."),
                        ("check", f"<b>Baseline.</b> Git {git('9f3c2ab')} equals SVN {svn('r4127')} (commit map)."),
                        ("info", f"<b>SVN branch.</b> {svn('branches/heater-map')} does not exist. It will be copied from {svn('trunk@r4127')}."),
                        ("info", f"<b>Git work to carry over.</b> 3 commits since the baseline, tip {git('1d07e55')}."),
                        ("check", "<b>Conflicts:</b> none found. <b>Unknowns:</b> none.")),
                 check("Include parent SVN advancement in this reviewed result"),
                 evid(mono("review pin a7f3c0de…91b2 · intended Git result 4be2a90 · handled steps 0/3 · policy v3"),
                      "If either side moves before you publish, the server refuses and you review again.", title="Plan identity"))
    frm = form(field("Branch name", "heater-map", cls="mono", help="10/200 · letters, numbers, . _ - / only"),
               field("SVN branch path", "branches/heater-map", cls="mono", help="Auto-derived, editable"))
    mode = stack('<span class="lbl">Pairing mode</span>', choice("Reconcile from the verified SVN baseline", on=True, name="pm"),
                 choice("Start from now (unsafe — refused)", name="pm"))
    d = dialog("Review branch pair", "Capture both sides, inspect the intended result, then explicitly publish or recover the pinned operation.",
               '<p class="t2 sm">Branching from ' + svn("trunk") + " → " + git("main") + ". The Git branch must already exist.</p>", frm, mode,
               sec("Plan · preview only; no repository changes", plan),
               foot=btn("Close", "ghost focus-demo") + btn("Preview pair") + btn("Publish reviewed result", "primary"), wide=True)
    states = board(
        ("Validation", form(field("Branch name", "fix..123", cls="mono", err='Must not contain ".."'), field("SVN branch path", "branches/fix 123", cls="mono", err="Invalid characters"))),
        ("Refused", banner("err", "Pairing refused", "This Git branch does not descend from an SVN import (" + mono("unrelated_git_first") + "). Nothing was created.", role="alert")),
        ("Blocked by conflicts", banner("warn", "Plan blocked", "Overlapping text change in " + mono("firmware/heater/map.c") + ". A blocked preview has no publishable result and creates nothing.")),
        ("Published, in progress", op("Add branch pair heater-map", tone("run", "run", "Applying"), '<span class="t2 sm">handled steps 1/3 · SVN branch copied at r4127 · the pair stays out of scheduled sync until verified</span>',
                                      opid="c2aa…7f10", foot=btn("Inspect operation", "sm") + btn("Reconcile / resume", "sm") + btn("Stop operation…", "sm warn")
                                      + '<span class="t3 sm">Cancellation after intent retains the hold; it does not undo remote work.</span>')),
        ("Held", op("Add branch pair heater-map", pill("held"), '<span class="t2 sm">A write may have happened and could not be proved. Only the administrator who started this can resume or stop it.</span>', opid="c2aa…7f10")),
        ("Completed", banner("ok", "Pair publication completed", f"Git tip {git('4be2a90')}, baseline {svn('r4127')}, 0 pending Git commits. Scheduler active.", role="status")),
    )
    return "Repository: add branch pair", repo_page("branches", d, sec("States", states))


def g04_refresh(o):
    prev = sec("Update pair from parent · preview only", '<p class="t2 sm">Read-only preview for <b>display-v2</b>. Published Git commits and SVN revisions stay. Unsynced work on either side is reported and is not discarded.</p>',
               kv(("Plan", mono("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")),
                  ("Git", mono("display-v2 c41d9e0 · parent main 9f3c2ab")), ("SVN", mono("branches/display-v2 r4119 · parent trunk r4127")),
                  ("Pending", "Git: pair 1, parent 0 · SVN: pair 0, parent 3"), ("Conflicts", "both_advanced · resolution is not offered here"),
                  ("Result", "Not executed. Discards unsynced work: no.")),
               acts=btn("Preview update from parent", "sm", icon="run"))
    held = sec("Re-anchor", banner("held", "Held: not available from this screen",
                                   "Re-anchor or recreate is a separate, explicitly approved mode. It does not reset, force-push, or delete the old SVN path.",
                                   "This design adds no execute or re-anchor control. The panel's wording and the server's reported status are for the pair-refresh owner (#69) to reconcile.", icon="held"),
               row(btn("Re-anchor pair", disabled=True), needs("Held under #69")))
    states = board(("Preview failed", banner("warn", "Preview unavailable", "not_a_branch_pair: a root repository has no parent to update from.", role="alert")),
                   ("Rewritten lineage", banner("warn", "Pair lineage is rewritten", "Those commits are not counted as new work and are not discarded.")))
    return "Repository: update preview", repo_page("branches", prev, held, sec("States", states))


def import_card(state_html, *body, foot="", pct=None, measured=True):
    m = meter(pct, "SVN import progress — loom-patterns") if measured else meter(None)
    return op("Full history import", state_html, m, *body, opid="7c1e4a52…f730", foot=foot, icon="svn")


def g04_operations(o):
    held_status = statline("held", "7s ago", "Not recorded", pos("r750", "8a02f5d"), "1 held operation", label="Held for reconciliation")
    running = import_card(pill("held", "Held for reconciliation"), '<ol class="phases"><li class="done">Connect</li><li class="now">Import</li><li>Verify</li><li>Final push</li><li>Complete</li></ol>',
                          row("<b>41%</b>", '<span class="t2 sm">of reported revisions when it stopped · r768 of r1871 · started 13:31:04 by Ines Okafor · stop requested 13:52:09 by Ines Okafor</span>'),
                          kv(("Written locally", "through SVN " + svn("r768") + " · Git " + git("e41b0c7")), ("Confirmed on the Git remote", "through SVN " + svn("r750") + " · Git " + git("8a02f5d")),
                             ("Commits / batches / LFS", "766 · 15 · 12")),
                          log("[14:08:29] [info] r768 12 files", "[14:08:31] [push] batch 15 pushed (50 commits)"),
                          foot=btn("Verify remote", "sm") + '<span class="t3 sm">Stopping does not undo commits already published.</span>', pct=41)
    b = board(
        ("Never run · can start", sec("SVN import", '<p class="t3 sm">No import history</p>', stack(choice("Full SVN history", on=True, name="s"), choice("Snapshot at pinned SVN revision", name="s")),
                                      row(btn("Start full history import", "primary sm")))),
        ("Queued", import_card(tone("unk", "unknown", "Queued"), '<span class="t2 sm">Accepted. No worker has started. Nothing has been written.</span>', foot=btn("Stop import…", "warn sm"), measured=False)),
        ("Connecting", import_card(pill("running"), '<span class="t2 sm">Connecting. Progress not yet measurable.</span>', foot=btn("Stop import…", "warn sm"), measured=False)),
        ("Stop requested", import_card(tone("run", "busy", "Stop requested"), '<span class="t2 sm">Cancellation requested — stopping. Work already published stays published.</span>', pct=41)),
        ("Stopped · held", import_card(pill("held", "Stopped · held"), '<span class="t2 sm">Stopping does not undo commits already published. This repository stays held until its local and remote history is reconciled.</span>',
                                       '<span class="st-held sm">' + ic("info") + " No in-product step continues or restarts a stopped import today.</span>", row(needs("Recovery route is the import owner's decision")), pct=41)),
        ("Needs reconciliation", import_card(pill("held", "Needs reconciliation"), kv(("Local Git", mono("e41b0c7")), ("Confirmed Git", mono("8a02f5d")), ("Publication intent", mono("refs/heads/main → e41b0c7"))),
                                             foot=btn("Verify remote", "primary sm") + '<span class="t3 sm">Read-only check. Administrators only.</span>', pct=41)),
        ("Verified · resume available", import_card(pill("held", "Held"), '<span class="t2 sm">Publication verified; partial import remains held; resume from the confirmed checkpoint when ready.</span>',
                                                    foot='<span class="t3 sm">Resuming is not available from this interface.</span>' + needs("A server route exists; nothing calls it. No control is drawn"), pct=40)),
        ("Failed · held", import_card(pill("error", "Failed · held"), '<span class="st-err mono sm">svn: E175002: connection reset during REPORT</span>', '<span class="t3 sm">Local through r911; remote confirmed through r900.</span>', pct=48)),
        ("Completed", import_card(tone("ok", "idle", "Completed"), '<span class="t2 sm">1,871 revisions · 1,866 commits · 37 batches · finished after 1h 10m</span>', '<span class="okl">' + ic("check") + "Tree verification matched</span>", pct=100)),
        ("Completed, verification differs", import_card(tone("warn", "conflict", "Completed · verification differs"), '<span class="t2 sm">2 mismatched files, 1 missing in Git.</span>', foot=link("Show the 3 files"), pct=100)),
        ("Snapshot baseline (another repository, imported as a snapshot)", banner("warn", "Snapshot at pinned SVN revision " + mono("r1840"), "SVN history before r1840 was not imported; later revisions remain pending.", icon="info")),
        ("Status read stale", '<div class="dim">' + import_card(pill("unknown"), '<span class="t2 sm">Last known: Running, r768 of r1871.</span>', pct=41) + "</div>"
         + row(stamp("Stale · last read 14:05:10 · 3m 21s ago", "stale"), btn("Retry read", "sm"))),
    )
    return "Repository: operations", repo_page("operations", sec("Current operation", running), sec("Every import state", b), held=True, status=held_status,
                                               title="loom-patterns", meta=chip("Sync configured enabled") + chip("Full history import"),
                                               acts=btn("Request sync", icon="run", disabled=True) + btn("Pause sync…", "warn", icon="off", disabled=True))


def settings_read():
    return (cols((6, sec("SVN source", kv(("Name", "kiln-controller"), ("URL", mono("https://svn.makerspace.example/kiln-controller")), ("Branch", mono("trunk")), ("Username", "sync-service")),
                         acts=btn("Edit", "sm", href="g04-settings-edit.html"))),
                 (6, sec("Git target", kv(("Provider", "GitHub"), ("API URL", mono("https://api.github.com")), ("Repository", mono("makerspace/kiln-controller")), ("Branch", mono("main"))),
                         acts=btn("Edit", "sm", href="g04-settings-edit.html"))))
            + sec("Credentials", '<p class="t3 sm">Used by RepoSync to read and write this repository. Never shown after saving.</p>',
                  '<ul class="credlist" aria-label="Credentials">'
                  + f'<li><span class="who">SVN password</span><span>{ic("check")} Set</span>{needs("Origin is not reported today")}{btn("Replace…", "sm")}</li>'
                  + f'<li><span class="who">Git token</span><span>{ic("minus")} Not set</span>{needs("Absent and revoked look the same today")}{btn("Set…", "sm")}</li></ul>')
            + cols((6, sec("Sync policy", kv(("Sync mode", "Direct"), ("Poll interval", "300 s"), ("LFS threshold", "0 MB (disabled)"), ("Auto merge", "No")), acts=btn("Edit", "sm"))),
                   (6, sec("Path policy", kv(("Allowed paths", mono("source/ · config/")), ("Blocked patterns", mono("*.exe · temp/"))),
                           acts=btn("Download Git pre-commit hook", "sm") + btn("Edit", "sm", href="g04-policy.html"))))
            + sec("About", kv(("Created", "3 Nov 2026 by ines"), ("Last updated", "2h ago"))))


LADDER = stack(
    row(stack("<b>Pause sync</b>", '<span class="t2 sm">Stops new sync work. Registration, mappings, secrets, local files and both remotes stay. You can turn it back on.</span>', cls="tight sp"), btn("Pause sync…", "warn", icon="off", href="g04-pause.html")),
    row(stack("<b>Remove from RepoSync</b>", '<span class="t2 sm">Stop work, clean only owned local data, and drop the registration from active views. Remotes and history stay unless you separately authorize branch deletion.</span>', cls="tight sp"),
        btn("Remove from RepoSync…", "danger", icon="trash", href="g04-remove-root.html")),
    row(stack("<b>Reset and reimport</b>", '<span class="t2 sm">Unavailable during safe import cancellation. Request a reviewed recovery plan for an existing repository.</span>', cls="tight sp"), chip("Unavailable")))


def g04_settings(o):
    return "Repository: settings", repo_page("settings", settings_read(), sec("Stop or remove", LADDER))


TEST_RESULT = '<span class="t3 sm">The result appears beside the button as a message, as today.</span>'


def g04_settings_edit(o):
    edit = sec("SVN source · editing", form(field("Name", "kiln-controller"), field("SVN URL", "https://svn.makerspace.example/kiln-controller", cls="mono"),
                                            field("Branch", "trunk", cls="mono"), field("Username", "sync-service"),
                                            field("SVN password", "", "password", help="A password is saved. Leave empty to keep it.")),
               row(btn("Test SVN connection", "sm"), TEST_RESULT))
    edit_git = sec("Git target · editing", form(field("Provider", ["GitHub", "Gitea"], "select"), field("API URL", "https://api.github.com", cls="mono"),
                                               field("Repository", "makerspace/kiln-controller", cls="mono"), field("Default branch", "main", cls="mono"),
                                               field("Git token", "", "password", help="No token is saved.")),
                   row(btn("Test Git connection", "sm"), TEST_RESULT))
    save_row = row('<span class="t3 sm">The two connection tests are in this edit view only, as today.</span>', sp(), btn("Cancel", "ghost"), btn("Save", "primary"))
    b = board(
        ("Unsaved changes", banner("", "2 unsaved changes", "URL and username differ from the saved settings.", acts=btn("Discard", "ghost sm") + btn("Save", "primary sm"))),
        ("Invalid", form(field("SVN URL", "https://ines:hunter2@svn.makerspace.example/kiln", cls="mono wide", err="Use the dedicated credential fields instead of credentials embedded in a URL."))),
        ("Saving", banner("run", "Saving in order: configuration, then credentials", "Neither stage is reported complete before confirmation.", role="status")),
        ("Changed on the server since you opened it", banner("warn", "This save conflicts with current settings", "Load and review the current settings on this existing repository before deliberately starting a new save.",
                                                             acts=btn("Load current settings for review", "sm"))),
        ("Saved, later edits pending", banner("ok", "The earlier save completed", "Newer entered changes remain unsaved; save again after reviewing the refreshed settings.", role="status")),
        ("Held: settings cannot change", banner("held", "Settings are locked while an import is held", "Settings and credentials cannot be saved while an import is held; the save stops at its first stage.")),
    )
    return "Repository: edit settings", repo_page("settings", edit, edit_git, save_row, sec("Save states", b))


def g04_policy(o):
    lim = banner("warn", "Limitations in this build",
                 "These rules filter Git→SVN only. SVN→Git imports every path on the SVN branch in this build; SVN→Git path filtering (#53) is not included.",
                 "Paths are not rewritten: an included Git path is written to the same branch-relative SVN path. Rewriting is read-only and cannot be configured here.", icon="info")
    lst = lambda title, noun, vals, err=None: sec(title, stack(*[row(field(f"{noun} {i+1}", v, cls="mono", err=err if i == len(vals) - 1 else None), btn("Remove", "sm ghost")) for i, v in enumerate(vals)]),
                                                  row(btn(f"Add {noun.lower()}", "sm", icon="plus"), btn("Clear", "sm ghost"), btn("Undo", "sm ghost")))
    prev = sec("Git→SVN preview", '<p class="t3 sm">Uses the lists as they would be after saving and the same matching as the sync engine. Nothing is sent to the server.</p>',
               field("Sample Git paths (one per line)", "source/main.c\nbuild/output.exe", "textarea", cls="mono wide"),
               table(["Git path", "Result"], [[mono("source/main.c"), '<span class="st-ok">' + ic("check") + " Included</span>"],
                                              [mono("build/output.exe"), '<span class="st-warn">' + ic("minus") + " Excluded: matches a blocked pattern</span>"]], cap="Git→SVN preview results"))
    p52 = stack(lim, lst("Allowed paths", "Path", ["source/", "config/"]), lst("Blocked patterns", "Pattern", ["*.exe", "temp//"], err="Remove the empty path component ('//')."), prev)
    r61 = stack(form(field("Allowed SVN paths (one per line)", "source/\nconfig/", "textarea", cls="mono", help="Files must be under these path prefixes. Leave empty for no restriction."),
                     field("Blocked patterns (one per line)", "*.exe\ntemp/", "textarea", cls="mono", help="Files matching these patterns will be excluded from SVN sync.")))
    b = board(("Stored value is malformed", banner("err", "Allowed paths: stored value is not a list", "The stored value is not a JSON array of strings. The scheduler refuses to sync this repository until it is replaced or explicitly cleared.",
                                                   acts=btn("Replace", "sm") + btn("Clear", "sm danger"), role="alert")),
              ("Not reported by the server", banner("", "Path policy unavailable", "This server did not report the path policy, so it cannot be shown or edited here. Saving other settings leaves it unchanged.")),
              ("Wildcard in an allowed path", form(field("Path 1", "src/*", cls="mono wide", err="Allowed paths are literal prefixes; '*' is not a wildcard here. Use a blocked pattern instead."))))
    return "Repository: path policy", repo_page("settings",
        sec("Path policy on epic/reposync-partial-52 (#56 editor)", p52), sec("Path policy on epic/reposync-reliability-61 (free-text fields)", r61),
        sec("States (partial-52)", b), note("The two epics differ here. Each option styles whichever editor the receiving epic has; this design does not change the editor's behaviour."))


def g04_skip(o):
    st = statline("paused", "4s ago", "11:22:03 · 2h 46m ago", pos("r612", "0be77f1"), errors="3")
    bn = banner("warn", "Sync paused — permanent error", "The sync engine encountered repeated permanent errors and has been automatically paused. "
                "Skip selected pending commits using observed tips; this does not adopt live HEAD or skip unselected work.",
                mono("cursor 0be77f1a · remote 4c2e19d0 · bridge 4c2e19d0"),
                acts=btn("Skip commits…") + btn("Resume sync…") + '<span class="t3 sm">Neither action proves the two sides match, and neither releases a hold.</span>')
    d = dialog("Skip selected pending commits", "Only the selected observed pending prefix will be excluded. Unselected work stays pending.",
               '<span class="lbl">Skip through</span>', choice(mono("a91f3e2") + " Recalibrate bay 2 sensors", "Skips 1 commit. 1 commit stays pending.", name="sk"),
               choice(mono("4c2e19d") + " Bump sampling interval", "Skips 2 commits. Nothing stays pending.", name="sk"),
               evid("Observed remote tip: " + mono("4c2e19d0") + ". If the remote tip has moved, the server refuses and nothing is skipped."),
               foot=btn("Cancel", "ghost focus-demo") + '<span class="t3 sm">Nothing is selected yet.</span>' + btn("Skip selected", "warn", disabled=True))
    b = board(("After a choice", stack(choice(mono("a91f3e2") + " Recalibrate bay 2 sensors", "Skips 1 commit. 1 commit stays pending.", on=True, name="sk2"),
                                       row(btn("Cancel", "ghost"), btn("Skip selected", "warn")))),
              ("Refused: status changed", banner("err", "Status changed; no Skip request was sent", "Current scoped status does not permit Skip. Refresh and inspect the retained evidence.", role="alert")),
              ("Resume sync confirmation", dialog("Resume sync?", "greenhouse-telemetry", conseq(("check", "Clears the error pause so scheduled sync can run again."), ("info", "It does not skip or change the 2 pending commits. If the same error recurs, sync pauses again.")),
                                                  foot=btn("Cancel", "ghost") + btn("Resume sync", "primary"))))
    return "Repository: paused, skip and resume", repo_page("overview", bn, sec("What this status proves", evidence("paused")), d, sec("States", b), status=st, title="greenhouse-telemetry", meta=chip("Sync configured enabled"),
                                                            acts=btn("Request sync", icon="run", disabled=True) + btn("Pause sync…", "warn", icon="off"))


def g04_pause(o):
    d = dialog("Pause sync for kiln-controller?", "Disable syncing without removing data or changing remotes.",
               conseq(("check", "No new sync work starts for this repository."), ("check", "Registration, mappings, secrets, local files and both remotes stay."), ("info", "You can turn sync back on later.")),
               foot=btn("Keep syncing", "ghost focus-demo") + btn("Pause sync", "warn"))
    b = board(("Pausing", banner("run", "Disabling…", "Request in progress. Please wait before closing this dialog.", role="status")),
              ("Failed", banner("err", "Failed to disable", "repository has active or unresolved import work", role="alert")),
              ("Paused", row(chip("Sync configured disabled"), btn("Turn sync on", "sm"))),
              ("Turn sync on failed", banner("err", "Sync could not be turned on", "API error 409. The repository is still paused.", role="alert")))
    return "Repository: pause", repo_page("settings", d, sec("States", b), note("For a repository, “Pause sync…” uses the existing explicit disable call and its dialog. A branch pair has no explicit disable today, and none is added: "
          "it keeps the existing one-click switch, “Turn sync off” and “Turn sync on”, with its failure shown."))


def review_table(git_out, svn_out):
    return table(["Target", "Exact value", "What happens"], [
        ["Git branch", git("display-v2 @ c41d9e0"), git_out], ["SVN path", svn("branches/display-v2 @ r4119"), svn_out],
        ["Local copy", mono("…/kiln-controller--display-v2"), ic("trash") + " Removed"], ["Credentials", "Inherited key", ic("check") + " " + mono("never_deleted_by_managed_remove")]], cap="Removal review")


def g04_remove_root(o):
    d = dialog("Remove from RepoSync", "Remove the managed registration and owned local data. Remote history is preserved.",
               evid("Review for " + mono("woodshop-inventory"), "Git: preserved", "SVN: preserved", "Tombstone, audit, mapping and recovery evidence are retained.", title="Exact-target review"),
               '<p class="t2">Remove <b>woodshop-inventory</b> from active listings and clean only RepoSync-owned local data. Remote Git and SVN history are not modified by this action. Completed removals can be restored while recovery metadata remains.</p>',
               kv(("Child branch pairs", "none"), ("Credentials", mono("secret_svn_password_woodshop-inventory — removed with the registration")), ("Local path removed", mono("data/repos/woodshop-inventory"))),
               foot=btn("Refresh exact-target review", "ghost") + btn("Cancel", "ghost focus-demo") + btn("Remove from RepoSync", "danger solid"))
    b = board(("Review unavailable", banner("err", "Removal review unavailable", "API error 503. No new deletion is authorized.", acts=btn("Refresh exact-target review", "sm"), role="alert")),
              ("Blocked by child pairs", banner("warn", "This repository has branch pairs", "Remove or re-home kiln-controller / pid-tuning and display-v2 first. Children are never removed by a parent removal.")),
              ("Dependency preview unavailable", banner("err", "Dependency preview is unavailable", "Removal is blocked until current shared-credential, child-dependency and exact-target evidence are available.", acts=btn("Retry preview", "sm"), role="alert")),
              ("Earlier request unresolved", banner("warn", "A previously approved removal request has no retained outcome receipt", "Recover that same request before approving different work.", acts=btn("Recover retained removal request", "sm"), role="alert")))
    return "Repository: remove from RepoSync", repo_page("settings", d, sec("States", b))


def g04_remove_pair(o):
    opts = stack('<span class="lbl">Also delete on the remote (off by default)</span>', check("Delete Git branch " + git("(display-v2)")), check("Delete SVN branch " + svn("(branches/display-v2)")))
    d1 = dialog("Remove branch pair", "Review dependency and remote-deletion choices. Remote deletion is off by default.",
                review_table(ic("check") + " Left on remote", ic("check") + " Left on remote"), opts,
                field("Type display-v2 to confirm", "", cls="mono", ph="display-v2"),
                foot=btn("Cancel", "ghost focus-demo") + btn("Remove pair", "danger solid", icon="trash", disabled=True), wide=True)
    d2 = dialog("Remove branch pair and delete its Git branch", "Remote deletion was selected below.",
                review_table('<b class="st-err">' + ic("trash") + " Deleted on remote when authorized</b>", ic("check") + " Left on remote"),
                stack(check("Delete Git branch " + git("(display-v2)"), on=True), check("Delete SVN branch " + svn("(branches/display-v2)"))),
                banner("err", "RepoSync cannot bring a deleted Git branch back", "Restoring the registration does not recreate it."),
                field("Type display-v2 to confirm", "display-v2", cls="mono"),
                foot=btn("Cancel", "ghost") + btn("Remove pair and delete Git branch", "danger solid", icon="trash"), wide=True)
    return "Repository: remove branch pair", repo_page("branches", d1, d2,
        note("Typed confirmation and both unchecked boxes are the existing behaviour. The confirm label now states what the ticks will do; the request is unchanged."))


def g04_recovery(o):
    stages = table(["Stage", "Outcome"], [["Git", "Deletion verified"], ["SVN", "Not requested; preserved"], ["Owned local cleanup", "Started; completion not recorded"], ["Registration still listed", "yes"]], cap="Removal stages")
    b = board(
        ("Queued", op("Remove pair display-v2", tone("unk", "unknown", "Removal queued"), '<span class="t2 sm">Work still owns the registration.</span>', opid="91c0…5e2a", icon="trash")),
        ("Running", op("Remove pair display-v2", pill("running", "Removal running"), stages, '<span class="t3 sm">Retry only retries owned local cleanup; recorded remote outcomes are kept.</span>', opid="91c0…5e2a", icon="trash")),
        ("Failed", op("Remove pair display-v2", pill("error", "Removal failed; evidence retained"), stages, opid="91c0…5e2a", icon="trash", foot=btn("Retry owned local cleanup", "sm"))),
        ("Blocked by unresolved external effect", op("Remove pair display-v2", pill("held", "Held"), '<span class="t2 sm">Git: Held; effect or collection evidence is unresolved.</span>', opid="91c0…5e2a", icon="trash", foot=btn("Continue original reviewed operation", "sm"))),
        ("Evidence unavailable", banner("err", "Removal evidence unavailable", "Git: Unknown; do not infer deletion. SVN: Unknown; do not infer deletion. Verified completion evidence is unavailable.", role="alert")),
        ("Completed", banner("ok", "Removal completed; verified cleanup recorded", "Recovery metadata (restore available) · maps=412, tip r88 / 5aa0c2d", acts=btn("Restore registration (stays disabled)", "sm"), role="status")),
        ("Lost reply", banner("warn", "A removal you approved has no recorded outcome", "This browser never received the reply. Nothing was retried automatically.", acts=btn("Check the original request", "primary sm") + '<span class="t3 sm">Read-only lookup.</span>', role="alert")),
        ("Repository not found", sec("", empty("Repository not found", "This repository or branch pair is no longer available. It may have been removed.", btn("Back to repositories", "primary", href="g03-repositories.html")),
                                     '<p class="t3 sm">Administrators see the retained removal evidence here. Actions require a fresh operation-status read.</p>')),
        ("Restore failed", banner("err", "Restore failed", "API error 409: an old completed request cannot restore a newer registration.", role="alert")),
    )
    return "Repository: removal receipts and recovery", pg("Removal receipts and recovery", b, crumbs=CRUMB)


def g04_states(o):
    return "Repository: page states", pg("Repository: page states", board(
        ("Loading", sec("", loading("Loading repository…"))),
        ("Read failed", sec("", empty("Repository could not be loaded", "API error 500.", btn("Retry", "sm") + link("Back to repositories", "g03-repositories.html"), role="alert", icon="error"))),
        ("Held", sec("", statline("held", "7s ago", "Not recorded", pos("r750", "8a02f5d"), "1 held operation", label="Held for reconciliation"),
                     evid("Sync requests · a second import · settings changes · pause and removal · adding branch pairs · skip and resume.",
                          "Settings and credentials cannot be saved from the edit view: the save stops at its first stage.", title="Refused by the server while this hold is in place"))),
        ("Paused after error", sec("", statline("paused", "4s ago", "11:22:03", pos("r612", "0be77f1"), errors="3"))),
        ("Unknown · stale", sec("", statline("unknown", "14:05:10 · 3m 21s ago", "14:04:52 (last known)", pos("r2290", "e19f04b"), stale=True),
                                '<p class="t2 sm">Unknown does not authorize Retry, Skip or hold release.</p>')),
        ("Malformed status", sec("", empty("Malformed status response; health is unknown", "The reply could not be read as a status. No value from it is shown.", btn("Retry read", "sm"), role="alert", icon="error"))),
        ("Sync request recorded", banner("", "Sync request recorded at 14:08", "The next scheduled poll runs within 5 minutes (poll interval 300 s). RepoSync does not report when a requested cycle starts.", role="status")),
        ("Hook download failed", banner("err", "The hook could not be downloaded", "Authentication required. Sign in again and retry.", role="alert")),
    ), crumbs=CRUMB)
