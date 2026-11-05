#!/usr/bin/env python3
"""Owned-fixture tests for the real offline installer entry point.

These use inert payload bytes and prove installer behavior, not native product,
schema, service, or old-install runtime qualification.
"""
import hashlib
import argparse
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import tarfile
import tempfile
import threading
import sqlite3
import sys
import re
import unittest
from unittest.mock import patch


SCRIPTS = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("install_offline", SCRIPTS / "install_offline.py")
offline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(offline)


class OfflineInstaller(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="reposync-installer-test-")
        self.addCleanup(self.temp.cleanup)
        self.area = Path(self.temp.name).resolve()
        self.root = self.area / "existing"
        self.root.mkdir(mode=0o700)
        for name in ("bin", "config", "data"):
            (self.root / name).mkdir()
        self.old = {name: ("old-" + name).encode() for name in offline.BINARIES}
        self.new = {name: ("new-" + name).encode() for name in offline.BINARIES}
        for name, content in self.old.items():
            p = self.root / "bin" / name
            p.write_bytes(content)
            p.chmod(0o700)
        (self.root / "config/config.toml").write_bytes(b"# retained default full import\n")
        (self.root / "config/authors.toml").write_bytes(b"# synthetic identity\n")
        (self.root / "data/reposync.db").write_bytes(b"synthetic-state-not-a-runtime-db")
        (self.root / "data/reposync.db-wal").write_bytes(b"retained WAL bytes")
        (self.root / "data/key").write_bytes(b"synthetic-only-key")
        self.before = self.state()
        self.archive = self.area / "package.tar.gz"

    def state(self):
        return {str(p.relative_to(self.root)): (p.read_bytes(), stat.S_IMODE(p.stat().st_mode))
                for group in ("config", "data") for p in (self.root / group).rglob("*")
                if p.is_file()}

    def package(self, members=None):
        with tarfile.open(self.archive, "w:gz") as archive:
            for name, content, kind in (members or [
                    (name, content, tarfile.REGTYPE) for name, content in self.new.items()]):
                info = tarfile.TarInfo(name)
                info.type = kind
                info.mode = 0o755
                info.linkname = "/outside"
                info.size = len(content) if kind == tarfile.REGTYPE else 0
                archive.addfile(info, io.BytesIO(content) if info.size else None)
        return hashlib.sha256(self.archive.read_bytes()).hexdigest()

    def invoke(self, digest):
        return subprocess.run(["bash", str(SCRIPTS / "install.sh"), "--offline-update",
                               "--root", str(self.root), "--archive", str(self.archive),
                               "--sha256", digest], capture_output=True, text=True, timeout=15)

    def assert_old(self):
        for name, content in self.old.items():
            self.assertEqual((self.root / "bin" / name).read_bytes(), content)
        self.assertEqual(self.state(), self.before)

    def test_real_entry_updates_only_binaries_and_preserves_state_modes(self):
        result = self.invoke(self.package())
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        receipt = json.loads(result.stdout)
        self.assertEqual(set(receipt["changed"]), set(offline.BINARIES))
        self.assertFalse(receipt["activation_performed"])
        self.assertFalse(receipt["configuration_or_database_opened"])
        for name, content in self.new.items():
            p = self.root / "bin" / name
            self.assertEqual(p.read_bytes(), content)
            self.assertEqual(stat.S_IMODE(p.stat().st_mode), 0o700)
        self.assertEqual(self.state(), self.before)
        self.assertEqual(list((self.root / "bin").glob(".reposync-update-*")), [])

    def test_repeat_is_noop_without_changing_retained_state(self):
        checksum = self.package()
        self.assertEqual(self.invoke(checksum).returncode, 0)
        result = self.invoke(checksum)
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(json.loads(result.stdout)["changed"], [])
        self.assertEqual(self.state(), self.before)

    def test_bad_checksum_refuses_before_root_mutation(self):
        self.package()
        result = self.invoke("0" * 64)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum mismatch", result.stdout)
        self.assertFalse((self.root / "bin/.reposync-offline-update.lock").exists())
        self.assert_old()

    def test_unexpected_traversal_duplicate_and_link_members_refuse(self):
        cases = [
            [("../outside", b"bad", tarfile.REGTYPE)],
            [("/absolute", b"bad", tarfile.REGTYPE)],
            [("reposync", b"one", tarfile.REGTYPE), ("reposync", b"two", tarfile.REGTYPE)],
            [("reposync", b"", tarfile.SYMTYPE)],
            [("reposync", b"", tarfile.LNKTYPE)],
            [("config.example.toml", b"bad", tarfile.REGTYPE)],
            [("reposync", b"incomplete", tarfile.REGTYPE)],
        ]
        for members in cases:
            with self.subTest(members=members):
                result = self.invoke(self.package(members))
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.root / "bin/.reposync-offline-update.lock").exists())
                self.assert_old()
        self.assertFalse((self.area / "outside").exists())

    def test_linked_root_refuses_without_following_it(self):
        checksum = self.package()
        original = self.root
        self.root = self.area / "linked"
        self.root.symlink_to(original, target_is_directory=True)
        result = self.invoke(checksum)
        self.root = original
        self.assertNotEqual(result.returncode, 0)
        self.assert_old()

    def test_linked_binary_refuses_and_external_canary_is_unchanged(self):
        checksum = self.package()
        outside = self.area / "outside"
        outside.write_bytes(b"keep outside")
        p = self.root / "bin/reposync"
        p.unlink()
        p.symlink_to(outside)
        result = self.invoke(checksum)
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(p.is_symlink())
        self.assertEqual(outside.read_bytes(), b"keep outside")
        self.assertEqual(self.state(), self.before)

    def test_nonprivate_root_refuses(self):
        checksum = self.package()
        self.root.chmod(0o755)
        result = self.invoke(checksum)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("private", result.stdout)
        self.assert_old()

    def test_partial_replacement_error_restores_previous_pair(self):
        checksum = self.package()
        replace = os.replace

        def fail_second(source, destination):
            if Path(source).name == "reposync-daemon":
                raise OSError("fixture second replacement failed")
            return replace(source, destination)

        with patch.object(offline.os, "replace", side_effect=fail_second):
            with self.assertRaisesRegex(OSError, "second replacement"):
                offline.update(self.root, self.archive, checksum)
        self.assert_old()
        self.assertEqual(list((self.root / "bin").glob(".reposync-update-*")), [])

    def test_failed_restore_retains_original_cause_and_recovery_files(self):
        checksum = self.package()
        replace = os.replace

        def fail_second_and_restore(source, destination):
            if Path(source).name in ("reposync-daemon", "reposync.previous"):
                raise OSError("fixture failure: " + Path(source).name)
            return replace(source, destination)

        with patch.object(offline.os, "replace", side_effect=fail_second_and_restore):
            with self.assertRaisesRegex(RuntimeError, "rollback uncertain") as failure:
                offline.update(self.root, self.archive, checksum)
        self.assertIn("reposync-daemon", str(failure.exception))
        self.assertIn("reposync.previous", str(failure.exception))
        stages = list((self.root / "bin").glob(".reposync-update-*"))
        self.assertEqual(len(stages), 1)
        self.assertEqual((stages[0] / "reposync.previous").read_bytes(), self.old["reposync"])
        self.assertEqual(self.state(), self.before)

    def test_supplied_invalid_qualification_spec_fails_without_fallback(self):
        specification = self.area / "invalid-spec.json"
        for content in ("not json", '{"kind":"unsupported"}'):
            with self.subTest(content=content):
                specification.write_text(content)
                result = subprocess.run([sys.executable, str(SCRIPTS / "test_install.py"),
                                         "--qualify-existing-update", "--root", str(self.root),
                                         "--spec", str(specification)],
                                        capture_output=True, text=True, timeout=15)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("R12_PINNED_INSTALLER_UPDATE", result.stdout)
                self.assert_old()


def package_into(record, destination):
    """Fixture construction from an authentic, separately pinned product package."""
    path = Path(record["archive"])
    offline.require(hashlib.sha256(path.read_bytes()).hexdigest() == record["sha256"],
                    "fixture package checksum mismatch")
    with tarfile.open(path, "r:gz") as archive:
        members = archive.getmembers()
        offline.require(len(members) == 2 and {m.name for m in members} == set(offline.BINARIES)
                        and all(m.isfile() for m in members), "fixture package inventory mismatch")
        for member in members:
            with archive.extractfile(member) as source:
                content = source.read()
            offline.require(hashlib.sha256(content).hexdigest() == record["binaries"][member.name],
                            "fixture executable does not match build provenance")
            target = destination / member.name
            target.write_bytes(content)
            target.chmod(0o755)


def tree_bytes(directory):
    value = {}
    for path in sorted(directory.rglob("*")):
        info = path.lstat()
        name = path.relative_to(directory).as_posix()
        if stat.S_ISLNK(info.st_mode):
            value[name] = {"link": os.readlink(path), "mode": stat.S_IMODE(info.st_mode)}
        elif stat.S_ISREG(info.st_mode):
            value[name] = {"sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                           "mode": stat.S_IMODE(info.st_mode)}
        else:
            offline.require(stat.S_ISDIR(info.st_mode), "unsupported fixture state node")
    return value


def qualify_existing_update(root, spec_path):
    """Called only by the source-bound pinned-old engine fixture, before its assertions."""
    import tomllib
    from http.server import HTTPServer
    from urllib.parse import urlsplit, unquote

    root = root.resolve(strict=True)
    offline.require(root.is_relative_to(Path(tempfile.gettempdir()).resolve()),
                    "qualification root must be in the owned temporary fixture")
    specification = json.loads(spec_path.read_text())
    offline.require(specification["kind"] == "reposync.synthetic-installer-update.v1",
                    "unknown qualification specification")
    old, candidate = specification["old"], specification["candidate"]
    offline.require(old["source_head"] == "87379741779a6259f7eeb52a68cc6f061174e5ef",
                    "qualification requires the canonical pinned old baseline")
    root.chmod(0o700)  # Only this newly constructed temporary fixture prefix.
    (root / "bin").mkdir()
    (root / "config").mkdir(mode=0o700)
    package_into(old, root / "bin")
    original = (root / "data/config.toml").read_text()
    parsed = tomllib.loads(original)
    source_url = urlsplit(parsed["svn"]["url"])
    offline.require(source_url.scheme == "file" and source_url.netloc in ("", "localhost")
                    and Path(unquote(source_url.path)).resolve().is_relative_to(root.parent),
                    "non-fixture SVN refused")
    config, replacements = re.subn(r"(?m)^data_dir\s*=.*$",
                                   "data_dir = " + json.dumps(str(root / "data")), original)
    offline.require(replacements == 1, "ambiguous old data directory")
    config = config.replace("[github]\n", '[github]\napi_url = "http://127.0.0.1:1/api"\n', 1)
    config_path = root / "config/config.toml"
    config_path.write_text(config)
    (root / "config/authors.toml").write_text("# separately owned synthetic fixture settings\n")
    for name in ("fixture-home", "fixture-tmp"):
        (root / name).mkdir(mode=0o700)
    env = {"PATH": os.environ.get("PATH", os.defpath),
           "HOME": str(root / "fixture-home"), "TMPDIR": str(root / "fixture-tmp"),
           "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1",
           "GIT_TERMINAL_PROMPT": "0", "REPOSYNC_TEST_SVN_PW": "synthetic-only",
           "REPOSYNC_TEST_GH_TOKEN": "synthetic-only", "RUST_BACKTRACE": "1"}
    executions = []

    def run(arguments, cwd=root, extra=None):
        result = subprocess.run([str(a) for a in arguments], cwd=cwd, env=dict(env, **(extra or {})),
                                capture_output=True, timeout=60)
        executions.append({"argv": [str(a) for a in arguments], "exit_code": result.returncode,
                           "stdout_sha256": hashlib.sha256(result.stdout).hexdigest(),
                           "stderr_sha256": hashlib.sha256(result.stderr).hexdigest()})
        offline.require(result.returncode == 0,
                        f"fixture command failed: {arguments}: {result.stderr.decode(errors='replace')}")
        return result.stdout

    def schema():
        with sqlite3.connect((root / "data/reposync.db").as_uri() + "?mode=ro", uri=True) as db:
            return db.execute("PRAGMA user_version").fetchone()[0]

    run([root / "bin/reposync", "--config", config_path, "status"])
    old_schema = schema()
    before = {name: tree_bytes(root / name) for name in ("config", "data")}
    update_result = run(["bash", SCRIPTS / "install.sh", "--offline-update", "--root", root,
                         "--archive", candidate["archive"], "--sha256", candidate["sha256"]])
    update_receipt = json.loads(update_result)
    offline.require(before == {name: tree_bytes(root / name) for name in before},
                    "installer changed existing configuration, data, refs, WAL or key bytes/modes")
    for name in offline.BINARIES:
        offline.require(hashlib.sha256((root / "bin" / name).read_bytes()).hexdigest()
                        == candidate["binaries"][name], "installed candidate differs from build artifact")
    run([root / "bin/reposync-daemon", "--version"])
    run([root / "bin/reposync", "--config", config_path, "status"])
    candidate_schema = schema()
    offline.require(old_schema == candidate_schema == 12, "unexpected fixture schema transition")

    # Reuse #198's real smart-HTTP backend in one owned thread. No persistent
    # helper process, OS process discovery, service control or signal is needed.
    sys.path.insert(0, str(SCRIPTS.parent / "crates/core/tests/fixtures"))
    from git_http_auth_server import GitHttpBackendHandler

    class Provider(GitHttpBackendHandler):
        def do_HEAD(self):
            self.send_response(200 if self.path == "/api/repos/fixture/repo" else 404)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def do_GET(self):
            if self.path.startswith("/api/"):
                self.send_response(200)
                self.send_header("Content-Length", "2")
                self.end_headers()
                self.wfile.write(b"[]")
            else:
                super().do_GET()

    area = root / "new-full-import"
    area.mkdir()
    svn = area / "svn"
    run(["svnadmin", "create", svn])
    seed = area / "seed"
    seed.mkdir()
    (seed / "content.txt").write_text("first revision\n")
    run(["svn", "import", seed, svn.as_uri(), "-m", "first", "--username", "fixture",
         "--no-auth-cache", "--non-interactive"])
    wc = area / "wc"
    run(["svn", "checkout", svn.as_uri(), wc, "--no-auth-cache", "--non-interactive"])
    (wc / "content.txt").write_text("second revision\n")
    run(["svn", "commit", "-m", "second", "--username", "fixture", "--no-auth-cache",
         "--non-interactive"], cwd=wc)
    (area / "fixture").mkdir()
    bare = area / "fixture/repo.git"
    run(["git", "init", "--bare", bare])
    run(["git", "symbolic-ref", "HEAD", "refs/heads/main"], cwd=bare)
    run(["git", "config", "http.receivepack", "true"], cwd=bare)
    with patch.dict(os.environ, dict(env, GIT_PROJECT_ROOT=str(area),
                                    GIT_TEST_TOKEN="synthetic-only"), clear=True):
        server = HTTPServer(("127.0.0.1", 0), Provider)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            port = server.server_address[1]
            data = area / "personal"
            personal_config = area / "personal.toml"
            personal_config.write_text(f'''[personal]
data_dir = {json.dumps(str(data))}
[svn]
url = {json.dumps(svn.as_uri())}
username = "fixture"
password_env = "REPOSYNC_TEST_SVN_PW"
[github]
api_url = "http://127.0.0.1:{port}/api"
git_base_url = "http://127.0.0.1:{port}"
repo = "fixture/repo"
token_env = "REPOSYNC_TEST_GH_TOKEN"
auto_create = false
[developer]
name = "Installer Fixture"
email = "fixture@example.invalid"
svn_username = "fixture"
''')
            args = [root / "bin/reposync", "personal", "--personal-config", personal_config, "import"]
            run(args)  # Omitted mode must import BOTH revisions, not a snapshot.
            tip = run(["git", "rev-parse", "main"], cwd=bare)
            offline.require(run(["git", "rev-list", "--count", "main"], cwd=bare).strip() == b"2",
                            "omitted mode did not preserve full history")
            offline.require(run(["git", "show", "main:content.txt"], cwd=bare) == b"second revision\n",
                            "new full import content differs")
            run(args)
            offline.require(run(["git", "rev-parse", "main"], cwd=bare) == tip,
                            "repeated full import changed published history")
            with sqlite3.connect((data / "personal.db").as_uri() + "?mode=ro", uri=True) as db:
                offline.require(db.execute("SELECT count(*) FROM commit_map").fetchone()[0] == 2,
                                "full import/repeat did not retain exactly two mappings")
        finally:
            server.shutdown()
            thread.join(timeout=10)
            server.server_close()
            offline.require(not thread.is_alive(), "owned provider thread did not join")
    return {"case": "R12_PINNED_INSTALLER_UPDATE", "old_source": old["source_head"],
            "candidate_source": candidate["source_head"], "old_package": old["sha256"],
            "candidate_package": candidate["sha256"], "installer_receipt": update_receipt,
            "installer_preserved_state_bytes_and_modes": True,
            "schema_before": old_schema, "schema_after": candidate_schema,
            "installed_cli_executed": True, "installed_daemon_version_only": True,
            "omitted_mode_full_import_revisions": 2, "full_import_repeat_noop": True,
            "executions": executions,
            "limits": "Owned-prefix synthetic update; no privileged installer, system service, physical daemon restart or deployed-version proof. Engine continuation assertions must still complete in the enclosing retained fixture."}


if __name__ == "__main__":
    if sys.argv[1:2] == ["--qualify-existing-update"]:
        parser = argparse.ArgumentParser()
        parser.add_argument("--root", required=True, type=Path)
        parser.add_argument("--spec", required=True, type=Path)
        args = parser.parse_args(sys.argv[2:])
        print(json.dumps(qualify_existing_update(args.root, args.spec), sort_keys=True))
    else:
        unittest.main()
