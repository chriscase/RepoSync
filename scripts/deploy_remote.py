#!/usr/bin/env python3
"""Linux staging/promotion helper, supplied over stdin from the pinned commit.

No invocation of this helper is authorization to deploy. Qualification is inert.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def command(args, **kwargs):
    return subprocess.run(args, check=True, text=True, **kwargs)


def output(args, **kwargs):
    return command(args, stdout=subprocess.PIPE, **kwargs).stdout.strip()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def manifest_files(directory):
    require(directory.is_dir() and not directory.is_symlink(), "missing or linked static directory")
    result = {}
    for path in sorted(directory.rglob("*")):
        require(not path.is_symlink(), "linked static node")
        if path.is_dir():
            continue
        require(path.is_file(), "unsupported static node")
        result[path.relative_to(directory).as_posix()] = digest(path)
    require("index.html" in result and any(p.endswith(".js") for p in result), "incomplete UI")
    return result


def extract_ui(archive, directory, expected):
    require(re.fullmatch(r"[0-9a-f]{64}", expected) and digest(archive) == expected,
            "UI transfer checksum mismatch")
    with tarfile.open(archive) as tar:
        members = tar.getmembers()
        require(0 < len(members) <= 10000 and sum(m.size for m in members) <= 256 * 1024 * 1024,
                "UI archive exceeds file/byte limits")
        names = set()
        for member in members:
            path = PurePosixPath(member.name)
            require(member.isfile() and not path.is_absolute() and
                    all(p not in ("", ".", "..") for p in member.name.split("/")) and
                    member.name not in names, "unsupported/duplicate UI archive member")
            names.add(member.name)
        directory.mkdir()
        for member in members:
            target = directory / member.name
            target.parent.mkdir(parents=True, exist_ok=True)
            with tar.extractfile(member) as src, target.open("xb") as dst:
                shutil.copyfileobj(src, dst)


def save(path, value):
    with path.open("x") as f:
        json.dump(value, f, sort_keys=True)
        f.write("\n")
        f.flush()
        os.fsync(f.fileno())


def live_state(binary):
    if not binary.exists() and not binary.is_symlink():
        return {"exists": False}
    require(binary.is_file(), "existing daemon is missing or not regular")
    return {"exists": True, "link": os.readlink(binary) if binary.is_symlink() else None,
            "sha256": digest(binary), "mode": stat.S_IMODE(binary.stat().st_mode)}


def service_path(binary):
    value = output(["systemctl", "show", "reposync", "--property=ExecStart", "--value"])
    paths = re.findall(r"(?:^|[ {;])path=([^ ;}]+)", value)
    require(paths == [str(binary)], "service ExecStart does not name the managed deployment path")


def prepare(repo, store, sha, ui_tree):
    require(not output(["git", "-C", str(repo), "status", "--porcelain", "--untracked-files=all"]),
            "dirty remote repository refused")
    require(output(["git", "-C", str(repo), "rev-parse", "--verify", f"{sha}^{{commit}}"])
            == sha, "requested remote commit missing")
    require(output(["git", "-C", str(repo), "rev-parse", f"{sha}:web-ui"]) == ui_tree,
            "remote UI source identity mismatch")
    binary = repo / "target/release/reposync-daemon"
    service_path(binary)
    previous = live_state(binary)
    stage = Path(tempfile.mkdtemp(prefix=sha + "-", dir=store))
    print(f"[deploy] retain staging on any failure: {stage}", file=sys.stderr)
    source = stage / "source"
    save(stage / "input.json", {"commit": sha, "ui_source_tree": ui_tree, "previous": previous})
    command(["git", "-C", str(repo), "worktree", "add", "--detach", str(source), sha])
    return {"stage": str(stage)}


def build(stage, sha, ui_tree, lock_sha):
    metadata = json.loads((stage / "input.json").read_text())
    require(metadata["commit"] == sha and metadata["ui_source_tree"] == ui_tree,
            "foreign build identity")
    require(re.fullmatch(r"[0-9a-f]{64}", lock_sha) and digest(stage / "Cargo.lock") == lock_sha,
            "dependency lock transfer checksum mismatch")
    source = stage / "source"
    shutil.copy2(stage / "Cargo.lock", source / "Cargo.lock")
    env = dict(os.environ, CARGO_TARGET_DIR=str(stage / "target"))
    # No pipeline: any compiler failure is fatal, irrespective of its last line.
    command(["cargo", "build", "--locked", "--release", "--bin", "reposync-daemon"], cwd=source, env=env)
    require(not output(["git", "-C", str(source), "status", "--porcelain", "--untracked-files=all"]),
            "build changed source")
    artifact = stage / "artifact"
    artifact.mkdir()
    shutil.copy2(stage / "target/release/reposync-daemon", artifact / "reposync-daemon")
    require(os.access(artifact / "reposync-daemon", os.X_OK), "built daemon is not executable")
    short = output(["git", "-C", str(source), "rev-parse", "--short", "HEAD"])
    require(re.fullmatch(r"[0-9a-f]{7,40}", short) and sha.startswith(short), "invalid build identity")
    save(stage / "build.json", {"commit": sha, "short": short,
                               "lock_sha256": lock_sha,
                               "binary_sha256": digest(artifact / "reposync-daemon")})
    return {"lock_sha256": lock_sha}


def seal(repo, stage, sha, ui_tree, archive_sha):
    metadata = json.loads((stage / "input.json").read_text())
    build = json.loads((stage / "build.json").read_text())
    require(metadata["commit"] == build["commit"] == sha and metadata["ui_source_tree"] == ui_tree,
            "foreign staging identity")
    artifact = stage / "artifact"
    require(digest(artifact / "reposync-daemon") == build["binary_sha256"], "built binary changed")
    if archive_sha == "reuse":
        binary = repo / "target/release/reposync-daemon"
        require(binary.is_symlink(), "no verified UI available; omit --no-web-ui")
        previous = binary.resolve().parent
        old = json.loads((previous / "manifest.json").read_text())
        require(old["ui_source_tree"] == ui_tree and
                digest(previous / "reposync-daemon") == old["binary_sha256"] and
                manifest_files(previous / "static") == old["static"],
                "previous UI source/content mismatch; rebuild UI")
        shutil.copytree(previous / "static", artifact / "static", symlinks=False)
        require(manifest_files(artifact / "static") == old["static"], "UI changed during reuse")
    else:
        extract_ui(stage / "ui.tar", artifact / "static", archive_sha)
    value = {"commit": sha, "short": build["short"], "ui_source_tree": ui_tree,
             "lock_sha256": build["lock_sha256"],
             "binary_sha256": build["binary_sha256"], "static": manifest_files(artifact / "static")}
    save(artifact / "manifest.json", value)
    return {"manifest_sha256": digest(artifact / "manifest.json")}


def activate(repo, store, stage, sha, ui_tree, expected_manifest):
    # Only deployers using this helper share this lock, not ordinary engine work.
    with (store / "promotion.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        artifact = stage / "artifact"
        require(digest(artifact / "manifest.json") == expected_manifest, "manifest changed")
        manifest = json.loads((artifact / "manifest.json").read_text())
        require(manifest["commit"] == sha and manifest["ui_source_tree"] == ui_tree and
                digest(artifact / "reposync-daemon") == manifest["binary_sha256"] and
                manifest_files(artifact / "static") == manifest["static"], "artifact identity mismatch")
        binary = repo / "target/release/reposync-daemon"
        service_path(binary)
        original = json.loads((stage / "input.json").read_text())["previous"]
        require(live_state(binary) == original, "deployment changed since preparation")
        save(stage / "promotion-intent.json", {"previous": original, "commit": sha,
                                               "manifest_sha256": expected_manifest})
        binary.parent.mkdir(parents=True, exist_ok=True)
        if binary.exists() and not binary.is_symlink():
            shutil.copy2(binary, stage / "previous-daemon")
        link = binary.parent / (".reposync-link-" + stage.name)
        link.symlink_to(artifact / "reposync-daemon")
        os.replace(link, binary)
        fd = os.open(binary.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
        # No previous release/static files are removed, even after success.
        command(["sudo", "-n", "systemctl", "restart", "reposync"])
        health = None
        for _ in range(12):
            try:
                health = json.loads(output(["curl", "--fail", "--silent", "--show-error", "--max-time", "5",
                                            "http://127.0.0.1:8080/api/status/health"]))
                if isinstance(health, dict) and health.get("ok") is True:
                    break
            except (subprocess.CalledProcessError, ValueError):
                pass
            time.sleep(1)
        require(isinstance(health, dict) and health.get("ok") is True and health.get("git_commit") == manifest["short"],
                "health/build identity failed after promotion; reconcile, do not blindly roll back")
        pid = output(["systemctl", "show", "reposync", "--property=MainPID", "--value"])
        require(pid.isdigit() and int(pid) > 0, "missing service process identity")
        require(digest(Path("/proc") / pid / "exe") == manifest["binary_sha256"],
                "running service binary differs from sealed artifact")
        require(live_state(binary)["sha256"] == manifest["binary_sha256"], "promoted pointer changed")
        save(stage / "verified.json", {"commit": sha, "manifest_sha256": expected_manifest,
                                      "binary_sha256": manifest["binary_sha256"]})
        return {"verified": True, "manifest_sha256": expected_manifest,
                "binary_sha256": manifest["binary_sha256"], "pid": pid}


def main(argv):
    require(sys.platform == "linux", "remote helper supports Linux only")
    require(len(argv) >= 4, "missing helper arguments")
    phase, location, sha, ui_tree, *values = argv
    require(re.fullmatch(r"[0-9a-f]{40}", sha) and re.fullmatch(r"[0-9a-f]{40}", ui_tree),
            "invalid immutable identity")
    repo = Path(location)
    if not repo.is_absolute():
        repo = Path.home() / repo
    repo = repo.resolve(strict=True)
    store = repo.parent / (".reposync-deploy-" + repo.name)
    os.umask(0o077)
    store.mkdir(mode=0o700, exist_ok=True)
    require(not store.is_symlink() and store.stat().st_uid == os.getuid() and
            stat.S_IMODE(store.stat().st_mode) == 0o700, "unsafe deployment staging directory")
    if phase == "prepare":
        require(not values, "unexpected preparation arguments")
        result = prepare(repo, store, sha, ui_tree)
    else:
        require(len(values) == 2, "missing stage/digest")
        stage = Path(values[0])
        require(stage.resolve(strict=True).parent == store and not stage.is_symlink(), "foreign staging path")
        if phase == "build":
            result = build(stage, sha, ui_tree, values[1])
        elif phase == "seal":
            result = seal(repo, stage, sha, ui_tree, values[1])
        elif phase == "activate":
            result = activate(repo, store, stage, sha, ui_tree, values[1])
        else:
            raise ValueError("unknown deployment phase")
    print(json.dumps(dict(result, commit=sha, phase=phase), sort_keys=True))


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as exc:
        print(f"ERROR: remote deployment stopped: {exc}; retain staging and prior releases", file=sys.stderr)
        sys.exit(1)
