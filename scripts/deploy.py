#!/usr/bin/env python3
"""Pinned deployment client. Tests use inert transport; never a live server."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tarfile
import tempfile


class DeploymentError(RuntimeError):
    pass


def run(args, **kwargs):
    result = subprocess.run(args, check=True, text=True, **kwargs)
    return result.stdout.strip() if result.stdout else ""


def git(root, *args):
    return run(["git", "--no-optional-locks", "-C", str(root), *args], stdout=subprocess.PIPE)


def clean_commit(root, branch):
    head = git(root, "rev-parse", "--verify", "HEAD^{commit}")
    selected = git(root, "rev-parse", "--verify", f"{branch}^{{commit}}")
    if not re.fullmatch(r"[0-9a-f]{40}", head) or selected != head:
        raise DeploymentError("requested branch must resolve to the checked-out immutable HEAD")
    if git(root, "status", "--porcelain", "--untracked-files=all"):
        raise DeploymentError("dirty source refused; commit or separately preserve changes first")
    return head, git(root, "rev-parse", f"{head}:web-ui")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--branch", default=os.environ.get("DEPLOY_BRANCH", "HEAD"))
    parser.add_argument("--no-web-ui", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--lockfile", type=Path, help="reviewed Cargo.lock; defaults to checkout Cargo.lock")
    args = parser.parse_args(argv)
    root = Path(__file__).resolve().parent.parent
    sha, ui_tree = clean_commit(root, args.branch)
    plan = {"commit": sha, "ui_source_tree": ui_tree,
            "web_ui": "reuse exact verified UI tree" if args.no_web_ui else "build pinned UI",
            "phases": ["prepare/build isolated commit", "transfer and verify assets",
                       "seal artifact", "atomic binary-pointer promotion", "restart and verify"]}
    # Return before ALL transport, temporary files/worktrees and build commands.
    if args.dry_run:
        print("[dry-run] No transport, build, service or filesystem mutation.")
        print(json.dumps(plan, sort_keys=True))
        return 0
    lockfile = (args.lockfile or root / "Cargo.lock").resolve(strict=True)
    if not lockfile.is_file() or lockfile.stat().st_size > 4 * 1024 * 1024:
        raise DeploymentError("a reviewed regular Cargo.lock (at most 4 MiB) is required")
    lock_sha = hashlib.sha256(lockfile.read_bytes()).hexdigest()
    host = os.environ.get("DEPLOY_SSH_HOST", "rk10")
    remote_repo = os.environ.get("DEPLOY_REMOTE_REPO", "GitSvnSync")
    if not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.@-]*", host):
        raise DeploymentError("invalid SSH host")
    if not re.fullmatch(r"/?[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*", remote_repo):
        raise DeploymentError("remote repository must be a simple absolute or home-relative path")
    options = shlex.split(os.environ.get("DEPLOY_SSH_OPTS", "-o ControlMaster=no -o ControlPath=none"))
    helper = git(root, "show", f"{sha}:scripts/deploy_remote.py") + "\n"

    def remote(phase, *values):
        command = shlex.join(["python3", "-", phase, remote_repo, sha, ui_tree, *values])
        output = run(["ssh", *options, "-o", "ConnectTimeout=10", host, command],
                     input=helper, stdout=subprocess.PIPE)
        lines = output.splitlines()
        if not lines:
            raise DeploymentError(f"missing {phase} receipt")
        print("\n".join(lines[:-1]))
        receipt = json.loads(lines[-1])
        if receipt.get("commit") != sha or receipt.get("phase") != phase:
            raise DeploymentError(f"foreign or malformed {phase} receipt")
        return receipt

    # No merge, force push or moving deployment branch: send only this object.
    run(["git", "-C", str(root), "push", "server", f"{sha}:refs/reposync/deploy/{sha}"])
    prepared = remote("prepare")
    stage = prepared.get("stage", "")
    if not re.fullmatch(r"/[A-Za-z0-9_./-]+", stage):
        raise DeploymentError("unsafe staging receipt")
    run(["scp", *options, str(lockfile), f"{host}:{stage}/Cargo.lock"])
    remote("build", stage, lock_sha)
    archive_sha = "reuse"
    with tempfile.TemporaryDirectory(prefix="reposync-deploy-") as temp:
        work = Path(temp) / "source"
        if not args.no_web_ui:
            git(root, "worktree", "add", "--detach", str(work), sha)
            try:
                run(["npm", "ci"], cwd=work / "web-ui")
                run(["npm", "run", "build"], cwd=work / "web-ui")
                if git(work, "status", "--porcelain", "--untracked-files=all"):
                    raise DeploymentError("UI build changed tracked/unignored source")
                dist = work / "web-ui/dist"
                files = sorted(dist.rglob("*"))
                if not (dist / "index.html").is_file() or not files:
                    raise DeploymentError("UI build is incomplete")
                archive = Path(temp) / "ui.tar"
                with tarfile.open(archive, "w") as tar:
                    for path in files:
                        if path.is_symlink() or not (path.is_file() or path.is_dir()):
                            raise DeploymentError("unsupported UI build node")
                        if path.is_file():
                            tar.add(path, arcname=path.relative_to(dist), recursive=False)
                archive_sha = hashlib.sha256(archive.read_bytes()).hexdigest()
                run(["scp", *options, str(archive), f"{host}:{stage}/ui.tar"])
            finally:
                git(root, "worktree", "remove", "--force", str(work))
        sealed = remote("seal", stage, archive_sha)
        result = remote("activate", stage, sealed["manifest_sha256"])
        if result.get("verified") is not True:
            raise DeploymentError("deployment did not return verified artifact identity")
        print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (DeploymentError, subprocess.CalledProcessError, OSError, ValueError, KeyError) as exc:
        print(f"ERROR: deployment stopped: {exc}. Preserve staging/previous assets; reconcile before retry.", file=sys.stderr)
        sys.exit(1)
