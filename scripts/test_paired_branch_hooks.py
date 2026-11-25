#!/usr/bin/env python3
"""Exercise optional paired-branch hooks in temporary repositories using file transport only."""

import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


INSTALLER = Path(__file__).resolve().with_name("paired_branch_hooks.py")
PAIRED = "refs/heads/paired"


class GitFixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="RepoSync hooks ")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        for name in ("home", "templates", "tmp", "bin"):
            (self.root / name).mkdir()
        git = self.git_executable()
        self.assertIsNotNone(git, "A Git executable is required on PATH for the native hook fixtures.")
        (self.root / "bin/git").symlink_to(git)
        self.env = {
            "PATH": os.pathsep.join([str(self.root / "bin"), str(Path(sys.executable).parent), os.defpath]),
            "HOME": str(self.root / "home"),
            "XDG_CONFIG_HOME": str(self.root / "home/config"),
            "TMPDIR": str(self.root / "tmp"),
            "LC_ALL": "C",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": str(self.root / "home/.gitconfig"),
            "GIT_TEMPLATE_DIR": str(self.root / "templates"),
            "GIT_ALLOW_PROTOCOL": "file",
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_AUTHOR_NAME": "Fixture",
            "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
            "GIT_COMMITTER_NAME": "Fixture",
            "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
            "GIT_EDITOR": "true",
            "GIT_SEQUENCE_EDITOR": "true",
            "PYTHONDONTWRITEBYTECODE": "1",
        }
        self.repo = self.root / "dev's clone"
        self.bare = self.root / "origin.git"
        self.git("init", "-q", "-b", "main", self.repo, cwd=self.root)
        self.git("init", "-q", "--bare", self.bare)
        self.git("remote", "add", "origin", self.bare)
        self.base = self.commit("base.txt")
        self.git("branch", "paired")
        self.git("push", "-q", "origin", "paired")

    def git_executable(self):
        return shutil.which("git")

    def command(self, *argv, cwd=None, check=True):
        result = subprocess.run(
            argv, cwd=cwd or self.repo, env=self.env, stdin=subprocess.DEVNULL,
            capture_output=True, text=True, timeout=20,
        )
        if check:
            self.assertEqual(result.returncode, 0, f"{argv}: {result.stdout}\n{result.stderr}")
        return result

    def git(self, *args, cwd=None, check=True):
        return self.command("git", *args, cwd=cwd, check=check)

    def oid(self, ref="HEAD", cwd=None):
        return self.git("rev-parse", "--verify", ref, cwd=cwd).stdout.strip()

    def commit(self, name, cwd=None):
        repo = cwd or self.repo
        (repo / name).write_text(name + "\n")
        self.git("add", "--", name, cwd=repo)
        self.git("commit", "-qm", name, cwd=repo)
        return self.oid(cwd=repo)

    def tool(self, action, *, apply=False, cwd=None, check=True):
        repo = cwd or self.repo
        args = [sys.executable, str(INSTALLER), action, "--repo", str(repo)]
        if action == "install":
            args += ["--branch", "paired", "--remote", "origin"]
        if apply:
            args.append("--apply")
        return self.command(*args, cwd=self.root, check=check)

    def private(self):
        return self.repo / ".git/reposync-paired-branch-hooks"

    def rejected(self, *args, cwd=None):
        result = self.git(*args, cwd=cwd, check=False)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("reposync", result.stderr.lower(), result.stderr)

    def assert_absent(self, ref, cwd=None):
        result = self.git("show-ref", "--verify", "--quiet", ref, cwd=cwd, check=False)
        self.assertEqual(result.returncode, 1, result.stderr)


class NativeHookTests(GitFixture):
    def setUp(self):
        super().setUp()
        probe = self.git("-c", "hook.test-capability.command=true", "-c",
                         "hook.test-capability.event=pre-push", "hook", "list", "pre-push", check=False)
        self.assertTrue(probe.returncode == 0 and "test-capability" in probe.stdout,
                        "NativeHookTests require a Git with configured hook support (Git 2.54+). "
                        + self.git("--version").stdout.strip())

    def test_previews_repeated_install_and_removal(self):
        config = self.repo / ".git/config"
        original = config.read_bytes()
        self.tool("status")
        self.tool("install")
        self.tool("remove")
        self.assertEqual(config.read_bytes(), original)
        self.assertFalse(self.private().exists())
        self.tool("install", apply=True)
        self.assertEqual(json.loads(self.tool("status").stdout)["state"], "installed")
        active = config.read_bytes()
        self.tool("install", apply=True)
        self.tool("remove")
        self.assertEqual(config.read_bytes(), active)
        self.git("config", "--local", "test.retained", "added after installation")
        self.tool("remove", apply=True)
        self.tool("remove", apply=True)
        self.assertFalse(self.private().exists())
        self.assertEqual(self.git("config", "--local", "--get", "test.retained").stdout.strip(),
                         "added after installation")
        self.assertEqual(json.loads(self.tool("status").stdout)["state"], "absent")
        self.assertNotIn("reposync-paired-branch-hooks", config.read_text())

    def test_push_guard_is_opt_in_and_covers_target_ref(self):
        self.git("switch", "-q", "paired")
        published = self.commit("published.txt")
        self.git("push", "-q", "origin", "paired")
        self.git("switch", "-qc", "unrelated", self.base)
        divergent = self.commit("unrelated.txt")
        # An ordinary clone with this history allows the exact same forced update.
        force = ("push", "--force", "origin", "HEAD:" + PAIRED)
        self.git(*force)
        self.assertEqual(self.oid(PAIRED, self.bare), divergent)
        self.git("push", "-q", "--force", "origin", "paired")
        self.assertEqual(self.oid(PAIRED, self.bare), published)
        self.tool("install", apply=True)
        self.rejected(*force)
        self.rejected("push", "--force", "origin", "HEAD:refs/heads/another", "HEAD:" + PAIRED)
        self.rejected("push", "origin", ":" + PAIRED)
        self.assertEqual(self.oid(PAIRED, self.bare), published)
        self.assert_absent("refs/heads/another", self.bare)
        self.git("push", "-q", "origin", "HEAD:refs/heads/another")
        self.assertEqual(self.oid("refs/heads/another", self.bare), divergent)
        other = self.root / "other.git"
        self.git("init", "--bare", "-q", other)
        self.git("remote", "add", "other", other)
        self.git("push", "-q", "other", "HEAD:" + PAIRED)
        self.assertEqual(self.oid(PAIRED, other), divergent)
        self.git("switch", "-q", "paired")
        forward = self.commit("forward.txt")
        self.git("push", "-q", "origin", "paired")
        self.assertEqual(self.oid(PAIRED, self.bare), forward)
        self.assertEqual(self.git("show", PAIRED + ":forward.txt", cwd=self.bare).stdout, "forward.txt\n")
        self.git("update-ref", "-d", PAIRED, cwd=self.bare)
        self.rejected("push", "origin", "paired")
        self.assert_absent(PAIRED, self.bare)
        self.tool("remove", apply=True)
        self.git("push", "-q", "origin", "paired")
        self.assertEqual(self.oid(PAIRED, self.bare), forward)

    def test_explicit_current_detached_and_linked_worktree_rebase(self):
        self.git("switch", "-q", "paired")
        paired = self.commit("paired.txt")
        self.git("switch", "-qc", "scratch", self.base)
        scratch = self.commit("scratch.txt")
        self.git("switch", "-q", "main")
        self.commit("upstream.txt")
        # Ensure native Git really invokes pre-rebase, with divergent histories.
        self.git("rebase", "main", "paired")
        self.assertNotEqual(self.oid(PAIRED), paired)
        self.git("switch", "-q", "main")
        self.git("branch", "-f", "paired", paired)
        self.tool("install", apply=True)
        self.rejected("rebase", "main", "paired")
        self.rejected("rebase", "main", PAIRED)
        self.assertEqual(self.oid(PAIRED), paired)
        linked = self.root / "linked dev"
        self.git("worktree", "add", "-q", linked, "paired")
        self.rejected("rebase", "main", cwd=linked)
        self.rejected("rebase", "main", "HEAD", cwd=linked)
        self.rejected("rebase", "main", "@", cwd=linked)
        self.assertEqual(self.oid(cwd=linked), paired)
        self.assertEqual(json.loads(self.tool("status", cwd=linked).stdout)["state"], "installed")
        self.git("rebase", "main", "scratch", cwd=linked)
        self.assertNotEqual(self.oid("refs/heads/scratch"), scratch)
        self.git("switch", "-q", "--detach", paired, cwd=linked)
        self.git("rebase", "main", cwd=linked)
        self.assertEqual(self.oid(PAIRED), paired)
        self.tool("remove", apply=True, cwd=linked)
        self.git("switch", "-q", "paired", cwd=linked)
        self.git("rebase", "main", cwd=linked)
        self.assertNotEqual(self.oid(PAIRED), paired)

    def test_unavailable_remote_ancestor_does_not_trigger_fetch(self):
        peer = self.root / "peer"
        self.git("clone", "-q", "--no-local", "-b", "paired", self.bare, peer)
        advance = self.commit("peer.txt", cwd=peer)
        self.git("push", "-q", "origin", "paired", cwd=peer)
        self.assertNotEqual(self.git("cat-file", "-e", advance, check=False).returncode, 0)
        self.tool("install", apply=True)
        self.commit("local.txt")
        self.rejected("push", "--force", "origin", "HEAD:" + PAIRED)
        self.assertEqual(self.oid(PAIRED, self.bare), advance)
        self.assertNotEqual(self.git("cat-file", "-e", advance, check=False).returncode, 0)
        self.assertEqual(self.oid("refs/remotes/origin/paired"), self.base)

        # In a partial clone cat-file can contact the promisor for a missing
        # object. Feed the same advertised update to the installed native hook.
        self.git("config", "core.repositoryformatversion", "1")
        self.git("config", "extensions.partialClone", "origin")
        self.git("config", "remote.origin.promisor", "true")
        self.git("config", "remote.origin.partialclonefilter", "blob:none")
        advertised = self.root / "pre-push-input"
        advertised.write_text(f"HEAD {self.oid()} {PAIRED} {advance}\n")
        self.rejected("hook", "run", "--to-stdin=" + str(advertised), "pre-push", "--",
                      "origin", str(self.bare))
        self.assertNotEqual(self.git("--no-lazy-fetch", "cat-file", "-e", advance, check=False).returncode, 0)
        # A normal object read *can* fetch it from this disposable file remote.
        self.assertEqual(self.git("cat-file", "-t", advance).stdout, "commit\n")
        self.git("--no-lazy-fetch", "cat-file", "-e", advance)

    def test_existing_configured_and_worktree_file_push_hooks_keep_stream_and_veto(self):
        self.git("switch", "-q", "paired")
        hooks_dir = self.repo / ".team-hooks"
        hooks_dir.mkdir()
        helper = hooks_dir / "record-helper"
        helper.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "from pathlib import Path\n"
            "record = [sys.argv[1:], sys.stdin.read(), str(Path(__file__).resolve())]\n"
            "with open(os.environ['FIXTURE_LOG'], 'a') as out:\n"
            "    print(json.dumps(record), file=out)\n"
            "if sys.argv[1] == 'file' and Path(os.environ['FIXTURE_VETO']).exists():\n"
            "    print('existing fixture hook veto', file=sys.stderr)\n"
            "    sys.exit(27)\n"
        )
        existing = hooks_dir / "pre-push"
        existing.write_text('#!/bin/sh\nexec "$(dirname "$0")/record-helper" file "$@"\n')
        for path in (existing, helper):
            path.chmod(0o755)
        self.git("add", ".team-hooks")
        self.git("commit", "-qm", "versioned hooks")
        old = self.oid()
        self.git("push", "-q", "origin", "paired")
        self.git("switch", "-q", "main")
        linked = self.root / "linked checkout"
        self.git("worktree", "add", "-q", linked, "paired")
        linked_helper = linked / ".team-hooks/record-helper"
        linked_hook = linked / ".team-hooks/pre-push"
        originals = {p: (p.read_bytes(), p.stat().st_mode) for p in (linked_hook, linked_helper)}
        log = self.root / "record.jsonl"
        veto = self.root / "veto"
        self.env.update(FIXTURE_LOG=str(log), FIXTURE_VETO=str(veto))
        self.git("config", "--global", "core.hooksPath", ".team-hooks")
        self.git("config", "--global", "hook.fixture-contributor.command",
                 shlex.quote(str(linked_helper)) + " configured")
        self.git("config", "--global", "hook.fixture-contributor.event", "pre-push")
        global_config = Path(self.env["GIT_CONFIG_GLOBAL"])
        original_config = global_config.read_bytes()

        self.tool("install", apply=True)
        tip = self.commit("forward.txt", cwd=linked)
        second = "refs/heads/second"
        self.git("push", "origin", "paired", "paired:" + second, cwd=linked)
        self.assertEqual(self.oid(PAIRED, self.bare), tip)
        self.assertEqual(self.oid(second, self.bare), tip)
        records = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertCountEqual([rec[0][0] for rec in records], ["configured", "file"])
        self.assertEqual(records[0][1], records[1][1])
        for argv, data, executable in records:
            self.assertEqual(argv[1:], ["origin", str(self.bare)])
            self.assertCountEqual(data.splitlines(), [f"{PAIRED} {tip} {PAIRED} {old}",
                                                     f"{PAIRED} {tip} {second} {'0' * len(tip)}"])
            self.assertEqual(executable, str(linked_helper))

        veto.touch()
        result = self.git("push", "origin", "HEAD:refs/heads/unrelated", cwd=linked, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("existing fixture hook veto", result.stderr)
        self.assert_absent("refs/heads/unrelated", self.bare)
        self.tool("remove", apply=True, cwd=linked)
        self.assertEqual(global_config.read_bytes(), original_config)
        for path, (content, mode) in originals.items():
            self.assertEqual((path.read_bytes(), path.stat().st_mode), (content, mode))
        self.assertEqual(self.git("status", "--porcelain", cwd=linked).stdout, "")
        veto.unlink()
        log.write_text("")
        self.git("push", "origin", "HEAD:refs/heads/unrelated", cwd=linked)
        self.assertCountEqual([json.loads(s)[0][0] for s in log.read_text().splitlines()], ["configured", "file"])

    def test_shared_absolute_hooks_path_and_neighbor_stay_unchanged(self):
        shared = self.root / "shared hooks"
        shared.mkdir()
        previous = shared / "pre-rebase"
        previous.write_text("#!/bin/sh\necho 'existing rebase hook veto' >&2\nexit 27\n")
        previous.chmod(0o755)
        original = previous.read_bytes(), previous.stat().st_mode
        self.git("config", "--global", "core.hooksPath", str(shared))
        neighbor = self.root / "neighbor"
        self.git("clone", "-qb", "paired", self.bare, neighbor)
        neighbor_config = neighbor / ".git/config"
        original_neighbor = neighbor_config.read_bytes()
        self.git("switch", "-qc", "topic")
        topic = self.commit("topic.txt")
        self.git("switch", "-q", "main")
        self.commit("main.txt")
        self.tool("install", apply=True)
        neighbor_hooks = self.git("hook", "list", "pre-rebase", cwd=neighbor).stdout
        self.assertNotIn("reposync-paired-branch-pre-rebase", neighbor_hooks)
        for installed in (True, False):
            if not installed:
                self.tool("remove", apply=True)
            result = self.git("rebase", "main", "topic", check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("existing rebase hook veto", result.stderr)
            self.assertEqual(self.oid("refs/heads/topic"), topic)
            self.assertEqual(self.git("config", "--get", "core.hooksPath").stdout.strip(), str(shared))
            self.assertEqual(list(shared.iterdir()), [previous])
            self.assertEqual((previous.read_bytes(), previous.stat().st_mode), original)
            self.assertEqual(neighbor_config.read_bytes(), original_neighbor)

    def test_collision_and_edited_install_are_preserved(self):
        self.git("config", "--global", "hook.reposync-paired-branch-pre-push.command", "true")
        config = self.repo / ".git/config"
        before = config.read_bytes()
        self.assertNotEqual(self.tool("install", apply=True, check=False).returncode, 0)
        self.assertEqual(config.read_bytes(), before)
        self.assertFalse(self.private().exists())
        self.git("config", "--global", "--unset", "hook.reposync-paired-branch-pre-push.command")
        self.tool("install", apply=True)
        settings = self.private() / "settings.json"
        settings.write_bytes(settings.read_bytes() + b"\n")
        changed = settings.read_bytes()
        active = config.read_bytes()
        self.assertNotEqual(self.tool("remove", apply=True, check=False).returncode, 0)
        self.assertEqual(settings.read_bytes(), changed)
        self.assertEqual(config.read_bytes(), active)

    def test_repository_routing_cannot_redirect_an_explicit_install(self):
        neighbor = self.root / "decoy"
        self.git("clone", "-qb", "paired", self.bare, neighbor)
        configs = [r / ".git/config" for r in (self.repo, neighbor)]
        originals = {p: p.read_bytes() for p in configs}
        with patch.dict(self.env, GIT_DIR=str(neighbor / ".git"), GIT_WORK_TREE=str(neighbor)):
            result = self.tool("install", apply=True, check=False)
        self.assertNotEqual(result.returncode, 0)
        for path, before in originals.items():
            self.assertEqual(path.read_bytes(), before)
            self.assertFalse((path.parent / "reposync-paired-branch-hooks").exists())

    def test_shared_or_linked_config_cannot_be_replaced(self):
        config = self.repo / ".git/config"
        contents = config.read_bytes()
        shared = self.root / "shared.config"
        config.rename(shared)
        for style in ("symlink", "hardlink"):
            with self.subTest(style=style):
                if style == "symlink":
                    config.symlink_to(shared)
                else:
                    os.link(shared, config)
                try:
                    result = self.tool("install", apply=True, check=False)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertTrue(config.samefile(shared))
                    self.assertEqual(shared.read_bytes(), contents)
                    self.assertFalse(self.private().exists())
                finally:
                    config.unlink()

    def test_interrupted_registration_can_be_reattached_or_removed(self):
        self.tool("install", apply=True)
        self.git("config", "--local", "test.unrelated", "preserved")
        include = "reposync-paired-branch-hooks/hooks.config"
        for action in ("install", "remove"):
            with self.subTest(action=action):
                self.git("config", "--local", "--fixed-value", "--unset-all", "include.path", include)
                self.assertTrue(self.private().is_dir())
                self.assertEqual(json.loads(self.tool("status").stdout)["state"], "staged")
                self.tool(action, apply=True)
                self.assertEqual(self.private().exists(), action == "install")
                expected = "installed" if action == "install" else "absent"
                self.assertEqual(json.loads(self.tool("status").stdout)["state"], expected)
                self.assertEqual(self.git("config", "--get", "test.unrelated").stdout.strip(), "preserved")


class LegacyGitTests(GitFixture):
    def git_executable(self):
        chosen = os.environ.get("REPOSYNC_TEST_OLD_GIT")
        if not chosen:
            self.skipTest("set REPOSYNC_TEST_OLD_GIT to a real Git without named hook support")
        self.assertTrue(Path(chosen).is_absolute() and Path(chosen).is_file(), chosen)
        return chosen

    def test_unsupported_git_refuses_install_without_mutation(self):
        self.assertTrue(INSTALLER.is_file(), INSTALLER)
        version = self.git("--version").stdout.strip()
        probe = self.git("-c", "hook.test-capability.command=true", "-c",
                         "hook.test-capability.event=pre-push", "hook", "list", "pre-push", check=False)
        self.assertNotEqual(probe.returncode, 0, f"{version} is not a legacy Git")
        config = self.repo / ".git/config"
        before = config.read_bytes()
        result = self.tool("install", apply=True, check=False)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("git", result.stderr.lower(), result.stderr)
        self.assertEqual(config.read_bytes(), before)
        self.assertFalse(self.private().exists())


if __name__ == "__main__":
    unittest.main(defaultTest="NativeHookTests")
