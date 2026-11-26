#!/usr/bin/env python3
"""Use the actual CLI and Personal worker against disposable SVN, Git and loopback API."""
import argparse
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sqlite3
import errno
import pty
import select
import time
import tomllib
import subprocess
import tempfile
import threading
from urllib.parse import urlsplit


def wizard(cli, output, env, source, api, scoped):
    """Exercise dialoguer through a disposable PTY rather than synthesizing its output."""
    prompts = [
        (b"SVN repository URL", source),
        (b"SVN username", "fixture"),
        (b"Environment variable that holds the SVN password", "FIXTURE_SVN"),
        (b"GitHub API URL", api),
        (b"GitHub repository (owner/repo format)", "test/mirror"),
        (b"Environment variable that holds the GitHub token", "FIXTURE_GIT"),
        (b"Should the GitHub repo be private", ""),
        (b"Git author name", "Fixture"),
        (b"Git author email", "fixture@example.test"),
        (b"SVN username (for echo suppression", ""),
        (b"Poll interval (how often", ""),
        (b"SVN-relative allowed paths", "team/alpha/selected/" if scoped else ""),
    ]
    prompts += ([(b"Optional SVN prefix", "team/alpha/"),
                 (b"Optional blocked SVN path globs", "team/alpha/selected/blocked.txt")]
                if scoped else [(b"Initial import mode", "")])
    prompts += [(b"Write this configuration?", "y")]
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(str(cli), [str(cli), "personal", "init", "--output", str(output)],
                  dict(env, TERM="xterm"))
    all_text, unread = b"", b""
    try:
        for needle, answer in prompts:
            deadline = time.monotonic() + 15
            while needle not in unread:
                assert time.monotonic() < deadline, (needle, all_text.decode(errors="replace"))
                if not select.select([fd], [], [], 0.1)[0]:
                    continue
                chunk = os.read(fd, 8192)
                assert chunk, (needle, all_text.decode(errors="replace"))
                all_text += chunk
                unread += chunk
            after = unread.index(needle) + len(needle)
            unread = unread[after:]
            os.write(fd, (answer + "\r").encode())
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            waited, status = os.waitpid(pid, os.WNOHANG)
            if waited:
                assert os.waitstatus_to_exitcode(status) == 0, all_text.decode(errors="replace")
                return tomllib.loads(output.read_text())
            if select.select([fd], [], [], 0.1)[0]:
                try:
                    all_text += os.read(fd, 8192)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
        raise AssertionError("wizard did not complete")
    finally:
        os.close(fd)
        try:
            os.kill(pid, 9)
            os.waitpid(pid, 0)
        except (ProcessLookupError, ChildProcessError):
            pass


def exercise(root, cli, personal, poll_for_rs60=False):
    prs = []
    commits = {}
    events = []

    class API(BaseHTTPRequestHandler):
        def do_HEAD(self):
            events.append((self.command, self.path))
            self.send_response(200 if self.path == "/repos/test/mirror" else 404)
            self.end_headers()

        def do_GET(self):
            path = urlsplit(self.path).path
            events.append((self.command, path))
            if path == "/repos/test/mirror/pulls":
                obj = prs
            elif path.startswith("/repos/test/mirror/pulls/") and path.endswith("/commits"):
                obj = [commits[prs[int(path.split("/")[-2]) - 1]["merge_commit_sha"]]]
            elif path.startswith("/repos/test/mirror/commits/"):
                obj = commits.get(path.split("/")[-1])
            else:
                self.send_error(404)
                return
            payload = json.dumps(obj).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_POST(self):
            events.append((self.command, urlsplit(self.path).path))
            self.send_error(405)

        def log_message(self, *_):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), API) as server:
        threading.Thread(target=server.serve_forever, daemon=True).start()
        env = dict(os.environ, HOME=str(root), GIT_CONFIG_GLOBAL=os.devnull,
                   GIT_CONFIG_NOSYSTEM="1", NO_COLOR="1", FIXTURE_SVN="fixture",
                   FIXTURE_GIT="fixture", NO_PROXY="127.0.0.1", no_proxy="127.0.0.1",
                   GIT_AUTHOR_NAME="Fixture", GIT_AUTHOR_EMAIL="fixture@example.test",
                   GIT_COMMITTER_NAME="Fixture", GIT_COMMITTER_EMAIL="fixture@example.test")

        def run(*args, cwd=root, ok=True):
            result = subprocess.run([str(a) for a in args], cwd=cwd, env=env, text=True,
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=90)
            assert (result.returncode == 0) == ok, (args, result.returncode, result.stdout)
            return result.stdout

        svn, wc, remote, cfg, data = (root/"svn", root/"wc", root/"test"/"mirror.git",
                                      root/"personal.toml", root/"data")
        remote.parent.mkdir()
        run("svnadmin", "create", svn)
        run("svn", "checkout", svn.as_uri(), wc)
        excluded_message = "EXCLUDED-ONLY-HISTORICAL-SECRET"
        historical_subject = "PRIVATE-MIXED-FULL-SUBJECT"
        historical_body = "PRIVATE-MIXED-FULL-BODY"
        incremental_subject = "PRIVATE-MIXED-INCREMENTAL-SUBJECT"
        incremental_body = "PRIVATE-MIXED-INCREMENTAL-BODY"
        private_author = "private-source-mixed-actor"
        mapped_name = "PRIVATE-SVN-MAPPED-NAME"
        mapped_email = "private-svn-mapped-email@example.test"
        mapping = root/"authors.toml"
        mapping.write_text(f'[authors]\n{private_author} = {{ name = "{mapped_name}", email = "{mapped_email}" }}\n')
        (wc/"team/bravo").mkdir(parents=True)
        (wc/"team/bravo/other.txt").write_text("never mirror")
        run("svn", "add", "team", cwd=wc)
        run("svn", "commit", "-m", excluded_message + "-1", "--username", "excluded-actor", cwd=wc)
        (wc/"team/alpha/selected").mkdir(parents=True)
        (wc/"team/alpha/selected/note.txt").write_text("r2")
        (wc/"team/alpha/selected/blocked.txt").write_text("private")
        run("svn", "add", "team/alpha", cwd=wc)
        run("svn", "commit", "-m", "initial selected effect", "--username", "fixture", cwd=wc)
        (wc/"team/bravo/other.txt").write_text("r3 excluded")
        run("svn", "commit", "-m", excluded_message + "-3", "--username", "excluded-actor", cwd=wc)
        (wc/"team/alpha/selected/note.txt").write_text("r4")
        (wc/"team/bravo/other.txt").write_text("full mixed excluded bytes")
        run("svn", "commit", "-m", historical_subject + "\n\n" + historical_body,
            "--username", private_author, cwd=wc)
        (wc/"team/bravo/other.txt").write_text("r5 excluded")
        run("svn", "commit", "-m", excluded_message + "-5", "--username", "excluded-actor", cwd=wc)
        run("git", "init", "--bare", "--initial-branch=main", remote)
        base = f"""
[personal]
data_dir = {json.dumps(str(data))}
[svn]
url = "{svn.as_uri()}"
username = "fixture"
password_env = "FIXTURE_SVN"
[github]
repo = "test/mirror"
api_url = "http://127.0.0.1:{server.server_port}"
git_base_url = "{root.as_uri()}"
token_env = "FIXTURE_GIT"
auto_create = false
[developer]
name = "Fixture"
email = "fixture@example.test"
svn_username = "fixture"
[identity]
mapping_file = {json.dumps(str(mapping))}
[commit_format]
svn_to_git = "{{original_message}}\\n\\nSVN-Revision: r{{svn_rev}}\\nSVN-Author: {{svn_author}}\\nSVN-Date: {{svn_date}}\\nSync-Marker: [reposync]"
[options]
"""
        scope = """
projected_history = true
allowed_paths = ["team/alpha/selected/"]
blocked_patterns = ["team/alpha/selected/blocked.txt"]
path_rewrite_rules = [{ strip_prefix = "team/alpha/" }]
"""
        api = f"http://127.0.0.1:{server.server_port}"
        generated = wizard(cli, root/"wizard.toml", env, svn.as_uri(), api, True)
        assert generated["options"]["projected_history"] is True
        assert generated["options"]["allowed_paths"] == ["team/alpha/selected/"]
        assert generated["options"]["blocked_patterns"] == ["team/alpha/selected/blocked.txt"]
        assert generated["options"]["path_rewrite_rules"] == [{"strip_prefix":"team/alpha/"}]
        legacy = wizard(cli, root/"wizard-legacy.toml", env, svn.as_uri(), api, False)
        assert "allowed_paths" not in legacy["options"]
        assert "projected_history" not in legacy["options"]
        if poll_for_rs60:
            # Two disjoint selected namespaces under one rewrite prefix.
            scope = scope.replace(
                'allowed_paths = ["team/alpha/selected/"]',
                'allowed_paths = ["team/alpha/selected/", "team/alpha/docs/"]',
            )
            base = base.replace('[personal]\n', '[personal]\npoll_interval_secs = 1\n')
        cfg.write_text(base+scope)

        def op(*args, **kw):
            return run(cli, "personal", "--personal-config", cfg, *args, **kw)

        def ps(*args, **kw):
            return run(personal, "--config", cfg, *args, **kw)

        def git(*args):
            return run("git", "--git-dir", remote, *args).strip()

        def sql(query):
            with sqlite3.connect(data/"personal.db") as conn:
                return conn.execute(query).fetchall()

        assert "refusing bootstrap" in ps("sync", ok=False)
        assert not (data/"personal.db").exists()
        assert "snapshot refused" in op("import", "--snapshot", ok=False)
        assert "unverified" in ps("sync", ok=False)
        assert not events, events
        op("import", "--full")
        assert "Path policy ACTIVE" in op("status")
        assert "requires a fresh mirror" in op("import", "--full", ok=False)
        revision_shas = git("rev-list", "--reverse", "main").splitlines()
        assert len(revision_shas) == 2, revision_shas
        assert excluded_message not in git("log", "--format=%B", "main")
        def assert_scoped_metadata(sha, revision, secrets):
            metadata = git("show", "-s", "--format=%an%n%ae%n%cn%n%ce%n%s%n%b", sha)
            assert metadata.splitlines()[:5] == [
                "Fixture", "fixture@example.test", "Fixture", "fixture@example.test",
                f"Mirror selected SVN changes at r{revision}",
            ], metadata
            for secret in [*secrets, private_author, mapped_name, mapped_email, "SVN-Author:", "SVN-Date:"]:
                assert secret not in metadata, (secret, metadata)
            assert f"SVN-Revision: r{revision}" in metadata and "Sync-Marker: [reposync]" in metadata
        assert_scoped_metadata(revision_shas[1], 4, [historical_subject, historical_body, "excluded-actor"])
        assert sql("SELECT value FROM watermarks WHERE source='svn_rev'") == [("5",)]
        assert sql("SELECT svn_rev FROM commit_map WHERE git_sha IS NULL ORDER BY svn_rev") == [(1,), (3,), (5,)]
        for rev, content in zip(revision_shas, ["r2", "r4"]):
            assert git("ls-tree", "-r", "--name-only", rev) == "selected/note.txt"
            assert git("show", f"{rev}:selected/note.txt") == content

        @contextmanager
        def poll(label):
            log = root / f"poll-{label}.log"
            with log.open("wb") as out:
                process = subprocess.Popen(
                    [str(cli), "personal", "--personal-config", str(cfg), "start", "--foreground"],
                    cwd=root, env=env, stdout=out, stderr=subprocess.STDOUT,
                )
                try:
                    def wait(predicate):
                        deadline = time.monotonic() + 25
                        while time.monotonic() < deadline:
                            assert process.poll() is None, log.read_text()
                            if predicate():
                                return
                            time.sleep(0.10)
                        raise AssertionError(f"{label} poll timed out: {log.read_text()}")
                    wait(lambda: "Daemon started" in log.read_text())
                    yield wait
                finally:
                    if process.poll() is None:
                        process.terminate()
                        try:
                            process.wait(timeout=12)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()
                            raise
                    assert process.returncode == 0, log.read_text()

        def commit_selected_r7():
            (wc/"team/alpha/selected/note.txt").write_text("r7 selected")
            (wc/"team/bravo/other.txt").write_text("incremental mixed excluded bytes")
            if poll_for_rs60:
                (wc/"team/alpha/docs").mkdir()
                (wc/"team/alpha/docs/guide.txt").write_text("selected shared doc")
                run("svn", "add", "team/alpha/docs", cwd=wc)
            run("svn", "commit", "-m", incremental_subject + "\n\n" + incremental_body,
                "--username", private_author, cwd=wc)

        (wc/"team/bravo/other.txt").write_text("r6 excluded")
        run("svn", "commit", "-m", "excluded only", cwd=wc)
        if poll_for_rs60:
            refs = git("for-each-ref")
            marks = sql("SELECT source,value FROM watermarks WHERE source != 'svn_rev' ORDER BY source")
            def durable():
                return (
                    sql("SELECT svn_rev,git_sha FROM commit_map ORDER BY svn_rev,git_sha"),
                    sql("SELECT count(*) FROM audit_log WHERE action='svn_to_git_filtered'"),
                    sql("SELECT source,value FROM watermarks ORDER BY source"),
                    sql("""SELECT outcome,target_git_sha,target_svn_rev
                           FROM canonical_mapping_outcomes
                           WHERE direction='svn_to_git' AND source_key='svn:6'"""),
                )
            with poll("excluded") as wait:
                wait(lambda: sql("SELECT value FROM watermarks WHERE source='svn_rev'") == [("6",)])
                filtered = durable()
                assert [(rev, sha) for rev, sha in filtered[0] if rev == 6] == [(6, None)]
                assert filtered[3] == [("intentionally_filtered_no_target", None, None)]
                assert sql("""SELECT count(*) FROM audit_log
                              WHERE action='svn_to_git_filtered' AND svn_rev=6""") == [(1,)]
                assert sql("SELECT source,value FROM watermarks WHERE source != 'svn_rev' ORDER BY source") == marks
                assert git("for-each-ref") == refs
            with poll("reopen") as wait:
                # The merged-PR GET occurs downstream of a completed SVN phase.
                before = events.count(("GET", "/repos/test/mirror/pulls"))
                wait(lambda: events.count(("GET", "/repos/test/mirror/pulls")) >= before + 2)
                assert durable() == filtered and git("for-each-ref") == refs
                commit_selected_r7()
                wait(lambda: sql("SELECT value FROM watermarks WHERE source='svn_rev'") == [("7",)])
                assert git("rev-list", "--count", "main") == "3"
                assert git("show", "main:docs/guide.txt") == "selected shared doc"
                assert not any(method == "POST" for method, _ in events), events
            print("PASS RS60-SC11: personal start polled r6 no-target, reopened quietly, and selected both prefixes at r7", flush=True)
        else:
            commit_selected_r7()
            ps("sync")
        assert git("show", "main:selected/note.txt") == "r7 selected"
        expected_tree = "docs/guide.txt\nselected/note.txt" if poll_for_rs60 else "selected/note.txt"
        assert git("ls-tree", "-r", "--name-only", "main") == expected_tree
        assert_scoped_metadata("main", 7, [incremental_subject, incremental_body])
        local = root/"git-wc"
        run("git", "clone", remote, local)
        parent = run("git", "rev-parse", "HEAD", cwd=local).strip()
        (local/"selected/note.txt").write_text("Git PR edit")
        run("git", "commit", "-am", "selected PR", cwd=local)
        run("git", "push", "origin", "main", cwd=local)
        sha = run("git", "rev-parse", "HEAD", cwd=local).strip()
        actor = {"name": "Fixture", "email": "fixture@example.test"}
        prs.append({"number":1, "title":"selected PR", "html_url":"http://127.0.0.1/", "state":"closed",
                    "head":{"ref":"topic", "sha":sha}, "base":{"ref":"main", "sha":parent},
                    "merged":True, "merge_commit_sha":sha, "merged_at":"2026-11-26T00:00:00Z"})
        commits[sha] = {"sha":sha, "commit":{"message":"selected PR", "author":actor, "committer":actor},
                        "parents":[{"sha":parent}]}
        ps("sync")
        assert run("svnlook", "youngest", svn).strip() == "8"
        assert run("svnlook", "cat", svn, "team/alpha/selected/note.txt").strip() == "Git PR edit"
        assert run("svnlook", "cat", svn, "team/bravo/other.txt").strip() == "incremental mixed excluded bytes"
        assert "team/alpha/selected/note.txt" in run("svnlook", "changed", "-r", "8", svn)
        ps("sync")
        assert run("svnlook", "youngest", svn).strip() == "8"
        assert git("rev-parse", "main") == sha

        # Genuine two-parent PR publication: the API may return side-branch
        # commits, but only the actual accepted main commit authorizes writes.
        run("git", "switch", "-c", "next-topic", cwd=local)
        (local/"selected/second.txt").write_text("from real merge")
        run("git", "add", "selected/second.txt", cwd=local)
        run("git", "commit", "-m", "topic change", cwd=local)
        run("git", "switch", "main", cwd=local)
        run("git", "merge", "--no-ff", "-m", "merge next topic", "next-topic", cwd=local)
        run("git", "push", "origin", "main", cwd=local)
        merge_sha = run("git", "rev-parse", "HEAD", cwd=local).strip()
        parents = run("git", "log", "-1", "--format=%P", cwd=local).strip().split()
        assert len(parents) == 2 and parents[0] == sha
        prs.append({"number":2, "title":"two-parent PR", "html_url":"http://127.0.0.1/", "state":"closed",
                    "head":{"ref":"next-topic", "sha":parents[1]}, "base":{"ref":"main", "sha":sha},
                    "merged":True, "merge_commit_sha":merge_sha, "merged_at":"2099-01-01T00:00:00Z"})
        commits[merge_sha] = {"sha":merge_sha, "commit":{"message":"two-parent PR", "author":actor, "committer":actor},
                              "parents":[{"sha":p} for p in parents]}
        ps("sync")
        assert run("svnlook", "cat", svn, "team/alpha/selected/second.txt").strip() == "from real merge"
        ps("sync")
        assert run("svnlook", "youngest", svn).strip() == "9"

        cfg.write_text(base+scope.replace('team/alpha/selected/', 'team/alpha/changed/'))
        before = sql("SELECT source,value FROM watermarks ORDER BY source")
        assert "re-import" in op("doctor")
        assert "diverges" in op("doctor")
        assert "changed" in ps("sync", ok=False)
        assert before == sql("SELECT source,value FROM watermarks ORDER BY source")
        assert run("svnlook", "youngest", svn).strip() == "9"
        cfg.write_text(base)
        assert "changed" in ps("sync", ok=False)
        assert git("rev-parse", "main") == merge_sha

        cfg.write_text(base+scope)
        (local/"out-of-scope.txt").write_text("wrong Git coordinate")
        run("git", "add", "out-of-scope.txt", cwd=local)
        run("git", "commit", "-m", "foreign path", cwd=local)
        run("git", "push", "origin", "main", cwd=local)
        foreign_sha = git("rev-parse", "main")
        diagnosed = op("doctor")
        assert "Git remote tree diverges" in diagnosed and "out-of-scope.txt" in diagnosed, diagnosed
        assert "Git tree diverges" in ps("sync", ok=False)
        assert before == sql("SELECT source,value FROM watermarks ORDER BY source")
        assert run("svnlook", "youngest", svn).strip() == "9"

        # No-policy legacy full import still sees both teams and does not
        # create a projected-policy marker or reposition any existing mirror.
        legacy_root = root/"legacy"
        legacy_remote = legacy_root/"test"/"mirror.git"
        legacy_remote.parent.mkdir(parents=True)
        run("git", "init", "--bare", "--initial-branch=main", legacy_remote)
        legacy_cfg = base.replace(json.dumps(str(data)), json.dumps(str(root/"legacy-data")))
        legacy_cfg = legacy_cfg.replace(root.as_uri() + '"', legacy_root.as_uri() + '"')
        cfg.write_text(legacy_cfg)
        op("import", "--full")
        tree = run("git", "--git-dir", legacy_remote, "ls-tree", "-r", "--name-only", "main")
        assert "team/bravo/other.txt" in tree
        assert "team/alpha/selected/blocked.txt" in tree
        legacy_history = run("git", "--git-dir", legacy_remote, "log", "--format=%an %ae %B", "main")
        for value in [historical_subject, historical_body, incremental_subject, incremental_body,
                      f"SVN-Author: {private_author}", mapped_name, mapped_email]:
            assert value in legacy_history, (value, legacy_history)
        assert "unrestricted legacy mode" in op("status")
        assert git("rev-parse", "main") == foreign_sha
        # Prove an actual interrupted publication cannot activate the policy:
        # the receiving disposable Git repository refuses the sole push.
        interrupted = root/"interrupted"
        interrupted_remote = interrupted/"test"/"mirror.git"
        interrupted_remote.parent.mkdir(parents=True)
        run("git", "init", "--bare", "--initial-branch=main", interrupted_remote)
        hook = interrupted_remote/"hooks/pre-receive"
        hook.write_text("#!/bin/sh\nexit 1\n")
        hook.chmod(0o755)
        interrupted_data = root/"interrupted-data"
        interrupted_cfg = base.replace(json.dumps(str(data)), json.dumps(str(interrupted_data)))
        interrupted_cfg = interrupted_cfg.replace(root.as_uri() + '"', interrupted.as_uri() + '"')
        cfg.write_text(interrupted_cfg+scope)
        op("import", "--full", ok=False)
        assert run("git", "--git-dir", interrupted_remote, "for-each-ref", "refs/heads") == ""
        with sqlite3.connect(interrupted_data/"personal.db") as conn:
            assert conn.execute("SELECT value FROM kv_state WHERE key = 'personal_projected_history_v1'").fetchone() is None
        refused = ps("sync", ok=False)
        assert "unverified" in refused or "reconciliation" in refused or "writer admission refused" in refused, refused
        assert run("svnlook", "youngest", svn).strip() == "9"
        # A normal copied nested branch begins at its proved incarnation;
        # imported history does not disclose the predecessor branch log.
        copied = root/"copied-svn"
        cpwc = root/"copied-wc"
        copies_git = root/"copied-git"
        cpremote = copies_git/"test"/"mirror.git"
        cpd = root/"copied-data"
        cpremote.parent.mkdir(parents=True)
        run("git", "init", "--bare", "--initial-branch=main", cpremote)
        run("svnadmin", "create", copied)
        run("svn", "checkout", copied.as_uri(), cpwc)
        selected = cpwc/"old-source/team/alpha/selected/note.txt"
        selected.parent.mkdir(parents=True)
        selected.write_text("original copy content")
        (cpwc/"branches").mkdir()
        run("svn", "add", "old-source", "branches", cwd=cpwc)
        run("svn", "commit", "-m", "PREDECESSOR-PRIVATE-HISTORY", cwd=cpwc)
        cpbranch = copied.as_uri() + "/branches/copy"
        run("svn", "copy", copied.as_uri()+"/old-source", cpbranch, "-m", "establish branch")
        clone_cfg = base.replace(json.dumps(str(data)), json.dumps(str(cpd)))
        clone_cfg = clone_cfg.replace(root.as_uri() + '"', copies_git.as_uri() + '"')
        clone_cfg = clone_cfg.replace(svn.as_uri(), cpbranch)
        cfg.write_text(clone_cfg+scope)
        op("import", "--full")
        cpcommits = lambda: run("git", "--git-dir", cpremote, "rev-list", "main").splitlines()
        assert len(cpcommits()) == 1
        assert "PREDECESSOR-PRIVATE-HISTORY" not in run("git", "--git-dir", cpremote, "log", "main")
        assert run("git", "--git-dir", cpremote, "show", "main:selected/note.txt") == "original copy content"
        run("svn", "update", cwd=cpwc)
        notefile = cpwc/"branches/copy/team/alpha/selected/note.txt"
        notefile.write_text("copied branch increment")
        run("svn", "commit", "-m", "copied branch increment", cwd=cpwc)
        prs.clear()
        ps("sync")
        assert len(cpcommits()) == 2
        assert run("git", "--git-dir", cpremote, "show", "main:selected/note.txt") == "copied branch increment"

        # Pending selected work on both sides must hold before copying a
        # tree, writing a new local commit, starting an intent, or applying a PR.
        cpgitwc = root/"copied-git-wc"
        bridge = cpd/"git-repo"
        run("git", "clone", cpremote, cpgitwc)
        bridge_before = run("git", "rev-parse", "HEAD", cwd=bridge).strip()
        (cpgitwc/"selected/note.txt").write_text("concurrent Git edit")
        run("git", "commit", "-am", "concurrent Git effect", cwd=cpgitwc)
        run("git", "push", "origin", "main", cwd=cpgitwc)
        remote_before = cpcommits()
        notefile.write_text("concurrent SVN edit")
        run("svn", "commit", "-m", "concurrent SVN effect", cwd=cpwc)

        def copystate():
            with sqlite3.connect(cpd/"personal.db") as conn:
                return {name: conn.execute(query).fetchall() for name, query in {
                    "frontiers": "SELECT source,value FROM watermarks ORDER BY source",
                    "journal": "SELECT COUNT(*) FROM kv_state WHERE key LIKE 'svn_to_git_push_v1:%'",
                    "maps": "SELECT COUNT(*) FROM commit_map",
                }.items()}

        before_hold = copystate()
        remote_youngest = run("svnlook", "youngest", copied).strip()
        observed_events = events.copy()
        held = ps("sync", ok=False)
        assert "simultaneous selected SVN and Git changes are held" in held, held
        assert run("git", "rev-parse", "HEAD", cwd=bridge).strip() == bridge_before
        assert run("git", "status", "--porcelain", cwd=bridge) == ""
        assert (bridge/"selected/note.txt").read_text() == "copied branch increment"
        assert cpcommits() == remote_before
        assert run("svnlook", "youngest", copied).strip() == remote_youngest == "4"
        assert copystate() == before_hold and events == observed_events
        server.shutdown()
        print("PASS " + json.dumps({
            "wizard_scoped_and_default": True, "projected_history": revision_shas,
            "svn_forward": [6, 7], "reverse_linear": {"git": sha, "svn": 8},
            "reverse_merge": {"git": merge_sha, "svn": 9},
            "historical_no_target": [1, 3, 5], "mixed_metadata_redacted_at": [4, 7], "copied_branch_initial_r2_and_increment_r3": True,
            "simultaneous_selected_changes_held_before_mutation": True,
            "legacy_full_import": True, "no_repeat_writes": True,
            "changed_or_removed_policy_held": True, "remote_path_divergence_held": True,
            "interrupted_publication_did_not_activate_reverse": True,
        }, sort_keys=True))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--personal", type=Path, required=True)
    parser.add_argument("--poll-for-rs60", action="store_true", help="also qualify real Personal daemon start/restart for #60")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="reposync-personal-rewrite-59-") as root:
        exercise(Path(root), args.cli.resolve(), args.personal.resolve(), args.poll_for_rs60)
