"""Screens G05-G13 and the non-route surfaces. Synthetic data only."""
from kit import *

# ---------------------------------------------------------------- G05 / G06
def conflict_filters(on=0):
    return row("<h1>Conflicts</h1>", sp(), select("Repository filter", "All repositories", "kiln-controller", "loom-patterns"),
               seg("All", "Detected", "Queued", "Deferred", "Resolved", on=on, label="Status"))


def g05_list(o):
    t = table(["Status", "File", "Repository", "Type", "Found at", "Created", ""], [
        [tone("warn", "conflict", "Detected"), mono("firmware/display/layout.c"), link("kiln-controller / display-v2", "g04-overview.html"), "Content", f"{svn('r4131')} · {git('5e1d7a0')}", "12 Nov, 13:41", link("<b>Resolve</b>", "g06-conflict-diff.html")],
        [tone("off", "off", "Deferred"), mono("docs/wiring.svg"), link("kiln-controller", "g04-overview.html"), "Binary", f"{svn('r4102')} · {git('b7730ce')}", "10 Nov, 09:12", link("Resolve", "g06-conflict-diff.html")],
        [tone("ok", "check", "Resolved"), mono("config/limits.toml"), link("kiln-controller", "g04-overview.html"), "Edit/Delete", f"{svn('r4090')} · {git('0c3a11e')}", "8 Nov, 16:30", link("View", "g06-conflict-states.html")]],
        cap="Conflicts")
    return "Conflicts", f'<div class="pg">{conflict_filters()}{sec("", t)}{pager("3 matching conflicts · Page 1 / 1", nxt=False)}' \
                        '<p class="t3 sm">An empty list means no conflict rows match these filters. It does not prove both sides are in sync.</p></div>'


def g05_states(o):
    return "Conflicts: states", pg("Conflicts: states", board(
        ("Loading", sec("", loading("Loading conflicts…"))),
        ("Read failed", sec("", empty("Error loading conflicts", "API error 502. Results are unknown.", btn("Retry", "sm"), role="alert", icon="error"))),
        ("No matches", sec("", empty("No matching conflicts", "Try another status or repository filter."))),
        ("Nothing on this page", sec("", empty("No conflicts on this page", "Matching records exist on other pages.", btn("Return to first page", "sm")))),
        ("Invalid filter in the address", sec("", empty("Invalid page. Return to the first page.", "", btn("Clear filters", "sm"), role="alert", icon="error"))),
        ("Repository no longer registered", sec("", conflict_filters(1), '<p class="t3 sm">Filtering by “Repository old-fixtures”, which is no longer registered. Its historic rows remain reachable.</p>')),
    ))


CONFLICT_HEAD = dict(crumbs=[("Conflicts", "g05-conflicts.html"), ("firmware/display/layout.c", None)],
                     meta=tone("warn", "conflict", "Detected") + link("kiln-controller / display-v2", "g04-overview.html"))
CONFLICT_META = kv(("Type", "Content"), ("SVN revision", svn("r4131")), ("Git SHA", git("5e1d7a0c22b1")), ("Created", "12 Nov 2026, 13:41:07"))
TRUTH = banner("", "At this version RepoSync records your decision. It does not write the chosen content to SVN or Git.",
               "Applying resolved content is open work (issue #73). Apply the change yourself, then confirm both sides on the repository page.")


def decision(edit=False):
    opts = stack(choice("Accept SVN", "Record that the SVN version was chosen for this file.", name="res"),
                 choice("Accept Git", "Record that the Git version was chosen for this file.", name="res"),
                 choice("Manual edit", "Record a manual resolution. The text in the Manual edit view is sent with the decision.", name="res"))
    return sec("Resolution", opts, TRUTH, row(btn("Defer"), sp(), '<span class="t3 sm">Choose a decision first.</span>', btn("Record decision…", "primary", disabled=True)))


def g06_diff(o):
    panes = cols((6, diff(ic("svn") + " SVN version · r4131", " #define ROWS 4", "-#define COLS 20", " void draw(void);")),
                 (6, diff(ic("git") + " Git version · 5e1d7a0", " #define ROWS 4", "+#define COLS 18", " void draw(void);")))
    base = sec("Common base", diff("Base · r4119", " #define ROWS 4", " #define COLS 16", " void draw(void);"),
               note="<span class='t3 sm'>Returned by the server today; not shown by the current page.</span>")
    tabs = '<div class="tabs" role="group" aria-label="Conflict view"><a href="g06-conflict-diff.html" aria-current="page">Side-by-side diff</a><a href="g06-conflict-edit.html">Manual edit</a></div>'
    return "Conflict: diff", pg(mono("firmware/display/layout.c"), sec("", CONFLICT_META), tabs, panes, base, decision(), **CONFLICT_HEAD)


def g06_edit(o):
    tabs = '<div class="tabs" role="group" aria-label="Conflict view"><a href="g06-conflict-diff.html">Side-by-side diff</a><a href="g06-conflict-edit.html" aria-current="page">Manual edit</a></div>'
    ed = sec("", field("Edit merged content:", "#define ROWS 4\n#define COLS 20\nvoid draw(void);", "textarea", cls="mono wide", rows=9,
                       help="Starts from the Git version. This content is sent with the decision; the server does not store or apply it at this version."))
    return "Conflict: manual edit", pg(mono("firmware/display/layout.c"), sec("", CONFLICT_META), tabs, ed, decision(True), **CONFLICT_HEAD)


def g06_states(o):
    confirm = dialog("Record this decision?", "", kv(("File", mono("firmware/display/layout.c")), ("Repository", "kiln-controller / display-v2"), ("Decision", "<b>Accept SVN</b> (r4131)"), ("Not chosen", "Git version " + git("5e1d7a0"))),
                     conseq(("info", "This marks the conflict resolved in RepoSync. It does not change either repository.")),
                     foot=btn("Go back", "ghost focus-demo") + btn("Record decision", "primary"))
    return "Conflict: states", pg("Conflict: states", board(
        ("Loading", sec("", loading("Loading conflict…"))),
        ("Not found", sec("", empty("Conflict not found", "It may belong to a repository that was removed.", btn("Back to conflicts", "sm", href="g05-conflicts.html"), icon="unknown"), row(needs("Today this stays on “Loading...”")))),
        ("Read failed", sec("", empty("The conflict could not be loaded", "API error 500.", btn("Retry", "sm"), role="alert", icon="error"))),
        ("Recording", banner("run", "Recording decision…", "Request in progress.", role="status")),
        ("Decision failed", banner("err", "Resolution failed", "API error 500: failed to resolve conflict.", role="alert")),
        ("Deferral failed", banner("err", "Deferral failed", "API error 502.", role="alert")),
        ("Deferred", sec("", row(tone("off", "off", "Deferred")), '<p class="t2 sm">Deferred on 10 Nov. It still needs a decision.</p>')),
        ("Resolved", banner("ok", "Resolved as accept_svn", "Recorded 12 Nov 2026, 14:20:11. The record names the resolver as “api” today, not the signed-in person.", role="status")),
        ("Applying and verified: not available", banner("held", "Not available at this version", "Requested, applying, held and verified-on-both-sides states arrive with issue #73. Nothing in this design claims them.")),
        ("Binary conflict", banner("", "Binary file", "Shown like any other conflict today: the diff and Manual edit are both offered. This design changes nothing here.")),
    ), sec("Confirmation", confirm))


# ---------------------------------------------------------------- G07
def audit_filters(view=0):
    return (row("<h1>Audit log</h1>", '<span class="t3 sm">Complete activity trail of all sync operations, events, and changes</span>')
            + row(select("Repository filter", "All repositories", "kiln-controller"), select("Action filter", "All actions", "Sync cycles", "Sync refused", "Errors acknowledged"),
                  seg("All", "Success", "Failures", label="Result"), seg("Stream", "Table", on=view, label="View"), sp(),
                  '<span class="t3 sm">Showing 50 of 1,284 matching entries · 47 successes, 3 failures on this page</span>'))


def g07_stream(o):
    day = lambda d, items: f'<h3 class="t3 sm">{d}</h3><ul class="actlist">' + "".join(
        f"<li>{ic(i)}<span><b>{t}</b></span><span class='t3 sm'>{x}</span><time>{tm}</time></li>" for i, t, x, tm in items) + "</ul>"
    body = sec("", day("Today", [("check", "Sync cycle " + chip("×6"), "kiln-controller · r4127 · " + link("Show group"), "14:06"),
                                 ("x", "Sync refused (failed)", "greenhouse-telemetry · " + link("Show details"), "13:57"),
                                 ("check", "Errors acknowledged", "All repositories · 4 acknowledged, originals kept · by ines", "12:30")]),
               day("Yesterday", [("check", "Import (full)", "loom-patterns · started by ines", "16:02"), ("check", "Managed removal completed", "woodshop-archive", "11:48")]))
    return "Audit log: stream", f'<div class="pg">{audit_filters(0)}{body}{pager("Page 1 of 26")}</div>'


def g07_table(o):
    t = table(["Outcome", "Time", "Repository", "Action", "Direction", "Details", "Author", "SVN rev", "Git SHA"], [
        [ic("check") + " Success", "14:06:12", "kiln-controller", "sync cycle", chip("SVN → Git"), link("Show audit details: 1 revision applied", cls="wrap"), "mei", svn("r4127"), git("9f3c2ab4")],
        [ic("x") + " Failed", "13:57:44", "greenhouse-telemetry", "sync refused", chip("Git → SVN"), link("Hide audit details"), "system", "none", git("a91f3e2c")],
        {"cls": "sub", "cells": ['<td colspan="9">' + kv(("Audit ID", "#18422"), ("Action", mono("sync_refused")), ("Status", "Failed"), ("Timestamp", "12 Nov 2026, 13:57:44"),
                                                         ("Full details", mono("svn: E160028: 'sensors/calib.json' is out of date; third consecutive permanent error, sync paused")),
                                                         ("Direction", mono("git_to_svn")), ("Author", "System")) + "</td>"]},
        [ic("check") + " Success", "12:30:55", "All repositories", "errors acknowledged", "none", link("Show audit details: 4 acknowledged", cls="wrap"), "ines", "none", "none"]], cap="Audit entries", cls="dense")
    return "Audit log: table", f'<div class="pg">{audit_filters(1)}{sec("", t)}{pager("Page 1 of 26")}</div>'


def g07_states(o):
    return "Audit log: states", pg("Audit log: states", board(
        ("Loading", sec("", loading("Loading audit log…"))),
        ("Read failed", sec("", empty("Error loading audit log", "API error 502. Results are unknown.", btn("Retry", "sm"), role="alert", icon="error"),
                            '<p class="t3 sm">Success count unknown · Failure count unknown · Matching total unknown</p>')),
        ("No matches", sec("", empty("No matching audit entries", ""))),
        ("Nothing on this page", sec("", empty("No audit entries on this page", "Matching records exist on other pages.", btn("Return to first page", "sm")))),
        ("Invalid filter", sec("", empty("Invalid success filter. Choose all, success or failures.", "", btn("Clear filters", "sm"), role="alert", icon="error"))),
        ("Stale after a failed refresh", sec("", '<div class="dim">' + table(["Outcome", "Time", "Action"], [[ic("check") + " Success", "14:06:12", "sync cycle"]]) + "</div>",
                                             row(stamp("Last loaded 3m 21s ago · refresh failed", "stale"), btn("Retry", "sm")))),
        ("Action not in the list", sec("", row(select("Action filter", "path_violation_filtered", "All actions")), '<p class="t3 sm">An action from the address that the list does not name is still shown and still filters.</p>')),
    ), note("Event words are the server's action strings with underscores removed. Acknowledging errors adds its own entry and leaves the originals unchanged."))


# ---------------------------------------------------------------- G08
def cred_card(title, state, *body, acts=""):
    return sec(title, *body, acts=acts, note=state)


def g08_account(o):
    profile = sec("My profile", form(field("Username", "ines", disabled=True), field("Role", "admin", disabled=True), field("Display name", "Ines Okafor"), field("Email", "ines@example.org", "email")),
                  row(btn("Save profile", "primary")))
    pw = sec("Change password", form(field("Current password", "", "password"), field("New password", "", "password", help="At least 8 characters."), field("Confirm password", "", "password")),
             row(btn("Change password", disabled=True)))
    creds = cols((4, cred_card("SVN password", chip("Configured"), form(field("Server URL", "https://svn.makerspace.example", cls="mono"), field("Username", "ines"), field("Password", "", "password", ph="Current password is set"), cls="one"),
                               row(btn("Save", "primary sm"), btn("Test connection", "sm"), btn("Delete…", "danger sm")))),
                 (4, cred_card("SVN token", chip("Not configured"), form(field("Server URL", "", cls="mono", ph="https://svn.example.com"), field("Token", "", "password", ph="Enter token"), cls="one"), row(btn("Save", "primary sm", disabled=True)))),
                 (4, cred_card("Git token", chip("Configured"), form(field("Server URL", "https://github.com", cls="mono"), field("Token", "", "password", ph="Current token is set"), cls="one"),
                               row(btn("Save", "primary sm"), btn("Test connection", "sm"), btn("Delete…", "danger sm")))))
    return "My account", pg("My account", profile, pw, sec("My credentials", '<p class="t3 sm">Stored for your account. Repository sync uses the credentials on each repository\'s Settings.</p>'
                                                         + row(needs("What reads these credentials is unconfirmed")), creds),
                            meta='<span class="t3 sm">Manage your profile and credentials</span>')


def g08_states(o):
    return "My account: states", pg("My account: states", board(
        ("Directory-controlled profile", sec("My profile", form(field("Username", "tomas", disabled=True), field("Role", "ldap", disabled=True), field("Display name", "Tomás Rivera", disabled=True), field("Email", "tomas@corp.example", disabled=True)),
                                             '<p class="t3 sm">Name and email come from the directory and cannot be edited here. Password change is not offered for directory accounts.</p>')),
        ("Profile saved", banner("ok", "Profile updated successfully", role="status")),
        ("Profile save failed", banner("err", "Failed to update profile", "API error 500.", role="alert")),
        ("Password rules", form(field("New password", "short", "password", err="Password must be at least 8 characters"), field("Confirm password", "shorter", "password", err="Passwords do not match"), cls="one")),
        ("Current password missing", banner("err", "Current password is required", role="alert")),
        ("Password changed", banner("ok", "Password changed successfully", role="status")),
        ("Credential test: the result today", sec("", stack(row(btn("Test connection", "sm")), banner("err", "API error 405:", role="alert")),
                                                  '<p class="t3 sm">The page shows the client\'s error text: “API error”, the status, and the reply. No test route is registered for personal credentials, so no other result can appear today. '
                                                  'The status drawn here is read from the router\'s code, not from a running server; a proxy in front of the server may answer differently.</p>',
                                                  row(needs("A test that works is feature work (row G08.6)")))),
        ("Credential save failed", banner("err", "Failed to save credential", 'API error 400: {"error":"service, server_url, and username are required"}', role="alert"), ),
        ("Delete credential confirmation", dialog("Delete your Git token?", "github.com", conseq(("x", "The stored token is deleted and cannot be read back."), ("info", "Repository credentials are separate and are not changed.")),
                                                  foot=btn("Cancel", "ghost focus-demo") + btn("Delete token", "danger solid"))),
        ("Credentials loading", sec("My credentials", loading("Loading credentials…"))),
        ("Account unavailable", sec("", empty("Unable to load user information", "Please log in again.", btn("Sign in", "primary", href="g01-login.html"), role="alert", icon="error"))),
    ), note("“Leave blank to keep current” is not what the server does today for these credentials; the design drops that wording and asks for an explicit replace."))


# ---------------------------------------------------------------- G09
USERS = [("ines", "Ines Okafor", "ines@example.org", "admin", "Local", "Active"), ("tomas", "Tomás Rivera", "tomas@corp.example", "user", "Directory", "Active"),
         ("mei", "Mei-Ling Zhou", "mei@example.org", "user", "Local", "Disabled")]


def g09_users(o):
    t = table(["Username", "Display name", "Email", "Role", "Status", "Actions"], [
        [mono(u), n, m, chip(r), (ic("check") if st == "Active" else ic("off")) + " " + st,
         btn("Edit", "sm") + " " + btn("Delete…", "sm danger", disabled=(u == "ines"))] for u, n, m, r, s, st in USERS], cap="Users")
    return "Users", pg("Users", sec("", t), '<p class="t3 sm">You cannot delete the account you are signed in with.</p>',
                       row(needs("Whether an account comes from the directory is not reported today, so it is not shown")), acts=btn("Create user", "primary", icon="plus"),
                       meta='<span class="t3 sm">Administrator only</span>')


def g09_dialogs(o):
    create = dialog("Create user", "Review the selected user's settings. Changes are submitted only by the save action.",
                    form(field("Username", "", ph="johndoe", required=True), field("Display name", "", ph="John Doe"), field("Email", "", "email", ph="john@example.com"),
                         field("Password", "", "password", ph="Minimum 8 characters", required=True), field("Role", ["user", "admin"], "select")),
                    foot=btn("Cancel", "ghost focus-demo") + btn("Create user", "primary"))
    edit = dialog("Edit user: tomas", "Review the selected user's settings. Changes are submitted only by the save action.",
                  form(field("Display name", "Tomás Rivera"), field("Email", "tomas@corp.example", "email"), field("Role", ["user", "admin"], "select")), check("Account enabled", on=True),
                  '<p class="t3 sm">The username cannot be changed. Two people may share a display name; the username in the title identifies the account.</p>',
                  foot=btn("Cancel", "ghost focus-demo") + btn("Save", "primary"))
    delete = dialog("Delete user", "", '<p>Delete user "mei"? This cannot be undone.</p>', foot=btn("Cancel", "ghost focus-demo") + btn("Delete", "danger solid"))
    return "Users: dialogs", pg("Users: dialogs", board(("Create user", create), ("Edit user", edit), ("Delete confirmation", delete),
                                                        ("Saving", banner("run", "Request in progress", "Please wait before closing this dialog.", role="status")),
                                                        ("Create failed", banner("err", "Could not create the user", "API error 500. A duplicate username currently returns this generic failure.", role="alert")),
                                                        ("Delete failed", banner("err", "Could not delete the user", "API error 403: admin access required", role="alert"))))


def g09_states(o):
    return "Users: states", pg("Users: states", board(
        ("Loading", sec("", loading("Loading users…"))),
        ("Not an administrator", sec("", empty("This needs an administrator", "You are still signed in as <b>tomas</b>. Nothing was changed.", icon="shield"))),
        ("Read failed", sec("", empty("Users could not be loaded", "API error 502. Results are unknown.", btn("Retry", "sm"), role="alert", icon="error"))),
        ("Your own row", sec("", table(["Username", "Actions"], [[mono("ines") + " " + chip("you"), btn("Edit", "sm") + " " + btn("Delete…", "sm danger", disabled=True)]]),
                             '<p class="t3 sm">Delete is unavailable for the signed-in account.</p>')),
    ))


# ---------------------------------------------------------------- G10 / G11
def ldap_form(test=""):
    return stack(sec("Directory sign-in", check("Enable LDAP authentication", on=True, kind="switch"),
                     '<p class="t3 sm">When enabled, users can log in with their directory username and password.</p>'),
                 sec("Connection", form(field("LDAP URL", "ldaps://ldap.corp.example:3269", cls="mono"), field("Base DN", "dc=corp,dc=example,dc=com", cls="mono"),
                                        field("Search filter", "(&(objectClass=user)(name={0}))", cls="mono", help="Use {0} as a placeholder for the username."))),
                 sec("User mapping", form(field("Display name attribute", "displayname", cls="mono"), field("Email attribute", "mail", cls="mono"), field("Group attribute", "memberOf", cls="mono"))),
                 sec("Service account (optional)", form(field("Bind DN", "cn=svc-reposync,ou=service,dc=corp,dc=example,dc=com", cls="mono"),
                                                        field("Bind password", "", "password", ph="Leave blank to keep current", help="A password is saved. Leave blank to keep it; type a new one to replace it."))),
                 test, row(btn("Save configuration", "primary"), btn("Test connection")))


def g10_directory(o):
    return "Directory (LDAP)", pg("Directory (LDAP)", ldap_form(), meta='<span class="t3 sm">Administrator only</span>')


def g10_states(o):
    return "Directory (LDAP): states", pg("Directory (LDAP): states", board(
        ("Loading", sec("", loading("Loading directory settings…"))),
        ("Not an administrator", sec("", empty("Admin privileges required", "You need admin privileges to view this page.", icon="shield"))),
        ("Test passed", banner("ok", "LDAP connection successful", role="status")),
        ("Test failed", banner("err", "LDAP connection test failed: …", "The rest is the directory client's own error text. Your entries stay in the form.", role="alert")),
        ("Saved", banner("ok", "LDAP configuration saved", role="status")),
        ("Save failed", banner("err", "Save failed", "API error 500. Your entries were kept and nothing was marked invalid.", role="alert")),
        ("Bind password not set", form(field("Bind password", "", "password", ph="Service account password", help="No password is saved."), cls="one")),
        ("Disabled", sec("", check("Enable LDAP authentication", kind="switch"), '<p class="t3 sm">Local accounts continue to work. Saved directory settings are kept.</p>')),
    ), note("The server keeps the existing bind password when the field is empty. There is no way to clear it through this page."))


def g11_main(o):
    maps = table(["SVN username", "Git name", "Git email", "GitHub", "Action"], [
        [mono("ines"), "Ines Okafor", "ines@example.org", "none", btn("Remove…", "sm ghost")],
        [mono("tomas"), "Tomás Rivera", "tomas@example.org", "none", btn("Remove…", "sm ghost")]], cap="Author identity mappings")
    add = row(field("SVN username", "", ph="SVN username", cls="mono"), field("Full name", "", ph="Full name"), field("Email", "", "email", ph="email@example.org"), btn("Add"))
    teams = sec("Notifications: Microsoft Teams", '<details><summary>Setup instructions</summary><ol class="t2 sm"><li>Open the Teams channel and choose Workflows.</li>'
                '<li>Select “Send webhook alerts to a channel”, name it and add it.</li><li>Copy the webhook URL and paste it below.</li></ol></details>',
                kv(("Teams webhook URL", ic("check") + " Set · " + mono("https://prod-00.example.invalid/workflows/…")), ),
                row(btn("Replace…", "sm"), btn("Send test", "sm"), btn("Remove…", "sm danger")),
                '<p class="t3 sm">Notifications will be sent for: sync activity, errors, circuit breaker alerts, imports, branch pair changes, and path violations. No-change sync cycles are automatically filtered out.</p>')
    return "Identity and notifications", pg("Identity &amp; notifications", sec("Author identity mappings",
        '<p class="t2 sm">Map SVN usernames to Git author identities. These mappings ensure commits are attributed to the correct developer on both sides.</p>', maps, add), teams,
        meta='<span class="t3 sm">Shown in navigation to administrators · applies to every repository on this server</span>')


def g11_states(o):
    return "Identity and notifications: states", pg("Identity &amp; notifications: states", board(
        ("Loading", sec("", loading("Loading mappings…"))),
        ("Mappings could not be loaded", sec("", empty("Mappings could not be loaded", "They are unknown, so editing is turned off. Saving now would replace the whole list on the server.", btn("Retry", "sm"), role="alert", icon="error"),
                                             row(needs("Today this renders as an empty table")))),
        ("No mappings", sec("", empty("No mappings yet", "Unmapped authors use the fallback email domain."))),
        ("Remove confirmation", dialog("Remove this mapping?", "", kv(("SVN username", mono("tomas")), ("Git author", "Tomás Rivera &lt;tomas@example.org&gt;")),
                                       conseq(("info", "Future commits by this SVN user use the fallback identity. Past commits are not rewritten.")),
                                       foot=btn("Cancel", "ghost focus-demo") + btn("Remove mapping", "danger solid"))),
        ("Mapping save failed", banner("err", "The mappings were not saved", "API error 500. The list on the server is unchanged as far as is known.", role="alert"), ),
        ("Webhook not set", sec("Notifications: Microsoft Teams", kv(("Teams webhook URL", ic("minus") + " Not set")), row(btn("Set…", "sm")))),
        ("Webhook could not be read", sec("Notifications: Microsoft Teams", empty("The notification setting could not be read", "It is unknown. Saving is turned off so an existing webhook is not cleared.", btn("Retry", "sm"), role="alert", icon="error"),
                                          row(needs("Today this renders as an empty field")))),
        ("Saved", banner("ok", "Saved", role="status")), ("Save failed", banner("err", "Save failed", "Your entry was kept.", role="alert")),
        ("Test sent", banner("ok", "Test notification sent!", role="status")), ("Test failed", banner("err", "Failed to send test", role="alert")),
    ), note("These are server-wide settings. Personal credentials are on My account; server and sign-in settings are on Server setup."))


# ---------------------------------------------------------------- G12
WIZ = ["Welcome", "SVN repository", "Git provider", "Sync settings", "Identity mapping", "Server and auth", "Review", "Import"]
UNSENT = needs("Collected but not sent today")


def wiz_body(i):
    if i == 0:
        return stack('<h2>Welcome to RepoSync</h2><p class="t2">This wizard guides you through setting up synchronization between an SVN repository and Git, then saves the configuration to this server.</p>',
                     '<div class="cards">' + "".join(f'<div class="card"><h3>{t}</h3><span class="t2 sm">{d}</span></div>' for t, d in [
                         ("SVN and Git", "Connect your SVN repository and Git provider (GitHub or Gitea)."), ("Identity", "Map SVN usernames to Git author identities."),
                         ("Save and import", "Save the configuration to this server, then import SVN history.")]) + "</div>")
    if i == 1:
        return stack(form(field("Repository URL", "svn://svn.makerspace.example/repos/kiln", cls="mono wide", help="The root URL of your SVN repository", required=True),
                          field("Username", "sync-service", help="Service account username for SVN access", required=True),
                          field("Password", "", "password", ph="••••••• (saved)", help="Stored securely on the server — never written to config files")),
                     form(field("Repository layout", ["Standard (trunk/branches/tags)", "Custom paths"], "select", help="Standard layout uses trunk/, branches/, and tags/ under the repo root"),
                          field("Trunk path", "trunk", cls="mono"), field("Branches path", "branches", cls="mono", disabled=True), field("Tags path", "tags", cls="mono", disabled=True)),
                     row(btn("Test connection"), '<span class="okl" role="status">' + ic("check") + "Connected</span>"))
    if i == 2:
        return stack(row(seg("GitHub / GitHub Enterprise", "Gitea", label="Git provider")),
                     form(field("API URL", "https://api.github.com", cls="mono wide", help="Use https://api.github.com for GitHub.com, or your GHE API URL", required=True),
                          field("Repository", "makerspace/kiln-controller", cls="mono", help="In owner/repo format", required=True), field("Default branch", "main", cls="mono"),
                          field("Personal access token", "", "password", ph="••••••• (saved)", help="Needs repo scope. Stored securely on the server.")),
                     row(btn("Test connection")))
    if i == 3:
        return stack(row(seg("Direct push", "Pull request", label="Sync mode")), row(check("Auto merge", on=True, kind="switch", help="Automatically resolve 3-way merge conflicts when possible"),
                     check("Sync tags", on=True, kind="switch", help="Synchronize SVN tags to Git tags and vice versa")),
                     sec("File policy and large file storage (LFS)", form(field("LFS threshold (MB)", "0", "number", help="Files larger than this are tracked via Git LFS. Set 0 to disable."),
                         field("Max file size (MB)", "0", "number", help="Files larger than this are skipped entirely. Set 0 for no limit."),
                         field("LFS patterns", "", ph="*.psd, *.zip", help="Comma-separated glob patterns."), field("Ignore patterns", "", ph="*.tmp, .svn/", help="Comma-separated glob patterns."))),
                     sec("Pull request settings", form(field("PR title prefix", "[svn-sync]"), field("Labels", "", ph="svn-sync, automated"), field("Reviewers", "", ph="username1, username2")),
                         row(check("Auto-merge PRs", kind="switch"), UNSENT)))
    if i == 4:
        return stack(form(field("Fallback email domain", "example.org", help="Unmapped users get email: svn-username@domain"), field("Author mapping file", "", cls="mono", ph="/etc/reposync/authors.toml", help="Optional: path to a TOML file with explicit mappings")),
                     sec("Inline author mappings", table(["SVN username", "Git name", "Email", ""], [[mono("ines"), "Ines Okafor", "ines@example.org", btn("Remove", "sm ghost")]]),
                         row(field("SVN username", "", ph="SVN username", cls="mono"), field("Full name", "", ph="Full name"), field("Email", "", "email", ph="email@example.org"), btn("Add"))))
    if i == 5:
        return stack(sec("Web server", form(field("Listen address", "0.0.0.0:8080", cls="mono", help="host:port to bind the web server", required=True),
                                            field("Admin password env var", "REPOSYNC_ADMIN_PASSWORD", cls="mono", required=True)),
                         form(field("Authentication mode", ["Simple (password)", "GitHub OAuth", "Both"], "select"), cls="one"),
                         row(UNSENT, '<span class="t3 sm">Authentication mode and the OAuth fields</span>')),
                     sec("Daemon settings", form(field("Poll interval (seconds)", "60", "number", required=True), field("Log level", ["Info", "Trace", "Debug", "Warn", "Error"], "select"), field("Data directory", "/var/lib/reposync", cls="mono", required=True))),
                     sec("Notifications", form(field("Slack webhook env var", "", cls="mono", ph="REPOSYNC_SLACK_WEBHOOK"), field("Email SMTP server", "", cls="mono", ph="smtp.example.org:587"),
                                               field("From address", "", ph="reposync@example.org"), field("Email recipients", "", ph="admin@example.org")), row(UNSENT)))
    if i == 6:
        card = lambda t, *p: f'<div class="card"><h3>{t}</h3>{kv(*p)}</div>'
        return stack('<div class="cards">' + card("SVN", ("URL", mono("svn://svn.makerspace.example/repos/kiln")), ("Username", "sync-service"), ("Layout", "standard"), ("Trunk", "trunk"))
                     + card("Git", ("Provider", "github"), ("API URL", mono("https://api.github.com")), ("Repository", mono("makerspace/kiln-controller")), ("Branch", "main"))
                     + card("Sync", ("Mode", "direct"), ("Auto merge", "Yes"), ("Sync tags", "Yes")) + card("Server", ("Listen", mono("0.0.0.0:8080")), ("Poll interval", "60s"), ("Log level", "info")) + "</div>",
                     '<p class="t2 sm">1 identity mapping will be saved</p>', row(btn("Save configuration to server", "primary")))
    return stack('<ol class="phases"><li class="done">Connect</li><li class="now">Import &amp; push</li><li>Verify</li><li>Final push</li><li>Complete</li></ol>',
                 meter(41, "SVN import progress — Setup import"), '<p class="t2 sm" role="status">running — importing: 41%</p>',
                 kv(("Revisions", "768 / 1871"), ("Commits created", "766"), ("Current files", "12"), ("LFS files", "12"), ("Batches pushed", "15"), ("Operation", mono("2b9d7f10…1a44"))),
                 row(btn("Stop import…", "warn")), sec("Operation log", log("[14:08:29] [info] r768 12 files", "[14:08:31] [push] batch 15 pushed (50 commits)")))


def g12_step(i):
    def f(o):
        nav = row(btn("Back", "ghost") if 0 < i < 7 else "", sp(), link("Exit to dashboard", "g02-dashboard.html"),
                  btn("Get started" if i == 0 else "Next: import history" if i == 6 else "Next", "primary", href=f"g12-setup-{min(i + 2, 8)}.html") if i < 7 else "")
        return f"Server setup: {WIZ[i]}", pg("Server setup", steps(WIZ, i), sec(WIZ[i], wiz_body(i)), nav,
                                             meta='<span class="t3 sm">Shown in navigation to administrators · configures this server and its first repository</span>')
    return f


def g12_states(o):
    return "Server setup: states", pg("Server setup: states", board(
        ("Saved configuration could not be loaded", sec("", empty("The saved configuration could not be loaded", "The form is turned off so defaults are not saved over existing settings.", btn("Retry", "sm") + btn("Start from defaults", "sm ghost"), role="alert", icon="error"),
                                                        row(needs("Today the form shows defaults")))),
        ("Validation", form(field("Repository URL", "kiln", cls="mono", err="Must start with svn://, http://, or https://", required=True), field("Repository", "kiln-controller", cls="mono", err="Must be in owner/repo format", required=True),
                            field("Listen address", "8080", cls="mono", err="Must be in host:port format (e.g. 0.0.0.0:8080)", required=True), cls="one")),
        ("Connection test failed", row(btn("Test connection"), '<span class="err" role="alert">' + ic("error") + "Failed to run svn command: …</span>", '<span class="t3 sm">The rest is svn\'s own error text.</span>')),
        ("Configuration saved", banner("ok", "Configuration saved", "Continue to import your SVN repository.", role="status")),
        ("Saved with warnings", banner("warn", "Saved with warnings", "Git LFS is configured but not available: … Large files will be committed directly.", role="status")),
        ("Save failed", banner("err", "Error", "API error 500.", role="alert")),
        ("Import: ready", sec("", row(btn("Start full import", "primary", icon="run")), '<p class="t3 sm">This will import all SVN revisions as individual Git commits.</p>')),
        ("Import: stop requested", banner("warn", "Cancellation requested — stopping", "Published history is not undone.", role="status")),
        ("Import: stopped", banner("warn", "Import stopped", "766 commits were imported before the stop. This repository is held until its history is reconciled.", role="status")),
        ("Import: failed", banner("err", "Import failed", mono("svn: E175002: connection reset during REPORT"), role="alert")),
        ("Import: complete", banner("ok", "Import complete", "1,866 commits imported from 1,871 SVN revisions. Tree verification matched.", acts=btn("Go to dashboard", "primary sm", href="g02-dashboard.html"), role="status")),
        ("Import: complete, verification differs", banner("warn", "Import complete; verification differs", "2 mismatched files, 1 missing in Git.", role="alert")),
        ("Status read stale", row(stamp("Stale · last read 3m 21s ago", "stale"), btn("Retry read", "sm"))),
    ))


# ---------------------------------------------------------------- G13
DOCS = [("overview", "Overview", ["What RepoSync does", "Team mode and Personal Branch Mode", "How a sync cycle runs"]),
        ("getting-started", "Getting Started", ["Prerequisites", "First sign-in", "Add a repository", "Run the first import"]),
        ("dashboard", "Dashboard", ["Repository scope", "Health and freshness", "Sync records and commit map", "Server monitor"]),
        ("repositories", "Managing Repositories", ["Add and edit", "Branch pairs", "Pause, remove, restore", "Path policy"]),
        ("identity-mapping", "Identity Mapping", ["Lookup order", "Mapping file", "Fallback domain"]),
        ("conflict-resolution", "Conflict Resolution", ["When conflicts occur", "Conflict types", "Accept SVN, Accept Git, Manual edit, Defer"]),
        ("configuration", "Configuration", ["Server settings", "Environment variables for secrets", "PR-Based Workflow", "CLI Commands"]),
        ("personal-mode", "Personal Branch Mode", ["What differs from team mode", "Command line only", "Pull-request gating"]),
        ("deployment", "Deployment", ["systemd", "Docker", "Docker Compose", "Health Check", "File Paths"]),
        ("troubleshooting", "Troubleshooting", ["Common Issues", "Diagnostic Commands", "Getting Help"])]


def g13_doc(i):
    slug, title, topics = DOCS[i]

    def f(o):
        nav = '<nav class="pg-sections" aria-label="Documentation sections">' + "".join(
            f'<a href="g13-docs-{s}.html"{" aria-current=page" if s == slug else ""}>{t}</a>' for s, t, _ in DOCS).replace("aria-current=page", 'aria-current="page"') + "</nav>"
        body = sec(title, '<p class="t2">Section text is abbreviated here; the full text is the in-app documentation. The layout, navigation and content types are what this page shows.</p>',
                   "".join(f"<h3>{t}</h3><p class='t2'>How “{t.lower()}” works in RepoSync, with the terms the interface uses.</p>" for t in topics),
                   log("reposync status --repo kiln-controller", "curl -s http://localhost:8080/api/status/health"),
                   '<figure class="ph"><div class="ph-box">Placeholder figure</div><figcaption>The in-app documentation marks where a screenshot will go. The placeholder is kept.</figcaption></figure>',
                   '<p class="t2 sm">See also: ' + ", ".join([link(t, f"g13-docs-{s}.html") for s, t, _ in DOCS if s != slug][:4]) + "</p>")
        return f"Help: {title}", pg("Help", body, sections=nav, status='<span class="t3 sm">Ten sections. On a phone the section list wraps onto more lines above the text.</span>')
    return f


# ---------------------------------------------------------------- non-route surfaces
def s01_shell(o):
    grp = lambda rows: table(["Group", "Destination", "Route", "Who sees it"], rows, cap="Navigation entries")
    rows = [[GROUP_LABELS[g], l, mono(to), ("Administrators, in navigation only (#268)" if to in ("/config", "/setup") else "Administrators only") if adm else "Everyone signed in"] for to, l, g, adm, _, _ in NAV]
    status = board(("Idle", row(pill("idle"))), ("Needs attention", row(pill("held"), '<span class="t3 sm">The worst health among the repositories, which is what the read returns. No count.</span>')),
                   ("Stale", row(pill("unknown"), stamp("Stale", "stale"))), ("Unavailable", row(pill("unknown"), stamp("Status unavailable", "bad"))),
                   ("Loading", row(pill("unknown"), '<span class="t3 sm">Loading status</span>')), ("Conflict count", row("Conflicts", '<span class="shell-count">1</span>')))
    return "Shell: navigation and roles", pg("Shell: navigation and roles",
        '<p class="t3 sm">This specimen has two pages. The shell above the administrator page is the administrator\'s navigation; the shell above the non-administrator page is the other.</p>',
        sec("Navigation for an administrator", grp(rows)),
        sec("Navigation for a non-administrator", grp([r for r, n in zip(rows, NAV) if not n[3]]),
            '<p class="t3 sm">Visibility follows the signed-in account exactly as today. A layout never changes it. Users and Directory (LDAP) refuse other accounts with a message. Server setup and Identity &amp; notifications are only hidden in navigation today. '
            'Issue #268 records which of their calls need only a session; saving and testing the Teams webhook are checked for the administrator role.</p>'),
        sec("Status in the shell", status), sec("Account and sign-out", kv(("Account name", "Display name, or the username when no display name is set"), ("Sign out", "Ends the session at once; a failed server revocation does not keep you signed in"),
                                                 ("Help", "Opens the in-app documentation " + needs("New link to an existing route")))))


def s02_chooser(o):
    ch = lambda sel, notice="": ('<div class="chooser-demo"><span class="lbl">Layout</span><select><option>' + sel + '</option></select><span class="lbl">Theme</span>'
                                 '<select><option>Dark</option><option>Light</option></select><p class="notice">' + notice + "</p></div>")
    return "Shell: layout and theme chooser", pg("Shell: layout and theme chooser", board(
        ("Layout list", sec("", kv(("Existing, arrangement unchanged", "Classic · Individual · Team"), ("Added by this design", "Ledger · Console · Guide"), ("Default and fallback", "Classic, dark"),
                                       ("In these prototypes", "The existing three are listed greyed out because they are not drawn here. In the product they stay selectable")),
                            '<p class="t3 sm">Changes only how navigation and pages are arranged. Pages, actions, permissions and server mode stay the same.</p>')),
        ("Chosen and saved", sec("", ch(OPTIONS[o]["name"]))),
        ("Saved preference unusable", sec("", ch("Classic", "Using Classic"), '<p class="t3 sm">The saved layout preference could not be used, so Classic is shown. Choosing a layout replaces it.</p>')),
        ("Storage unreadable", sec("", ch("Classic", "Using Classic"), '<p class="t3 sm">This browser did not allow the saved layout preference to be read, so Classic is shown.</p>')),
        ("Choice not saved", sec("", ch(OPTIONS[o]["name"], "Not saved"), f'<p class="t3 sm">{OPTIONS[o]["name"]} applies in this tab until you reload; this browser did not allow the layout preference to be saved.</p>')),
        ("Dark until chosen", sec("", kv(("No saved theme", "Dark, in every layout"), ("Light", "Offered in all six layouts; a person chooses it"), ("Saved theme", "Kept per account and site, with the layout"),
                                          ("Device setting", "Not followed")), row(needs("Storing a theme needs a preference record change")))),
        ("Sign-in page", sec("", '<p class="t2 sm">One appearance for everyone: dark, as today. The saved choice belongs to an account, and nobody is signed in there.</p>')),
    ), note("Notice wording is the existing #262 text (layoutPreference.ts:187-212). The theme control is the one addition to the chooser."))


def s03_dialogs(o):
    busy = dialog("Remove from RepoSync", "Remove the managed registration and owned local data. Remote history is preserved.",
                  '<p class="t2 sm" role="status">Request in progress. Please wait before closing this dialog.</p>',
                  foot=btn("Cancel", "ghost", disabled=True) + btn("Removing…", "danger solid", disabled=True))
    return "Dialogs and transient interface", pg("Dialogs and transient interface", board(
        ("Dialog anatomy", dialog("Title names the action and its target", "One sentence says what will and will not happen.", conseq(("check", "What stays."), ("x", "What cannot be undone.")),
                                  foot=btn("Cancel", "ghost focus-demo") + btn("Named action", "primary")) + '<p class="t3 sm">Focus starts on Cancel. Tab stays inside. Escape closes unless a request is running. Focus returns to the control that opened it.</p>'),
        ("Busy dialog", busy),
        ("Inline status, not toasts", stack(banner("ok", "Saved", role="status"), '<p class="t3 sm">The product has no toast component. Outcomes appear next to the control that caused them and stay until replaced.</p>')),
        ("Alert", banner("err", "Failed to trigger sync", "repository has active or unresolved import work", role="alert")),
        ("Disclosure", sec("", '<details open><summary>Show audit details</summary><p class="t2 sm">Full details stay in the page and are not announced.</p></details>')),
        ("Explanations in the page, not in tooltips", stack(row(btn("Skip commits…", disabled=True)), '<p class="t3 sm">Unavailable: no pending commit is observed. The reason is text beside the control, not a hover tooltip.</p>')),
        ("Table controls", stack(pager("Page 2 of 26", prev=True), row(link("Show group"), link("Show audit details")))),
        ("Row menu", menu(("Open pair", ""), ("Preview update from parent", ""), ("Remove pair…", "danger"))),
        ("Secret fields", form(field("Git token", "", "password", ph="", help="A token is saved. It is never shown. Type a new one to replace it."), cls="one")),
        ("The “More” control", stack(row('<button type="button" class="shell-menu" style="display:inline-flex" aria-expanded="false">' + ic("menu") + "More</button>"),
                                     '<p class="t3 sm">One menu control in the shell opens the remaining navigation groups, the account name, the chooser and sign-out.</p>', row(needs("New shell control")))),
    ))


def s04_saves(o):
    rows = [["Unchanged", "Nothing to save", "Save is unavailable"], ["Edited", "“Unsaved changes” beside Save; draft kept in this browser without secrets", "Save, Discard"],
            ["Invalid", "Field error text; Save stays available and refuses with the first error", "Fix the field"],
            ["Saving", "“Saving in order: configuration, then credentials.”", "Wait; closing keeps the recovery record"],
            ["Saved", "Server receipt for each stage", "Form closes"],
            ["Changed on the server (stale version)", "“This save conflicts with current settings.”", "Load current settings for review, then a deliberate new save"],
            ["Result unknown (timeout or lost reply)", "“The save response timed out; its server outcome is unknown.” Never “nothing saved”", "Resume the same request; receipt lookup by request id"],
            ["Stage two not confirmed", "Configuration saved; credentials not confirmed", "Re-enter secrets or check the earlier request"],
            ["Signed out or signed in as someone else", "“The login session changed. The save is retained for its original user.”", "Sign in as that user and reopen the form"],
            ["Navigated away mid-save", "Recovery record kept; the later page is not changed by the earlier reply", "Reopen the form to resume"],
            ["Retry not allowed", "“No write will be blindly retried.”", "Review the existing repository ID first"]]
    return "Recoverable saves", pg("Recoverable saves", sec("Every state of a repository save", table(["State", "What the person sees", "What they can do"], rows, cap="Save states")),
                                   note("States, wording and the request-id recovery are the existing #115 behaviour for repository saves. Other forms (users, directory, mappings) save in one step and show saved or failed only."))


def s05_ops(o):
    rows = [["Import", "queued, running, cancel_requested, cancelling, completed, cancelled, failed, reconciliation_required", "Stop and Verify remote, shown to administrators"],
            ["Write Git commit to SVN", "queued, running, completed, failed, reconciliation_required", "None from this interface"],
            ["Push to Git", "queued, running, completed, failed, reconciliation_required", "None from this interface"],
            ["Removal", "queued, cancelling, running, completed, failed, reconciliation_required", "Retry local cleanup, Continue original reviewed operation and Restore registration, shown to administrators"],
            ["Add branch pair", "queued, svn_copy_pending, svn_copied, child_registered, replay_in_progress, completed, failed, reconciliation_required", "Inspect, Reconcile / resume and Stop, in the pairing dialog, which is shown to administrators"],
            ["Pair refresh", "unknown_or_held", "None from this interface (held under #69)"],
            ["History hold", "reconciliation_required, transient_refusal, unknown_or_held", "None; no release route exists"]]
    cards = board(("Write to SVN, running", op("Write Git commit to SVN", pill("running"), f'<span class="t2 sm">Source Git {git("77ab01c")}; SVN was at {svn("r4125")} before the write. Source/intent is not confirmed publication.</span>', opid="b71e…09aa")),
                  ("History hold", op("History hold", pill("held"), kv(("Reason", mono("observed_remote_rewrite")), ("Handled checkpoint", mono("0be77f1")), ("Fresh remote tip", mono("d4410ac"))),
                                      '<span class="t3 sm">No authenticated action releases this hold from the interface.</span>', icon="held")),
                  ("Details restricted", op("Operation", pill("held"), '<span class="t2 sm">Some evidence is unavailable or restricted; no completion is inferred.</span>', icon="held")),
                  ("Unknown state from the server", op("Operation", pill("unknown"), '<span class="t2 sm">The server reported a state this interface does not know. It is shown as Unknown and blocks actions.</span>')))
    return "Operations: one vocabulary", pg("Operations: one vocabulary", sec("Lifecycle words by operation kind", table(["Operation", "Server states", "Controls the page offers, and to whom"], rows, cap="Operation lifecycles")), cards,
                                            note("Labels are plain words for the server's lifecycle values; the raw value stays visible in details. Stopping never means undoing."))


def s06_creds(o):
    rows = [["Repository SVN password / Git token", "Set · Not set", "Not reported: own, inherited, server-wide and revoked look the same", "Repository ▸ Settings"],
            ["Personal SVN password, SVN token, Git token", "Configured · Not configured", "Per account; server URL and username shown", "My account"],
            ["Directory bind password", "Saved · Not saved", "Empty field keeps the saved value; cannot be cleared here", "Directory (LDAP)"],
            ["Setup wizard SVN password, Git token", "Saved · Not saved", "Shown as saved or not saved; the value is never shown", "Server setup"],
            ["Teams webhook URL", "Set · Not set", "Returned in full to any signed-in session today", "Identity &amp; notifications"]]
    ident = [["Profile and password", "Editable display name and email; password change offered", "My account"],
             ["Directory-controlled profile", "Drawn only for the role “ldap”. The server gives accounts it creates from the directory the role “user”, so this is not reached today", "My account"],
             ["Where an account signs in", "Not reported by the server; not shown anywhere", "none"],
             ["Role", "admin or user; shown, never editable by the person themselves", "My account, Users"], ["Legacy single-administrator sign-in", "No user record; layout preference is not saved", "Shell"]]
    return "Credentials and identity", pg("Credentials and identity", sec("How a secret's state is shown", table(["Secret", "States shown", "Provenance", "Where"], rows, cap="Secrets"),
                                          row(needs("Origin of repository credentials needs a server field"))),
                                          sec("How identity is shown", table(["Account", "What is shown", "Where"], ident, cap="Identity")),
                                          note("A secret is never displayed after saving. State is a word and a mark, never a placeholder string."))


def s07_exceptional(o):
    return "Exceptional paths", pg("Exceptional paths", board(
        ("Not allowed (403)", sec("", empty("This needs an administrator", "You are still signed in as <b>tomas</b>. Nothing was changed.", icon="shield"))),
        ("Session ended (401)", sec("", empty("Your session has ended", "Sign in again to continue. A change you had just submitted may still have been applied.", btn("Sign in", "primary", href="g01-login.html"), icon="user"))),
        ("Not found", sec("", empty("Repository not found", "This repository or branch pair is no longer available. It may have been removed.", btn("Back to repositories", "sm", href="g03-repositories.html")))),
        ("Unknown address", sec("", empty("Page not found", "Nothing is registered at this address.", btn("Go to dashboard", "sm", href="g02-dashboard.html")), row(needs("No catch-all route exists today")))),
        ("Capability held", sec("", banner("held", "Held: not available from this screen", "Re-anchor is a separate, explicitly approved mode. This interface does not start it."))),
        ("Capability unavailable in this build", sec("", banner("", "Unavailable in this build", "Reset and reimport is unavailable during safe import cancellation."))),
        ("Malformed data", sec("", empty("Malformed conflict page; matching results are unknown", "The reply could not be read. No row from it is shown.", btn("Retry", "sm"), role="alert", icon="error"))),
        ("Read data missing", sec("", kv(("Last sync", "Not recorded"), ("Uptime", "Unavailable"), ("Git SHA", "Unknown")), '<p class="t3 sm">Missing values are named. They are never shown as zero or as a dash.</p>')),
        ("Draft preserved across sign-in", banner("", "Your draft was kept", "Entries were kept in this browser, without passwords or tokens.")),
    ))


def s08_legacy(o):
    """Classic, Individual and Team with a light theme. Their arrangement does not change; only colour does."""
    def panel(scope, title):
        bar = ('<div class="legacy-bar"><b>RepoSync</b><span class="on">Dashboard</span><span>Repositories</span><span>Conflicts <span class="count">1</span></span>'
               '<span>Audit Log</span><span>Settings</span><span>Users</span><span>LDAP</span><span>Config</span><span>Setup</span></div>')
        body = "".join([
            sec("Repositories", table(["Repository", "State", "Last successful sync", "Position"], [
                [link("kiln-controller", "#"), pill("idle"), "14:06:12", svn("r4127") + " " + git("9f3c2ab")],
                [link("loom-patterns", "#"), pill("held"), "Not recorded", svn("r750") + " " + git("8a02f5d")],
                [link("greenhouse-telemetry", "#"), pill("paused"), "11:22:03", svn("r612") + " " + git("0be77f1")],
                [link("woodshop-inventory", "#"), pill("disabled"), "9 Nov, 17:40", svn("r88") + " " + git("5aa0c2d")]], cap=f"Repositories, {title}")),
            row(pill("running"), pill("conflict"), pill("error"), pill("unknown")),
            banner("warn", "Sync paused after repeated errors", "Two Git commits are waiting."),
            banner("err", "Failed to trigger sync", "repository has active or unresolved import work"),
            form(field("Poll interval", "300", help="Seconds between checks."), cls="one"),
            row(btn("Save", "primary"), btn("Test connection"), btn("Pause sync…", "warn"), btn("Remove…", "danger")),
        ])
        return f'<div class="legacy-scope" data-scope="{scope}"><p class="sr">{title}</p>{bar}<div class="legacy-body">{body}</div></div>'
    intro = ('<p class="t2">Classic, Individual and Team keep their arrangement, labels and order. This page shows only their colours: on the left as they are today, on the right with the proposed light theme. '
             'Dark stays the default in every layout; light is something a person chooses.</p>')
    return "Existing layouts in light", pg("Existing layouts in light", intro,
                                           cols((6, stack('<h2 class="t2">Dark, as today (approximate)</h2>', panel("dark", "dark"))), (6, stack('<h2 class="t2">Light, proposed</h2>', panel("light", "light")))),
                                           note("The navigation strip is a sketch of Classic's bar, to show the shell colours. Page content on the existing layouts reaches these colours through the colour bridge in the contracts, not through new markup."))
