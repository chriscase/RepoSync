#!/usr/bin/env python3
"""#117 inert transports/services/builds; only owned temporary Git/filesystem data.

No server, installed daemon, real npm/cargo build or service is exercised here.
"""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
import deploy as client
import deploy_remote as remote

ROOT = Path(__file__).resolve().parents[1]
SHA = "a" * 40
UI = "b" * 40


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], stderr=subprocess.PIPE, text=True).strip()


class ClientTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rs117-client-")
        self.addCleanup(self.temp.cleanup)
        self.env = patch.dict(os.environ, {"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
                                          "DEPLOY_BRANCH": "HEAD", "DEPLOY_SSH_HOST": "synthetic.invalid",
                                          "DEPLOY_SSH_OPTS": "-o ControlMaster=no -o ControlPath=none",
                                          "DEPLOY_REMOTE_REPO": "SyntheticRepo"})
        self.env.start()
        self.addCleanup(self.env.stop)
        templates = Path(self.temp.name) / "empty-templates"
        templates.mkdir()
        os.environ["GIT_TEMPLATE_DIR"] = str(templates)
        self.root = Path(self.temp.name) / "repo"
        (self.root / "scripts").mkdir(parents=True)
        (self.root / "web-ui").mkdir()
        for name in ["deploy.sh", "deploy.py", "deploy_remote.py"]:
            shutil.copy2(ROOT / "scripts" / name, self.root / "scripts" / name)
        (self.root / "web-ui/package.json").write_text('{"name":"synthetic"}\n')
        (self.root / ".gitignore").write_text('Cargo.lock\nweb-ui/dist/\n')
        git(self.root, "init", "-q")
        git(self.root, "add", ".")
        git(self.root, "-c", "user.name=Synthetic", "-c", "user.email=test@example.invalid", "commit", "-qm", "fixture")
        self.sha = git(self.root, "rev-parse", "HEAD")
        self.calls = []
        self.stage = str(Path(self.temp.name) / "remote-stage")

    def inert_bins(self):
        bins = Path(self.temp.name) / "bin"
        bins.mkdir(exist_ok=True)
        stub = '#!/bin/sh\nprintf "%s\\n" "$0 $*" >> "$RS117_CALLS"\nexit 90\n'
        for name in ["ssh", "scp", "npm", "cargo", "sudo", "systemctl", "curl"]:
            p = bins / name
            p.write_text(stub)
            p.chmod(0o755)
        return dict(os.environ, PATH=str(bins) + os.pathsep + os.environ["PATH"],
                    RS117_CALLS=str(Path(self.temp.name) / "calls"))

    def test_rs117_dry_run_zero_effects_with_and_without_ui(self):
        for extra in [[], ["--no-web-ui"]]:
            before = git(self.root, "status", "--porcelain")
            result = subprocess.run(["bash", str(self.root / "scripts/deploy.sh"), "--dry-run", *extra],
                                    env=self.inert_bins(), capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
            plan = json.loads(result.stdout.splitlines()[-1])
            self.assertEqual(plan["commit"], self.sha)
            self.assertFalse((Path(self.temp.name) / "calls").exists())
            self.assertEqual(git(self.root, "status", "--porcelain"), before)
            self.assertFalse((self.root / "web-ui/dist").exists())

    def test_rs117_dirty_and_mismatched_sources_fail_before_transport(self):
        (self.root / "uncommitted").write_text("preserve me")
        result = subprocess.run(["bash", str(self.root / "scripts/deploy.sh"), "--dry-run"],
                                env=self.inert_bins(), capture_output=True, text=True, timeout=15)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("dirty source", result.stderr)
        (self.root / "uncommitted").unlink()
        git(self.root, "branch", "stale")
        git(self.root, "-c", "user.name=Synthetic", "-c", "user.email=test@example.invalid",
            "commit", "--allow-empty", "-qm", "next")
        result = subprocess.run(["bash", str(self.root / "scripts/deploy.sh"), "--branch", "stale"],
                                env=self.inert_bins(), capture_output=True, text=True, timeout=15)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("immutable HEAD", result.stderr)
        self.assertFalse((Path(self.temp.name) / "calls").exists())

    def execute(self, fault=None, no_ui=False):
        (self.root / "Cargo.lock").write_text("version = 3\n")
        original = client.run

        def run(args, **kwargs):
            self.calls.append(args)
            if args[0] == "git":
                if "push" in args:
                    self.assertEqual(args[-1], f"{self.sha}:refs/reposync/deploy/{self.sha}")
                    self.assertNotIn("--force", args)
                    return ""
                return original(args, **kwargs)
            if args[0] == "ssh":
                words = shlex.split(args[-1])
                phase = words[2]
                if phase == fault:
                    raise subprocess.CalledProcessError(7, args)
                receipt = {"phase": phase, "commit": self.sha}
                if phase == "prepare":
                    receipt["stage"] = self.stage
                if phase == "seal":
                    receipt["manifest_sha256"] = "c" * 64
                if phase == "activate":
                    receipt["verified"] = True
                if fault == "foreign_receipt":
                    receipt["commit"] = SHA
                return json.dumps(receipt)
            if args[0] == "scp":
                if args[-1].endswith("ui.tar") and fault == "transfer":
                    raise subprocess.CalledProcessError(8, args)
                return ""
            if args[0] == "npm":
                if args[1:] == ["run", "build"]:
                    dist = Path(kwargs["cwd"]) / "dist"
                    dist.mkdir()
                    (dist / "index.html").write_text('<script src="app.js"></script>')
                    (dist / "app.js").write_text("synthetic")
                return ""
            self.fail(f"unmodeled effect: {args}")

        with patch.object(client, "__file__", str(self.root / "scripts/deploy.py")), \
                patch.object(client, "run", side_effect=run), contextlib.redirect_stdout(io.StringIO()):
            return client.main(["--no-web-ui"] if no_ui else [])

    def phases(self):
        return [shlex.split(c[-1])[2] for c in self.calls if c[0] == "ssh"]

    def test_rs117_client_happy_path_pins_commit_and_ui(self):
        self.assertEqual(self.execute(), 0)
        self.assertEqual(self.phases(), ["prepare", "build", "seal", "activate"])
        for call in [c for c in self.calls if c[0] == "ssh"]:
            self.assertEqual(shlex.split(call[-1])[4], self.sha)
        self.assertEqual([c[1:] for c in self.calls if c[0] == "npm"], [["ci"], ["run", "build"]])
        self.assertEqual(git(self.root, "status", "--porcelain"), "")

    def test_rs117_client_compile_failure_stops_later_effects(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.execute("build")
        self.assertEqual(self.phases(), ["prepare", "build"])
        self.assertFalse(any(c[0] == "npm" for c in self.calls))
        self.assertFalse(any(c[0] == "scp" and c[-1].endswith("ui.tar") for c in self.calls))

    def test_rs117_client_transfer_failure_does_not_seal_or_promote(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.execute("transfer")
        self.assertEqual(self.phases(), ["prepare", "build"])
        self.assertEqual(git(self.root, "status", "--porcelain"), "")

    def test_rs117_client_foreign_receipt_stops_before_build(self):
        with self.assertRaises(client.DeploymentError):
            self.execute("foreign_receipt")
        self.assertEqual(self.phases(), ["prepare"])

    def test_rs117_client_no_ui_never_builds_or_uploads_ui(self):
        self.assertEqual(self.execute(no_ui=True), 0)
        self.assertFalse(any(c[0] == "npm" for c in self.calls))
        self.assertFalse(any(c[0] == "scp" and c[-1].endswith("ui.tar") for c in self.calls))


class RemoteTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rs117-remote-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        self.binary = self.repo / "target/release/reposync-daemon"
        self.binary.parent.mkdir(parents=True)
        self.binary.write_bytes(b"retained previous binary")
        self.binary.chmod(0o755)
        self.old_static = self.binary.parent / "static"
        self.old_static.mkdir()
        (self.old_static / "index.html").write_bytes(b"retained previous UI")
        self.store = self.root / "store"
        self.store.mkdir()
        self.stage = self.store / (SHA + "-fixture")
        self.stage.mkdir()
        self.artifact = self.stage / "artifact"
        self.artifact.mkdir()
        (self.artifact / "reposync-daemon").write_bytes(b"new synthetic daemon")
        (self.artifact / "reposync-daemon").chmod(0o755)
        remote.save(self.stage / "input.json", {"commit": SHA, "ui_source_tree": UI,
                                               "previous": remote.live_state(self.binary)})
        remote.save(self.stage / "build.json", {"commit": SHA, "short": SHA[:7], "lock_sha256": "d" * 64,
                                               "binary_sha256": remote.digest(self.artifact / "reposync-daemon")})
        self.archive = self.stage / "ui.tar"
        self.make_tar({"index.html": b"new index", "assets/app.js": b"new JS"})

    def make_tar(self, values):
        with tarfile.open(self.archive, "w") as tar:
            for name, value in values.items():
                member = tarfile.TarInfo(name)
                member.size = len(value)
                tar.addfile(member, io.BytesIO(value))

    def seal(self):
        return remote.seal(self.repo, self.stage, SHA, UI, remote.digest(self.archive))["manifest_sha256"]

    def old_preserved(self):
        self.assertEqual((self.old_static / "index.html").read_bytes(), b"retained previous UI")
        self.assertFalse((self.stage / "verified.json").exists())

    def test_rs117_ui_transfer_corruption_fails_before_extraction(self):
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            remote.seal(self.repo, self.stage, SHA, UI, "e" * 64)
        self.assertFalse((self.artifact / "static").exists())
        self.assertEqual(self.binary.read_bytes(), b"retained previous binary")
        self.old_preserved()

    def test_rs117_ui_traversal_symlink_duplicate_refused(self):
        for name in ["../escape", "/escape", "assets//app.js"]:
            self.make_tar({name: b"bad"})
            with self.assertRaises(ValueError):
                self.seal()
            self.assertFalse((self.artifact / "static").exists())
        with tarfile.open(self.archive, "w") as tar:
            member = tarfile.TarInfo("index.html")
            member.type = tarfile.SYMTYPE
            member.linkname = "../escape"
            tar.addfile(member)
        with self.assertRaises(ValueError):
            self.seal()
        self.assertFalse((self.artifact / "static").exists())

    def test_rs117_duplicate_tar_members_are_refused(self):
        with tarfile.open(self.archive, "w") as tar:
            for _ in range(2):
                member = tarfile.TarInfo("index.html")
                member.size = 1
                tar.addfile(member, io.BytesIO(b"x"))
        with self.assertRaises(ValueError):
            self.seal()
        self.assertFalse((self.artifact / "static").exists())

    def test_rs117_compiler_exit_is_propagated_without_success_artifact(self):
        (self.stage / "build.json").unlink()
        shutil.rmtree(self.artifact)
        (self.stage / "source").mkdir()
        (self.stage / "Cargo.lock").write_text("synthetic lock")
        lock_sha = remote.digest(self.stage / "Cargo.lock")
        with patch.object(remote, "command", side_effect=subprocess.CalledProcessError(17, ["cargo"])) as effect:
            with self.assertRaises(subprocess.CalledProcessError) as error:
                remote.build(self.stage, SHA, UI, lock_sha)
        self.assertEqual(error.exception.returncode, 17)
        self.assertEqual(effect.call_args.args[0], ["cargo", "build", "--locked", "--release", "--bin", "reposync-daemon"])
        self.assertFalse(self.artifact.exists())
        self.assertFalse((self.stage / "build.json").exists())
        self.assertEqual(self.binary.read_bytes(), b"retained previous binary")

    def test_rs117_real_git_preparation_build_and_seal_use_one_fixed_commit(self):
        # Real disposable Git/worktree and artifact IO; cargo and systemd inert.
        templates = self.root / "empty-templates"
        templates.mkdir()
        env = {"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
               "GIT_TEMPLATE_DIR": str(templates)}
        with patch.dict(os.environ, env):
            (self.repo / ".gitignore").write_text("target/\nCargo.lock\n")
            (self.repo / "web-ui").mkdir()
            (self.repo / "web-ui/package.json").write_text('{"name":"synthetic"}')
            git(self.repo, "init", "-q")
            git(self.repo, "add", ".")
            git(self.repo, "-c", "user.name=Synthetic", "-c", "user.email=test@example.invalid",
                "commit", "-qm", "fixed source")
            sha = git(self.repo, "rev-parse", "HEAD")
            ui = git(self.repo, "rev-parse", "HEAD:web-ui")
            real_command, real_output = remote.command, remote.output
            calls = []

            def command(args, **kwargs):
                calls.append(args)
                if args[0] == "cargo":
                    binary = Path(kwargs["env"]["CARGO_TARGET_DIR"]) / "release/reposync-daemon"
                    binary.parent.mkdir(parents=True)
                    binary.write_bytes(b"modeled compiler output")
                    binary.chmod(0o755)
                    return subprocess.CompletedProcess(args, 0)
                self.assertEqual(args[0], "git")
                return real_command(args, **kwargs)

            def output(args, **kwargs):
                if args[0] == "systemctl":
                    return "{ path=" + str(self.binary) + " ; }"
                self.assertEqual(args[0], "git")
                return real_output(args, **kwargs)

            with patch.object(remote, "command", side_effect=command), patch.object(remote, "output", side_effect=output):
                with patch.object(remote, "service_path", side_effect=ValueError("wrong existing service path")):
                    with self.assertRaises(ValueError):
                        remote.prepare(self.repo, self.store, sha, ui)
                self.assertEqual([c[3] for c in calls], ["status", "rev-parse", "rev-parse"],
                                 "wrong service permits only read-only source checks")
                self.assertEqual(list(self.store.iterdir()), [self.stage],
                                 "wrong service must not allocate a new stage")
                prepared = Path(remote.prepare(self.repo, self.store, sha, ui)["stage"])
                (prepared / "Cargo.lock").write_text("version = 3\n")
                lock_sha = remote.digest(prepared / "Cargo.lock")
                built = remote.build(prepared, sha, ui, lock_sha)
            self.assertEqual(built["lock_sha256"], lock_sha)
            self.assertEqual(git(prepared / "source", "rev-parse", "HEAD"), sha)
            shutil.copyfile(self.archive, prepared / "ui.tar")
            sealed = remote.seal(self.repo, prepared, sha, ui, remote.digest(prepared / "ui.tar"))
            manifest = json.loads((prepared / "artifact/manifest.json").read_text())
            self.assertEqual(manifest["commit"], sha)
            self.assertEqual(manifest["ui_source_tree"], ui)
            self.assertEqual(manifest["lock_sha256"], lock_sha)
            self.assertEqual(sealed["manifest_sha256"], remote.digest(prepared / "artifact/manifest.json"))
            self.assertEqual(self.binary.read_bytes(), b"retained previous binary")
            self.old_preserved()

    def test_rs117_dependency_lock_mismatch_prevents_build(self):
        (self.stage / "Cargo.lock").write_text("synthetic lock")
        with patch.object(remote, "command") as effect:
            with self.assertRaisesRegex(ValueError, "lock transfer checksum"):
                remote.build(self.stage, SHA, UI, "f" * 64)
        effect.assert_not_called()

    def previous_release(self):
        previous = self.store / "previous-artifact"
        previous.mkdir()
        shutil.copy2(self.binary, previous / "reposync-daemon")
        (previous / "static/assets").mkdir(parents=True)
        (previous / "static/index.html").write_bytes(b"verified previous UI")
        (previous / "static/assets/app.js").write_bytes(b"verified JS")
        remote.save(previous / "manifest.json", {"ui_source_tree": UI,
                    "binary_sha256": remote.digest(previous / "reposync-daemon"),
                    "static": remote.manifest_files(previous / "static")})
        self.binary.unlink()
        self.binary.symlink_to(previous / "reposync-daemon")
        return previous

    def test_rs117_no_ui_reuses_exact_source_and_verified_bytes(self):
        previous = self.previous_release()
        receipt = remote.seal(self.repo, self.stage, SHA, UI, "reuse")
        self.assertEqual(remote.manifest_files(self.artifact / "static"), remote.manifest_files(previous / "static"))
        self.assertEqual(receipt["manifest_sha256"], remote.digest(self.artifact / "manifest.json"))

    def test_rs117_no_ui_refuses_stale_ui_bytes(self):
        previous = self.previous_release()
        (previous / "static/assets/app.js").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "source/content mismatch"):
            remote.seal(self.repo, self.stage, SHA, UI, "reuse")
        self.assertFalse((self.artifact / "static").exists())

    def activate(self, checksum, health=None, physical=True):
        self.effects = []
        original_digest = remote.digest

        def digest(path):
            if str(path).startswith("/proc/"):
                self.effects.append("physical-binary-observation")
                return original_digest(self.artifact / "reposync-daemon") if physical else "f" * 64
            return original_digest(path)

        def output(args, **kwargs):
            self.effects.append(args)
            if args[0] == "systemctl" and "--property=ExecStart" in args:
                return "{ path=" + str(self.binary) + " ; argv[]=synthetic ; }"
            if args[0] == "systemctl" and "--property=MainPID" in args:
                return "12345"
            if args[0] == "curl":
                return json.dumps(health if health is not None else {"ok": True, "git_commit": SHA[:7]})
            self.fail(f"unmodeled command {args}")

        def command(args, **kwargs):
            self.assertEqual(args, ["sudo", "-n", "systemctl", "restart", "reposync"])
            self.effects.append(args)
            return subprocess.CompletedProcess(args, 0)

        with patch.object(remote, "output", side_effect=output), patch.object(remote, "command", side_effect=command), \
                patch.object(remote, "digest", side_effect=digest), patch.object(remote.time, "sleep"):
            return remote.activate(self.repo, self.store, self.stage, SHA, UI, checksum)

    def test_rs117_sealed_identity_mismatch_prevents_promotion_and_restart(self):
        checksum = self.seal()
        (self.artifact / "reposync-daemon").write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "artifact identity"):
            self.activate(checksum)
        self.assertEqual(self.effects, [])
        self.assertEqual(self.binary.read_bytes(), b"retained previous binary")
        self.old_preserved()

    def test_rs117_concurrent_deployment_change_refused(self):
        checksum = self.seal()
        self.binary.write_bytes(b"newer unrelated promotion")
        with self.assertRaisesRegex(ValueError, "changed since preparation"):
            self.activate(checksum)
        self.assertFalse(any(isinstance(c, list) and c[0] == "sudo" for c in self.effects))
        self.assertEqual(self.binary.read_bytes(), b"newer unrelated promotion")
        self.old_preserved()

    def assert_bad_health_identity(self, identity):
        checksum = self.seal()
        with self.assertRaisesRegex(ValueError, "health/build identity"):
            self.activate(checksum, {"ok": True, "git_commit": identity})
        self.assertTrue(self.binary.is_symlink())
        self.assertEqual((self.stage / "previous-daemon").read_bytes(), b"retained previous binary")
        self.assertNotIn("physical-binary-observation", self.effects)
        self.old_preserved()

    def test_rs117_dirty_health_identity_refused(self):
        self.assert_bad_health_identity(SHA[:7] + "+dirty")

    def test_rs117_unknown_health_identity_refused(self):
        self.assert_bad_health_identity("unknown")

    def test_rs117_wrong_health_identity_refused(self):
        self.assert_bad_health_identity("f" * 7)

    def test_rs117_health_failure_stops_without_asset_deletion_or_rollback(self):
        checksum = self.seal()
        with self.assertRaisesRegex(ValueError, "health/build identity"):
            self.activate(checksum, {"ok": False, "git_commit": SHA[:7]})
        self.assertEqual(len([c for c in self.effects if isinstance(c, list) and c[0] == "curl"]), 12)
        self.assertTrue((self.stage / "promotion-intent.json").exists())
        self.assertTrue(self.binary.is_symlink())
        self.old_preserved()

    def test_rs117_running_binary_mismatch_refused(self):
        checksum = self.seal()
        with self.assertRaisesRegex(ValueError, "running service binary"):
            self.activate(checksum, physical=False)
        self.old_preserved()

    def test_rs117_success_retains_prior_assets_and_exact_receipt(self):
        checksum = self.seal()
        receipt = self.activate(checksum)
        self.assertTrue(receipt["verified"])
        self.assertEqual(receipt["manifest_sha256"], checksum)
        self.assertIn("physical-binary-observation", self.effects)
        self.assertTrue(self.binary.is_symlink())
        self.assertEqual((self.stage / "previous-daemon").read_bytes(), b"retained previous binary")
        self.assertEqual((self.old_static / "index.html").read_bytes(), b"retained previous UI")
        self.assertTrue((self.stage / "verified.json").is_file())

    def test_rs117_no_ui_refuses_unverified_legacy_assets(self):
        with self.assertRaisesRegex(ValueError, "no verified UI"):
            remote.seal(self.repo, self.stage, SHA, UI, "reuse")
        self.assertFalse((self.artifact / "static").exists())
        self.old_preserved()


if __name__ == "__main__":
    unittest.main(verbosity=2)
