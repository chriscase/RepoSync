#!/usr/bin/env python3
"""Black-box #55. All repositories, worktrees, HOME, DB and HTTP are disposable."""
import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import time
from contextlib import contextmanager
import sqlite3
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


@contextmanager
def loopback_svn(root):
    # Defaults are read-only anonymous SVN, no credentials or ACL changes.
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    process = subprocess.Popen(["svnserve","--daemon","--foreground","--listen-host","127.0.0.1",
                                "--listen-port",str(port),"--root",str(root)])
    try:
        for _ in range(100):
            if process.poll() is not None:
                raise RuntimeError("loopback svnserve failed to start")
            try:
                with socket.create_connection(("127.0.0.1",port), timeout=0.1):
                    break
            except OSError:
                time.sleep(0.02)
        else:
            raise RuntimeError("loopback svnserve did not accept a connection")
        yield f"svn://127.0.0.1:{port}/svn"
    finally:
        process.terminate()
        process.wait(timeout=10)


def exercise(root, personal, cli, network=None, hidden=None, nested=False, replacement=False):
    events = []

    class API(BaseHTTPRequestHandler):
        def do_HEAD(self):
            events.append((self.command, self.path))
            self.send_response(200 if self.path == "/repos/test/mirror" else 404)
            self.end_headers()

        def log_message(self, *_):
            pass

        def unexpected(self):
            events.append((self.command, self.path))
            self.send_error(405)

        do_GET = do_POST = do_PATCH = do_DELETE = unexpected

    server = ThreadingHTTPServer(("127.0.0.1", 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    env = dict(os.environ, HOME=str(root), GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
               GIT_AUTHOR_NAME="Fixture", GIT_AUTHOR_EMAIL="fixture@example.test",
               GIT_COMMITTER_NAME="Fixture", GIT_COMMITTER_EMAIL="fixture@example.test",
               FIXTURE_SVN="fixture", FIXTURE_GITHUB="fixture", NO_COLOR="1",
               NO_PROXY="127.0.0.1,localhost", no_proxy="127.0.0.1,localhost")
    def run(*args, cwd=root, ok=True):
        r = subprocess.run(list(map(str, args)), cwd=cwd, env=env, text=True,
                           stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=120)
        assert (r.returncode == 0) == ok, (args, r.returncode, r.stdout)
        return r.stdout

    data, remote, wc = root/"data", root/"mirror.git", root/"wc"
    data.mkdir()
    repo = root/"svn"
    svn = shutil.which("svn")
    run("svnadmin", "create", repo)
    if hidden:
        # Synthetic fixture policy installed before serving this disposable
        # repository. Neither the access profile nor actor changes mid-cycle.
        (repo/"conf"/"svnserve.conf").write_text(
            "[general]\nanon-access = none\nauth-access = read\npassword-db = passwd\nauthz-db = authz\n")
        (repo/"conf"/"passwd").write_text("[users]\nfixture = fixture\n")
        (repo/"conf"/"authz").write_text("[svn:/]\nfixture = r\n[svn:/foo/denied]\nfixture =\n")
    run(svn, "checkout", repo.as_uri(), wc)
    if nested:
        checkout = wc
        network += "/trunk"
        (checkout/"trunk").mkdir()
        (checkout/"elsewhere").mkdir()
        (checkout/"elsewhere"/"unrelated").write_text("elsewhere")
        wc = checkout/"trunk"
    (wc/"foo").mkdir()
    (wc/"foo"/"seed").write_text("seed")
    if nested:
        run(svn, "add", "trunk", "elsewhere", cwd=checkout)
        run(svn, "commit", "-m", "initial selected file", cwd=checkout)
    else:
        run(svn, "add", "foo", cwd=wc)
        run(svn, "commit", "-m", "initial selected file", cwd=wc)
    run("git", "init", "--bare", "--initial-branch=main", remote)
    run("git", "clone", remote, data/"git-repo")
    conf = root/"personal.toml"
    base = f"""
[personal]
data_dir = {json.dumps(str(data))}
[svn]
url = "{network or repo.as_uri()}"
username = "fixture"
password_env = "FIXTURE_SVN"
[github]
repo = "test/mirror"
api_url = "http://127.0.0.1:{server.server_port}"
token_env = "FIXTURE_GITHUB"
auto_create = false
[developer]
name = "Fixture"
email = "fixture@example.test"
svn_username = "fixture"
[options]
ignore_patterns = ["foo/*.tmp"]
"""
    conf.write_text(base)
    def ps(*args, **kw):
        return run(personal, "--config", conf, *args, **kw)
    def op(*args, **kw):
        return run(cli, "personal", "--personal-config", conf, *args, **kw)
    def remote_ref():
        return run("git", "--git-dir", remote, "rev-parse", "main").strip()
    def db(query, args=()):
        with sqlite3.connect(data/"personal.db") as conn:
            return conn.execute(query, args).fetchall()

    chosen = "foo/"
    profile = 'network_directory_profile = "apache_svnserve_wc31"\n' if network else ''
    def configure(prefix):
        conf.write_text(base+profile+'allowed_paths = '+json.dumps([prefix])+'\n')

    configure(chosen)
    assert "refusing bootstrap" in ps("sync", ok=False)
    assert not (data/"personal.db").exists()
    conf.write_text(base)
    ps("import", "--snapshot")
    tip = remote_ref()
    assert events == [("HEAD", "/repos/test/mirror")], events
    events.clear()
    configure(chosen)
    with sqlite3.connect(data/"personal.db") as conn:
        conn.execute("DELETE FROM watermarks WHERE source='svn_rev'")
    assert "existing proven SVN and Git checkpoint" in ps("sync", ok=False)
    assert tip == remote_ref() and not events
    with sqlite3.connect(data/"personal.db") as conn:
        conn.execute("INSERT INTO watermarks(source,value,updated_at) VALUES ('svn_rev','1','fixture')")
    assert "unavailable" in op("doctor")
    assert "unrestricted import refused" in ps("import", "--full", ok=False)
    assert "unrestricted import refused" in op("import", "--snapshot", ok=False)

    if replacement:
        # An older, real, positive SVN effect is within the next batch, but
        # the live branch URL now points to a different SVN node incarnation.
        (wc/"foo"/"seed").write_text("pending effect from retired incarnation")
        run(svn, "commit", "-m", "pending old branch edit", cwd=wc)
        temporary = root/"elsewhere-content"
        for n in range(33):
            temporary.write_text(f"unrelated SVN revision {n}")
            run("svnmucc", "-m", "unrelated global revision",
                "put", temporary, repo.as_uri()+"/elsewhere/unrelated")
        run("svnmucc", "-m", "replace trunk with a fresh copy of the old seed",
            "rm", repo.as_uri()+"/trunk", "cp", "1", repo.as_uri()+"/trunk", repo.as_uri()+"/trunk")
        result = ps("sync", ok=False)
        assert "current nested SVN incarnation differs from the Personal checkpoint" in result, result
        assert remote_ref() == tip and not events
        assert db("SELECT value FROM watermarks WHERE source='svn_rev'") == [("1",)]
        assert db("SELECT count(*) FROM canonical_mapping_outcomes") == [(0,)]
        assert (data/"git-repo"/"foo"/"seed").read_text() == "seed"
        server.shutdown()
        server.server_close()
        print("PASS nested svn:// replaced beyond batch: prior real positive revision held with its cursor and Git tree preserved.")
        return
    if nested:
        # An ordinary global revision outside the configured SVN branch.
        # The scoped checkout must cover its number without treating the
        # empty branch log as either proof or a missing-content failure.
        (checkout/"elsewhere"/"unrelated").write_text("changed outside the trunk branch")
        run(svn, "commit", "-m", "outside the configured branch", cwd=checkout/"elsewhere")
        (wc/"bar").mkdir()
        (wc/"bar"/"baz.txt").write_text("excluded")
        run(svn, "add", "bar", cwd=wc)
    else:
        (wc/"bar").mkdir()
        (wc/"bar"/"baz.txt").write_text("excluded")
        run(svn, "add", "bar", cwd=wc)
        run(svn, "commit", "-m", "outside foo", cwd=wc)
    if hidden:
        # A visible outside revision, then two revisions whose hidden selected
        # effect cancels, then a visible selected edit. A branch log or a
        # comparison only against HEAD must not skip the hidden interval.
        target = wc/"foo"/"denied"
        if hidden == "directory":
            target.mkdir()
            (target/"child").write_text("not readable by the fixture reader")
        else:
            target.write_text("not readable by the fixture reader")
        run(svn, "add", "foo/denied", cwd=wc)
        if hidden == "directory":
            (wc/"bar"/"baz.txt").write_text("visible change in the same revision")
        run(svn, "commit", "-m", "selected path unseen by fixture reader", cwd=wc)
        run(svn, "rm", "foo/denied", cwd=wc)
        run(svn, "commit", "-m", "remove unseen path", cwd=wc)
        (wc/"foo"/"seed").write_text("later visible change")
        run(svn, "commit", "-m", "selected visible file", cwd=wc)
        result = ps("sync", ok=False)
        assert "failed to project personal SVN r3" in result, result
        assert "network SVN cannot prove complete visibility of the selected directory" in result, result
        assert remote_ref() == tip and not events
        assert db("SELECT value FROM watermarks WHERE source='svn_rev'") == [("2",)]
        assert db("SELECT source_key FROM canonical_mapping_outcomes ORDER BY source_key") == [("svn:2",)]
        assert db("SELECT count(*) FROM pr_sync_log") == [(0,)]
        assert not (data/"git-repo"/"foo"/"denied").exists()
        assert "filtered, no commit" in op("log")
        # Request the hidden path itself: a listing that cannot see the
        # selected root must not cause it to be treated as absent.
        configure("foo/denied")
        again = ps("sync", ok=False)
        assert "cannot prove absence of the selected path from its pinned SVN parent" in again, again
        assert remote_ref() == tip and not events, again
        assert db("SELECT value FROM watermarks WHERE source='svn_rev'") == [("2",)]
        server.shutdown()
        server.server_close()
        print(f"PASS loopback svn:// denied {hidden}: preceding filtered revision checkpointed; hidden and later revisions held without publication.")
        return
    assert "SVN→Git: 0 commits" in ps("sync")
    assert tip == remote_ref()
    scope = "__reposync_personal__"
    rows = "SELECT git_sha FROM commit_map WHERE repo_id=? AND direction='svn_to_git' AND svn_rev=?"
    assert db(rows, (scope, 2)) == [(None,)]
    evidence = db("SELECT outcome,target_git_sha,target_svn_rev,policy_snapshot FROM canonical_mapping_outcomes WHERE scope_uuid=? AND direction='svn_to_git' AND source_key='svn:2'", (scope,))
    assert len(evidence) == 1 and evidence[0][:3] == ("intentionally_filtered_no_target", None, None), evidence
    assert json.loads(evidence[0][3])["allowed_paths"] == [chosen]
    marks = "SELECT source,value FROM watermarks ORDER BY source"
    bookmarks = db(marks)
    assert dict(bookmarks)["svn_rev"] == "2" and dict(bookmarks)["git_sha"] == tip
    assert "filtered, no commit" in op("status") and "filtered, no commit" in op("log")
    assert "SVN→Git: 0 commits" in ps("sync")
    assert remote_ref() == tip and db(marks) == bookmarks

    conf.write_text(base)
    for command in [lambda: ps("sync", ok=False), lambda: ps("import", "--snapshot", ok=False),
                    lambda: op("import", "--full", ok=False)]:
        assert "restore allowed_paths" in command()
    assert db(marks) == bookmarks and tip == remote_ref() and not events

    # Force an actual receipt lookup, not just HEAD <= watermark. Widen the
    # scope so recomputing the old decision would now incorrectly publish bar.
    with sqlite3.connect(data/"personal.db") as conn:
        conn.execute("UPDATE watermarks SET value='1' WHERE source='svn_rev'")
    configure('bar/')
    wrapper = root/"shim"
    wrapper.mkdir()
    trace = root/"svn-verbs"
    (wrapper/"svn").write_text(f"""#!/bin/sh
printf "%s\\n" "$1" >> "{trace}"
if [ "$1" = update ] && [ "$FIXTURE_SHORT_UPDATE" = 1 ]; then
  printf "Updating 'foo':\\n"
  exit 0
fi
exec "{svn}" "$@"
""")
    (wrapper/"svn").chmod(0o755)
    env["PATH"] = str(wrapper)+os.pathsep+env["PATH"]
    assert "SVN→Git: 0 commits" in ps("sync")
    verbs = trace.read_text().splitlines()
    assert "log" in verbs and not {"export", "diff", "proplist", "list", "checkout"}.intersection(verbs), verbs
    assert remote_ref() == tip and db(marks) == bookmarks and db(rows,(scope,2)) == [(None,)]
    assert db("SELECT count(*) FROM audit_log WHERE action='svn_to_git_filtered'") == [(1,)]

    configure(chosen)
    (wc/"foo"/"seed").write_text("changed")
    (wc/"foo"/"file.tmp").write_text("excluded by FilePolicy")
    (wc/"bar"/"baz.txt").write_text("also changed")
    run(svn, "add", "foo/file.tmp", cwd=wc)
    run(svn, "commit", "-m", "mixed selected and excluded changes", cwd=wc)
    # A naked NULL must not hide this genuine positive.
    with sqlite3.connect(data/"personal.db") as conn:
        conn.execute("INSERT INTO commit_map(repo_id,svn_rev,git_sha,direction,synced_at) VALUES (?,3,NULL,'svn_to_git','fixture')", (scope,))
    assert "SVN→Git: 1 commits" in ps("sync")
    applied = remote_ref()
    assert applied != tip and run("git","--git-dir",remote,"rev-parse","main^").strip() == tip
    tree = run("git","--git-dir",remote,"ls-tree","-r","--name-only","main").splitlines()
    assert tree == ["foo/seed"], tree
    assert run("git","--git-dir",remote,"show","main:foo/seed") == "changed"
    assert (applied,) in db(rows,(scope,3))
    assert applied[:8] in op("log") and applied[:8] in op("status")
    assert "SVN→Git: 0 commits" in ps("sync") and remote_ref() == applied
    # A deleted prefix is a positive selected deletion; a sibling named
    # foo-other is not selected, even when foo no longer exists.
    run(svn, "update", cwd=wc)
    run(svn, "rm", "foo", cwd=wc)
    run(svn, "commit", "-m", "remove selected subtree", cwd=wc)
    if network:
        # An abbreviated but zero-status tool completion must not count as
        # confirmed named-child absence or advance any durable cursor.
        env["FIXTURE_SHORT_UPDATE"] = "1"
        short_result = ps("sync", ok=False)
        del env["FIXTURE_SHORT_UPDATE"]
        assert "named-child SVN update did not complete" in short_result, short_result
        assert remote_ref() == applied and dict(db(marks))["svn_rev"] == "3"
    assert "SVN→Git: 1 commits" in ps("sync")
    deletion = remote_ref()
    assert deletion != applied and not run("git","--git-dir",remote,"ls-tree","-r","--name-only","main").strip()
    (wc/"foo-other").mkdir()
    (wc/"foo-other"/"keep").write_text("sibling")
    run(svn, "add", "foo-other", cwd=wc)
    run(svn, "commit", "-m", "component boundary", cwd=wc)
    # Fail the final operation in the no-target transaction. No partial
    # receipt, legacy row or advanced SVN watermark may survive reopening.
    old_marks = db(marks)
    db("CREATE TRIGGER fixture_veto BEFORE INSERT ON audit_log WHEN NEW.action='svn_to_git_filtered' BEGIN SELECT RAISE(ABORT,'fixture checkpoint veto'); END")
    assert "fixture checkpoint veto" in ps("sync", ok=False)
    assert db(marks) == old_marks and db(rows,(scope,5)) == []
    assert not db("SELECT 1 FROM canonical_mapping_outcomes WHERE scope_uuid=? AND source_key='svn:5'", (scope,))
    assert remote_ref() == deletion
    db("DROP TRIGGER fixture_veto")
    assert "SVN→Git: 0 commits" in ps("sync") and db(rows,(scope,5)) == [(None,)]
    assert remote_ref() == deletion and dict(db(marks))["svn_rev"] == "5"
    (wc/"foo").mkdir()
    (wc/"foo"/"returned").write_text("returned")
    run(svn, "add", "foo", cwd=wc)
    run(svn, "commit", "-m", "restore selected root", cwd=wc)
    assert "SVN→Git: 1 commits" in ps("sync")
    recreated = remote_ref()
    assert recreated != deletion
    assert run("git", "--git-dir", remote, "show", "main:foo/returned") == "returned"
    run(svn, "propset", "svn:executable", "*", "foo/returned", cwd=wc)
    run(svn, "commit", "-m", "selected executable-bit-only change", cwd=wc)
    assert "SVN→Git: 1 commits" in ps("sync")
    assert remote_ref() != recreated
    assert run("git", "--git-dir", remote, "ls-tree", "-r", "main").startswith("100755 blob")
    assert not events, events
    assert db("SELECT count(*) FROM pr_sync_log") == [(0,)]
    server.shutdown()
    server.server_close()
    prefix = ("qualified svn:// nested branch" if nested else "qualified svn:// directory") if network else "file:// directory"
    print(f"PASS #55 {prefix}: verified filtered receipt, actual no-export replay after policy change, orphan NULL rejection, selected mixed commit, selected deletion, transaction rollback and retry, fresh processes, PR isolation, CLI status/log/doctor and guarded imports.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--personal", required=True, type=Path)
    parser.add_argument("--cli", required=True, type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="reposync-personal-filter55-") as directory:
        root = Path(directory)
        local = root/"local"
        local.mkdir()
        exercise(local, args.personal.resolve(), args.cli.resolve())
        for mode in ["full", "nested", "replaced", "file", "directory"]:
            network = root/f"network-{mode}"
            network.mkdir()
            with loopback_svn(network) as url:
                exercise(network, args.personal.resolve(), args.cli.resolve(), url,
                         mode if mode in ("file", "directory") else None,
                         nested=mode in ("nested", "replaced"), replacement=mode=="replaced")
