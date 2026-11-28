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


def compatible_service(binary):
    facts = dict.fromkeys(remote.SERVICE_FIELDS, "")
    facts.update({field: "no" for field in remote.SERVICE_NO})
    facts.update(User=str(os.getuid()), Group=str(os.getgid()))
    return facts


def command_evidence(binary, restarted=False):
    invocation = [1700000000000000, 7000000, 1700000005000000, 12000000, 701, 1, 1]
    if restarted:
        invocation = [1700000010000000, 17000000, 0, 0, 12345, 0, 0]
    return {"type": "a(sasasttttuii)", "data": [[str(binary),
             [str(binary), "--config", "/etc/reposync/synthetic config.toml"], [], *invocation]]}


def prepared_service(binary):
    return dict(compatible_service(binary), resolved_uid=os.getuid(),
                ExecStart=remote.command_configuration(json.dumps(command_evidence(binary)), binary))


def service_text(facts):
    return "\n".join(f"{key}={value}" for key, value in facts.items())


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
        for name in ["ssh", "scp", "npm", "cargo", "sudo", "systemctl", "busctl", "curl"]:
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

    def execute(self, fault=None, no_ui=False, receipt_mutator=None):
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
                    receipt["binary_sha256"] = "e" * 64
                if phase == "activate":
                    receipt.update(verified=True, manifest_sha256="c" * 64,
                                   binary_sha256="e" * 64, pid="12345",
                                   executable_path=self.stage + "/artifact/reposync-daemon",
                                   static_directory=self.stage + "/artifact/static")
                if fault == "foreign_receipt":
                    receipt["commit"] = SHA
                if receipt_mutator:
                    receipt_mutator(phase, receipt)
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

        self.output = io.StringIO()
        with patch.object(client, "__file__", str(self.root / "scripts/deploy.py")), \
                patch.object(client, "run", side_effect=run), contextlib.redirect_stdout(self.output):
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

    def test_rs117_client_rejects_missing_or_malformed_seal_identities_before_activation(self):
        for field, value in [("manifest_sha256", None), ("manifest_sha256", "malformed"),
                             ("binary_sha256", None), ("binary_sha256", "x" * 64)]:
            with self.subTest(field=field, value=value):
                self.calls.clear()

                def mutate(phase, receipt):
                    if phase == "seal":
                        if value is None:
                            receipt.pop(field)
                        else:
                            receipt[field] = value

                with self.assertRaises(client.DeploymentError):
                    self.execute(no_ui=True, receipt_mutator=mutate)
                self.assertEqual(self.phases(), ["prepare", "build", "seal"])

    def test_rs117_client_requires_complete_same_manifest_activation_receipt(self):
        cases = [("manifest_sha256", None), ("manifest_sha256", "bad"),
                 ("manifest_sha256", "d" * 64), ("binary_sha256", None),
                 ("binary_sha256", "bad"), ("binary_sha256", "f" * 64),
                 ("pid", None), ("pid", 123), ("pid", "0"), ("pid", "-1"),
                 ("pid", "12x"), ("executable_path", None),
                 ("executable_path", "/another/release/reposync-daemon"),
                 ("static_directory", None), ("static_directory", "/another/release/static")]
        for field, value in cases:
            with self.subTest(field=field, value=value):
                self.calls.clear()

                def mutate(phase, receipt):
                    if phase == "activate":
                        if value is None:
                            receipt.pop(field)
                        else:
                            receipt[field] = value

                with self.assertRaises(client.DeploymentError):
                    self.execute(no_ui=True, receipt_mutator=mutate)
                self.assertEqual(self.phases(), ["prepare", "build", "seal", "activate"],
                                 "an incomplete confirmation must not trigger another activation or rollback")
                self.assertNotIn('"verified": true', self.output.getvalue())


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
        self.store.mkdir(mode=0o700)
        self.stage = self.store / (SHA + "-fixture")
        self.stage.mkdir()
        self.artifact = self.stage / "artifact"
        self.artifact.mkdir()
        (self.artifact / "reposync-daemon").write_bytes(b"new synthetic daemon")
        (self.artifact / "reposync-daemon").chmod(0o755)
        service = prepared_service(self.binary)
        remote.save(self.stage / "input.json", {"commit": SHA, "ui_source_tree": UI,
                                               "previous": remote.live_state(self.binary), "service": service})
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
                    return service_text(compatible_service(self.binary))
                if args[0] == "busctl":
                    return json.dumps(command_evidence(self.binary))
                self.assertEqual(args[0], "git")
                return real_output(args, **kwargs)

            with patch.object(remote, "command", side_effect=command), patch.object(remote, "output", side_effect=output):
                with patch.object(remote, "service_profile", side_effect=ValueError("wrong existing service path")):
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

    def activate(self, checksum, health=None, physical=True, running_path=None,
                 unavailable_executable=False, redirect_pointer=None, service_facts=None,
                 static_view=None, command_before=None, command_after=None,
                 service_after=None):
        self.effects = []
        restarted = False
        original_digest = remote.digest

        def process_executable(pid):
            self.effects.append("physical-binary-observation")
            if unavailable_executable:
                if unavailable_executable == "missing":
                    raise FileNotFoundError("modeled missing executable association")
                raise PermissionError("modeled unavailable executable association")
            path = (running_path or self.artifact / "reposync-daemon").resolve()
            info = path.stat()
            checksum = original_digest(path) if physical else "f" * 64
            return str(path), info.st_dev, info.st_ino, checksum

        def process_static(pid, location):
            self.effects.append("static-directory-observation")
            directory = static_view or Path(location)
            info = directory.stat()
            return info.st_dev, info.st_ino, remote.manifest_files(directory)

        def output(args, **kwargs):
            self.effects.append(args)
            if args[0] == "systemctl" and "--property=User" in args:
                facts = service_after if restarted and service_after is not None else service_facts
                if isinstance(facts, str):
                    return facts
                return service_text(facts if facts is not None else compatible_service(self.binary))
            if args[0] == "busctl":
                evidence = command_after if restarted else command_before
                if evidence is None:
                    evidence = command_evidence(self.binary, restarted)
                if isinstance(evidence, BaseException):
                    raise evidence
                if isinstance(evidence, str):
                    return evidence
                return json.dumps(evidence)
            if args[0] == "systemctl" and "--property=MainPID" in args:
                return "12345"
            if args[0] == "curl":
                return json.dumps(health if health is not None else {"ok": True, "git_commit": SHA[:7]})
            self.fail(f"unmodeled command {args}")

        def command(args, **kwargs):
            nonlocal restarted
            self.assertEqual(args, ["sudo", "-n", "systemctl", "restart", "reposync"])
            self.effects.append(args)
            restarted = True
            if redirect_pointer:
                self.binary.unlink()
                self.binary.symlink_to(redirect_pointer)
            return subprocess.CompletedProcess(args, 0)

        with patch.object(remote, "output", side_effect=output), patch.object(remote, "command", side_effect=command), \
                patch.object(remote, "process_executable", side_effect=process_executable), \
                patch.object(remote, "process_static", side_effect=process_static), patch.object(remote.time, "sleep"):
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

    def another_same_byte_release(self):
        other = self.store / "other-release"
        other.mkdir()
        shutil.copy2(self.artifact / "reposync-daemon", other / "reposync-daemon")
        (other / "static/assets").mkdir(parents=True)
        (other / "static/index.html").write_bytes(b"different release index")
        (other / "static/assets/app.js").write_bytes(b"different release JS")
        self.assertEqual(remote.digest(other / "reposync-daemon"),
                         remote.digest(self.artifact / "reposync-daemon"))
        return other

    def test_rs117_same_binary_bytes_in_another_running_release_are_refused(self):
        checksum = self.seal()
        other = self.another_same_byte_release()
        with self.assertRaisesRegex(ValueError, "outside the sealed release"):
            self.activate(checksum, running_path=other / "reposync-daemon")
        self.old_preserved()

    def test_rs117_pointer_redirected_to_same_byte_release_is_refused(self):
        checksum = self.seal()
        other = self.another_same_byte_release()
        with self.assertRaisesRegex(ValueError, "pointer changed release"):
            self.activate(checksum, redirect_pointer=other / "reposync-daemon")
        self.assertEqual(self.binary.resolve(), (other / "reposync-daemon").resolve())
        self.assertEqual(len([c for c in self.effects if isinstance(c, list) and c[0] == "sudo"]), 1)
        self.old_preserved()

    def test_rs117_unreadable_executable_location_refuses_confirmation(self):
        checksum = self.seal()
        with self.assertRaises(PermissionError):
            self.activate(checksum, unavailable_executable=True)
        self.old_preserved()

    def test_rs117_missing_executable_location_refuses_confirmation(self):
        checksum = self.seal()
        with self.assertRaises(FileNotFoundError):
            self.activate(checksum, unavailable_executable="missing")
        self.old_preserved()

    def test_rs117_different_process_static_view_is_refused(self):
        checksum = self.seal()
        other = self.another_same_byte_release()
        with self.assertRaisesRegex(ValueError, "filesystem view differs"):
            self.activate(checksum, static_view=other / "static")
        self.old_preserved()

    def test_rs117_incompatible_or_ambiguous_service_profile_refuses_before_promotion(self):
        checksum = self.seal()
        good = compatible_service(self.binary)
        missing = dict(good)
        missing.pop("User")
        invalid = [dict(good, User=str(os.getuid() + 1000)), missing,
                   dict(good, DynamicUser="yes"), dict(good, RootDirectory="/different-root"),
                   dict(good, ProtectHome="yes"), dict(good, PrivateUsers="yes"),
                   service_text(good) + "\nUser=1234"]
        private_mode = self.store.stat().st_mode
        for facts in invalid:
            with self.subTest(facts=facts):
                with self.assertRaises(ValueError):
                    self.activate(checksum, service_facts=facts)
                self.assertEqual(self.binary.read_bytes(), b"retained previous binary")
                self.assertFalse(self.binary.is_symlink())
                self.assertFalse((self.stage / "promotion-intent.json").exists())
                self.assertFalse(any(isinstance(c, list) and c[0] == "sudo" for c in self.effects))
                self.assertEqual(self.store.stat().st_mode, private_mode)
                self.old_preserved()

    def test_rs117_service_profile_drift_refuses_before_promotion(self):
        checksum = self.seal()
        facts = dict(compatible_service(self.binary), Group="changed-group")
        with self.assertRaisesRegex(ValueError, "profile changed since preparation"):
            self.activate(checksum, service_facts=facts)
        self.assertFalse((self.stage / "promotion-intent.json").exists())
        self.assertEqual(self.binary.read_bytes(), b"retained previous binary")
        self.old_preserved()

    def test_rs117_runtime_command_changes_after_restart_confirm_once(self):
        checksum = self.seal()
        before = command_evidence(self.binary)
        after = command_evidence(self.binary, restarted=True)
        self.assertTrue(all(a != b for a, b in zip(before["data"][0][3:], after["data"][0][3:])))
        self.assertEqual(remote.command_configuration(json.dumps(before), self.binary),
                         remote.command_configuration(json.dumps(after), self.binary))
        self.assertEqual(before["data"][0][1][-1], "/etc/reposync/synthetic config.toml")
        literal_arguments = command_evidence(self.binary)
        literal_arguments["data"][0][1].extend(["", "literal 'quoted' argument"])
        self.assertEqual(remote.command_configuration(json.dumps(literal_arguments), self.binary)["argv"],
                         literal_arguments["data"][0][1], "preserve empty/quoted arguments without shell parsing")
        receipt = self.activate(checksum, command_before=before, command_after=after)
        self.assertTrue(receipt["verified"])
        self.assertEqual(len([c for c in self.effects if isinstance(c, list) and c[0] == "sudo"]), 1)
        self.assertEqual(json.loads((self.stage / "verified.json").read_text())["manifest_sha256"], checksum)
        self.assertTrue((self.stage / "previous-daemon").is_file())

    def test_rs117_stable_command_drift_refuses_before_promotion(self):
        checksum = self.seal()
        original = command_evidence(self.binary)
        changed_path = command_evidence(self.binary)
        changed_path["data"][0][0] = "/other/daemon"
        changed_argv = command_evidence(self.binary)
        changed_argv["data"][0][1][-1] = "/etc/reposync/other.toml"
        changed_boundaries = command_evidence(self.binary)
        changed_boundaries["data"][0][1][-1:] = ["/etc/reposync/synthetic", "config.toml"]
        self.assertEqual(" ".join(original["data"][0][1]), " ".join(changed_boundaries["data"][0][1]),
                         "human output can conceal different argument boundaries")
        changed_flags = command_evidence(self.binary)
        changed_flags["data"][0][2] = ["ignore-failure"]
        for evidence in [changed_path, changed_argv, changed_boundaries, changed_flags]:
            with self.subTest(evidence=evidence):
                with self.assertRaises(ValueError):
                    self.activate(checksum, command_before=evidence)
                self.assertFalse((self.stage / "promotion-intent.json").exists())
                self.assertFalse(self.binary.is_symlink())
                self.assertEqual(self.binary.read_bytes(), b"retained previous binary")
                self.assertFalse(any(isinstance(c, list) and c[0] == "sudo" for c in self.effects))
                self.old_preserved()

    def assert_post_restart_configuration_refused(self, command=None, facts=None):
        checksum = self.seal()
        with self.assertRaises(ValueError):
            self.activate(checksum, command_after=command, service_after=facts)
        self.assertEqual(len([c for c in self.effects if isinstance(c, list) and c[0] == "sudo"]), 1)
        self.assertEqual(self.binary.resolve(), (self.artifact / "reposync-daemon").resolve())
        self.assertEqual((self.stage / "previous-daemon").read_bytes(), b"retained previous binary")
        self.assertTrue((self.stage / "promotion-intent.json").exists())
        self.old_preserved()

    def test_rs117_executable_configuration_drift_after_restart_refuses_success(self):
        value = command_evidence(self.binary, restarted=True)
        value["data"][0][0] = "/another/daemon"
        self.assert_post_restart_configuration_refused(command=value)

    def test_rs117_argument_configuration_drift_after_restart_refuses_success(self):
        value = command_evidence(self.binary, restarted=True)
        value["data"][0][1].append("--pair-refresh-recovery")
        self.assert_post_restart_configuration_refused(command=value)

    def test_rs117_command_flag_drift_after_restart_refuses_success(self):
        value = command_evidence(self.binary, restarted=True)
        value["data"][0][2] = ["fully-privileged"]
        self.assert_post_restart_configuration_refused(command=value)

    def test_rs117_user_configuration_drift_after_restart_refuses_success(self):
        value = dict(compatible_service(self.binary), User=str(os.getuid() + 1000))
        self.assert_post_restart_configuration_refused(facts=value)

    def test_rs117_filesystem_configuration_drift_after_restart_refuses_success(self):
        value = dict(compatible_service(self.binary), ProtectHome="yes")
        self.assert_post_restart_configuration_refused(facts=value)

    def test_rs117_malformed_or_ambiguous_command_evidence_refuses_before_promotion(self):
        checksum = self.seal()
        good = command_evidence(self.binary)
        records = good["data"]
        duplicate_key = '{"type":"a(sasasttttuii)","type":"a(sasasttttuii)","data":' + json.dumps(records) + '}'
        invalid = ["not-json", {}, {"type": good["type"], "data": []},
                   {"type": good["type"], "data": records + records},
                   {"type": "a(sasbttttuii)", "data": records}, duplicate_key]
        for index, replacement in [(1, "lossy joined arguments"), (2, ["unsupported-flag"]),
                                   (3, True), (4, -1), (7, "701"), (8, 2**40)]:
            value = command_evidence(self.binary)
            value["data"][0][index] = replacement
            invalid.append(value)
        for evidence in invalid:
            with self.subTest(evidence=evidence):
                with self.assertRaises(ValueError):
                    self.activate(checksum, command_before=evidence)
                self.assertFalse((self.stage / "promotion-intent.json").exists())
                self.assertEqual(self.binary.read_bytes(), b"retained previous binary")
                self.old_preserved()

    def test_rs117_missing_typed_command_reader_refuses_without_fallback(self):
        checksum = self.seal()
        with self.assertRaises(FileNotFoundError):
            self.activate(checksum, command_before=FileNotFoundError("modeled unavailable busctl"))
        self.assertFalse((self.stage / "promotion-intent.json").exists())
        self.assertFalse(any(isinstance(c, list) and c[0] == "sudo" for c in self.effects))
        self.assertEqual(self.binary.read_bytes(), b"retained previous binary")
        self.old_preserved()

    def test_rs117_release_traversal_refusal_does_not_relax_permissions(self):
        mode = self.artifact.stat().st_mode & 0o777
        self.artifact.chmod(0o600)
        try:
            with self.assertRaisesRegex(ValueError, "cannot traverse"):
                remote.release_access(self.artifact, {"resolved_uid": os.getuid()})
            self.assertEqual(self.artifact.stat().st_mode & 0o777, 0o600)
            self.assertEqual(self.binary.read_bytes(), b"retained previous binary")
        finally:
            self.artifact.chmod(mode)

    def test_rs117_named_static_user_must_resolve_to_helper_uid(self):
        from types import SimpleNamespace
        facts = dict(compatible_service(self.binary), User="synthetic-service-user")
        def output(args, **kwargs):
            return json.dumps(command_evidence(self.binary)) if args[0] == "busctl" else service_text(facts)

        with patch.object(remote, "output", side_effect=output), \
                patch.object(remote.pwd, "getpwnam", return_value=SimpleNamespace(pw_uid=os.getuid())):
            self.assertEqual(remote.service_profile(self.binary)["resolved_uid"], os.getuid())
        with patch.object(remote, "output", side_effect=output), \
                patch.object(remote.pwd, "getpwnam", return_value=SimpleNamespace(pw_uid=os.getuid() + 1000)):
            with self.assertRaisesRegex(ValueError, "same static UID"):
                remote.service_profile(self.binary)

    def test_rs117_success_retains_prior_assets_and_exact_receipt(self):
        checksum = self.seal()
        receipt = self.activate(checksum)
        self.assertTrue(receipt["verified"])
        self.assertEqual(receipt["manifest_sha256"], checksum)
        self.assertIn("physical-binary-observation", self.effects)
        self.assertIn("static-directory-observation", self.effects)
        self.assertEqual(receipt["executable_path"], str((self.artifact / "reposync-daemon").resolve()))
        self.assertEqual(receipt["static_directory"], str((self.artifact / "static").resolve()))
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
