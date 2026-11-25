#!/usr/bin/env python3
"""Package compiled tests and explicit runtime prerequisites, never a checkout."""
import argparse
import hashlib
import json
import os
import shutil
import stat
import sys
import tempfile
from pathlib import Path

APPLICATIONS = ("reposync", "reposync-personal")
FIXTURES = ("git_http_auth_server.py", "pair_refresh_http_server.py")
# Reviewed main blobs; fallback scripts must match both Git blob and SHA-256.
FIXTURE_ANCHOR = {
    "source_head": "7d4c8f81a6faeb664956bbe4c56c4a0da2fb1c65",
    "source_tree": "0d9d37853e0df188bb5ec84752d0c78adda46caf",
    "fixtures": {
        "git_http_auth_server.py": {"git_blob_sha1": "3b856fdd546e005613eb73e7e60d2a9470ec224b", "sha256": "ed5eed6797a2ca2b35cac5645293a44e90d1f77e409b95353c1465ae15083f33"},
        "pair_refresh_http_server.py": {"git_blob_sha1": "e690a127cd40b5172ef9ea86d5e4379a6576fc6e", "sha256": "5417a82fa1e72a65b5e43d74aaca45cf61ba471a10583dfe17c15d5d94521bd9"},
    },
}


# Exact reviewed native sources; these records never authorize fixture substitution.
NATIVE_ABSENCE_REGISTRY = [{'policy': 'native-absence-213-v1',
  'prepared_crates_file_count': 85,
  'prepared_crates_inventory_sha256': 'afd5bf52410391a409c08cc75cb178e4301db3b14c9d48b03ec80ceafe4804d7',
  'reviewed_overlay': {'files': [{'archive_sha256': '0663170a667903bd0ee2fd99f0e0d43c81c682029d1072cdb5eeae1cd0fdd65e',
                                  'path': 'crates/core/tests/team_mode_e2e.rs',
                                  'prepared_git_blob_sha1': '274fca0ba7d348fba1445ca49fc9ae0709bbfcd3',
                                  'prepared_sha256': 'fa8b4c40a6eee781cd19b8ab0056b0352656eaa04b3a133b6cc8d9615dc09904'}],
                       'id': 'svn-author-fixture-v1'},
  'role': 'base',
  'runtime_mode': 'baseline',
  'source_head': '87379741779a6259f7eeb52a68cc6f061174e5ef',
  'source_tree': 'f383bb9b123f3f75b2dde7f17f7311a264946abd'},
 {'policy': 'native-absence-213-v1',
  'prepared_crates_file_count': 85,
  'prepared_crates_inventory_sha256': '7624114405fa67ba108adf87f5f5c2129014c8ccde4be4a07e62f9c6f8c8d37b',
  'reviewed_overlay': {'files': [], 'id': 'none'},
  'role': 'previous',
  'runtime_mode': 'baseline',
  'source_head': 'f74fce855a1f1d80dd631397f436ba33906272e6',
  'source_tree': '2819ed3cd27d0dad13847e481e18f550aac15d06'},
 {'policy': 'native-absence-213-v1',
  'prepared_crates_file_count': 96,
  'prepared_crates_inventory_sha256': '9899774b12d7a0ef1a07755f9c2c6bde2ae9e2865366522819a2fb1318ff8877',
  'reviewed_overlay': {'files': [], 'id': 'none'},
  'role': 'immediate',
  'runtime_mode': 'baseline',
  'source_head': 'ab097580d9415ae2f8df44bed4041f740a1b38f2',
  'source_tree': '355bba3f3bd37009183c490f41a52cbea2ac68f9'},
 {'policy': 'native-absence-213-v1',
  'prepared_crates_file_count': 98,
  'prepared_crates_inventory_sha256': 'b2322560f1c0477fce31782d95a41b7a820ca9a189e3dd1807d53e62d3189e30',
  'reviewed_overlay': {'files': [], 'id': 'none'},
  'role': 'merged',
  'runtime_mode': 'baseline',
  'source_head': '54e183484961bd9ad12bd03505b3b00610de7cd9',
  'source_tree': '8d1c7a38239634e8ac830588cb6b09a93badd635'}]
CORE_FIXTURE_PATHS = (
    "crates/core/tests/fixtures/git_http_auth_server.py",
    "crates/core/tests/fixtures/pair_refresh_http_server.py",
)
STARTUP_FIXTURE_PATH = "crates/cli/tests/fixtures/startup_provider.py"


def strict_json_loads(text):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate input JSON key: {key}")
            result[key] = value
        return result
    def invalid_constant(value):
        raise ValueError(f"invalid input JSON constant: {value}")
    return json.loads(text, object_pairs_hook=unique, parse_constant=invalid_constant)


def native_admission(value):
    if value is None:
        return None
    # Canonical comparison also rejects numeric type changes such as 85 -> 85.0.
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":"))
    for record in NATIVE_ABSENCE_REGISTRY:
        if encoded == json.dumps(record, sort_keys=True, separators=(",", ":")):
            return record
    raise ValueError("unknown historical source/role/overlay admission")


def absent_record(relative):
    return {"path": relative, "origin": "source_absent",
            "archive": {"state": "absent"}, "fallback_allowed": False}


def require_absent(root, relative):
    path = root / relative
    for ancestor in (path, *path.parents):
        if ancestor == root.parent:
            break
        if ancestor.is_symlink():
            raise ValueError(f"absent fixture path is a symlink: {ancestor}")
    try:
        path.lstat()
    except FileNotFoundError:
        return
    raise ValueError(f"source-absent fixture is present: {relative}")


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def blob_identity(path):
    data = path.read_bytes()
    return {"sha256": hashlib.sha256(data).hexdigest(),
            "git_blob_sha1": hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()}


STAGED_RECEIPT = "crates/core/tests/fixtures/reliability-inputs.json"


def prepared_crates_inventory(root, *, exclude_receipt=False):
    crates = root / "crates"
    if root.is_symlink() or crates.is_symlink() or not crates.is_dir():
        raise ValueError("invalid prepared crate root")
    rows = []
    def failed(error):
        raise error
    for directory, directories, files in os.walk(crates, followlinks=False, onerror=failed):
        for name in directories:
            path = Path(directory) / name
            if not stat.S_ISDIR(path.lstat().st_mode):
                raise ValueError(f"invalid prepared crate directory: {path}")
        for name in files:
            path = Path(directory) / name
            info = path.lstat()
            if not stat.S_ISREG(info.st_mode):
                raise ValueError(f"invalid prepared crate file: {path}")
            relative = path.relative_to(root).as_posix()
            if exclude_receipt and relative == STAGED_RECEIPT:
                continue
            rows.append([relative, "100755" if info.st_mode & 0o111 else "100644",
                         blob_identity(path)["git_blob_sha1"]])
    rows.sort(key=lambda row: row[0])
    data = json.dumps(rows, ensure_ascii=True, separators=(",", ":")).encode()
    return hashlib.sha256(data).hexdigest(), len(rows)


def verify_prepared_admission(root, admission, *, exclude_receipt=False):
    expected = (admission["prepared_crates_inventory_sha256"],
                admission["prepared_crates_file_count"])
    if prepared_crates_inventory(root, exclude_receipt=exclude_receipt) != expected:
        raise ValueError("prepared crates differ from reviewed native source/overlay")
    for relative in CORE_FIXTURE_PATHS:
        require_absent(root, relative)


def staging_admission(context):
    head = os.environ.get("REPOSYNC_SOURCE_HEAD_OVERRIDE")
    tree = os.environ.get("REPOSYNC_SOURCE_TREE_OVERRIDE")
    for record in NATIVE_ABSENCE_REGISTRY:
        if (head, tree) == (record["source_head"], record["source_tree"]):
            require_absent(context, STAGED_RECEIPT)
            verify_prepared_admission(context, record)
            return record
    return None


def prepare_fixture_inputs(root, fixtures):
    receipt = fixtures / "reliability-inputs.json"
    # Bind package's direct production path to staging too. An existing receipt
    # must validate unchanged; never regenerate one to hide missing/tampered bytes.
    if not receipt.exists() and not receipt.is_symlink():
        stage_fixtures(root, root, receipt)
    if receipt.is_symlink() or not receipt.is_file():
        raise ValueError("invalid staged fixture receipt")
    inputs = strict_json_loads(receipt.read_text())
    if (not isinstance(inputs, dict)
            or set(inputs) != {"schema", "reviewed_anchor", "historical_admission", "fixtures"}
            or type(inputs["schema"]) is not int or inputs["schema"] != 2 or inputs["reviewed_anchor"] != FIXTURE_ANCHOR
            or not isinstance(inputs["fixtures"], dict)
            or set(inputs["fixtures"]) != set(FIXTURES)):
        raise ValueError("invalid staged fixture provenance/schema")
    admission = native_admission(inputs["historical_admission"])
    absent = []
    for name, item in inputs["fixtures"].items():
        if not isinstance(item, dict):
            raise ValueError("invalid fixture provenance entry")
        if item.get("origin") == "source_absent":
            if item != {"origin": "source_absent", "archive": {"state": "absent"},
                        "fallback_allowed": False} or item["fallback_allowed"] is not False:
                raise ValueError("malformed native fixture absence")
            absent.append(name)
    if absent:
        if admission is None or set(absent) != set(FIXTURES):
            raise ValueError("core fixture absence lacks complete historical admission")
        verify_prepared_admission(root, admission, exclude_receipt=True)
    elif admission is not None:
        raise ValueError("historical admission without native core absence")
    return inputs


def stage_fixtures(context, source, receipt, allowed_fallbacks=()):
    # A reviewed content hash is identity evidence, not substitution authority.
    # Only an explicit caller capability may select a fallback, and the caller
    # must separately hold the corresponding source/role substitution approval.
    allowed_fallbacks = frozenset(allowed_fallbacks)
    if not allowed_fallbacks <= set(FIXTURES):
        raise ValueError("unknown anchored fallback capability")
    target = context / "crates/core/tests/fixtures"
    if target.resolve() != context.resolve() / "crates/core/tests/fixtures":
        raise ValueError("fixture staging escaped source context")
    target.mkdir(parents=True, exist_ok=True)
    admission = staging_admission(context)
    if admission is not None and allowed_fallbacks:
        raise ValueError("native absence admission cannot select anchored fallback")
    inputs = {"schema": 2, "reviewed_anchor": FIXTURE_ANCHOR,
              "historical_admission": admission, "fixtures": {}}
    for name in FIXTURES:
        path = target / name
        if path.is_symlink():
            raise ValueError(f"symlink runtime fixture: {path}")
        capability = name in allowed_fallbacks
        if path.exists():
            if not path.is_file():
                raise ValueError(f"invalid runtime fixture: {path}")
            identity = blob_identity(path)
            archived = {"state": "present", **identity}
            origin = "source_archive"
        else:
            archived = {"state": "absent"}
            if not capability:
                inputs["fixtures"][name] = {"origin": "source_absent", "archive": archived,
                                            "fallback_allowed": False}
                continue
            fallback = source / "crates/core/tests/fixtures" / name
            if fallback.is_symlink() or not fallback.is_file():
                raise ValueError(f"invalid fallback runtime fixture: {fallback}")
            if blob_identity(fallback) != FIXTURE_ANCHOR["fixtures"][name]:
                raise ValueError(f"fallback fixture differs from reviewed head blob: {name}")
            shutil.copy2(fallback, path)
            origin = "reviewed_harness_fallback"
            identity = blob_identity(path)
        inputs["fixtures"][name] = {"origin": origin, "archive": archived,
                                    "fallback_allowed": capability, **identity}
    serialized = json.dumps(inputs, indent=2, sort_keys=True) + "\n"
    receipt.write_text(serialized)
    (target / "reliability-inputs.json").write_text(serialized)


def artifacts(source):
    for line in source.read_text().splitlines():
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            continue
        if item.get("reason") == "compiler-artifact" and item.get("executable"):
            yield item


def package(source, destination, applications=None, fixtures=None, runtime_root=None):
    destination.mkdir(parents=True, exist_ok=True)
    if destination.is_symlink():
        raise ValueError("application package root is a symlink")
    manifest = {}
    for item in artifacts(source):
        if not item.get("profile", {}).get("test"):
            continue
        name = item["target"]["name"]
        if name in manifest:
            raise ValueError(f"ambiguous test executable name: {name}")
        executable = Path(item["executable"])
        relative = Path("deps") / executable.name if applications else Path(executable.name)
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(executable, target)
        manifest[name] = relative.as_posix()
        if name == "reposync_core":
            manifest["reposync-core"] = relative.as_posix()
    if not manifest:
        raise ValueError("no Rust test executables were produced")
    if applications:
        selected = {}
        # Preserve PR214's final same-path Cargo artifact selection.
        for item in list(artifacts(applications)) + list(artifacts(source)):
            name = item["target"]["name"]
            if name not in APPLICATIONS or item.get("profile", {}).get("test"):
                continue
            if item["target"].get("kind") != ["bin"]:
                continue
            executable = Path(item["executable"])
            if executable.name != name or executable.parent.name != "debug" or executable.is_symlink():
                raise ValueError(f"unexpected application artifact: {executable}")
            if name in selected and selected[name]["source"] != str(executable):
                raise ValueError(f"ambiguous application artifact: {name}")
            selected[name] = {"source": str(executable), "features": item.get("features", []),
                              "cargo_package_id": item["package_id"]}
        if set(selected) != set(APPLICATIONS):
            raise ValueError(f"missing required application: {sorted(set(APPLICATIONS) - set(selected))}")
        root = Path(runtime_root) if runtime_root is not None else Path(selected[APPLICATIONS[0]]["source"]).parents[2]
        if fixtures != root / "crates/core/tests/fixtures":
            raise ValueError("runtime fixture path differs from the canonical source root")
        staging = destination.parent / "reliability-runtime-inputs"
        for crate in ("core", "cli", "personal", "web"):
            (staging / "crates" / crate).mkdir(parents=True, exist_ok=True)
        files = {}
        def stage(path, relative, kind, **metadata):
            path.relative_to(root)
            for ancestor in (path, *path.parents):
                if ancestor == root.parent:
                    break
                if ancestor.is_symlink():
                    raise ValueError(f"runtime input is a symlink: {ancestor}")
            if not path.is_file():
                raise ValueError(f"runtime input missing or not a regular file: {path}")
            mode = stat.S_IMODE(path.stat().st_mode)
            if kind == "application" and not mode & 0o111:
                raise ValueError(f"application subject is not executable: {path}")
            target = staging / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
            data = target.read_bytes()
            files[relative] = {"kind": kind, "bytes": len(data),
                               "sha256": hashlib.sha256(data).hexdigest(),
                               "mode": mode, **metadata}
        provenance = prepare_fixture_inputs(root, fixtures)
        absent = []
        for name, item in selected.items():
            path = root / "target/debug" / name
            if str(path) != item["source"]:
                raise ValueError(f"unexpected application path for {name}: {item['source']}")
            stage(path, f"target/debug/{name}", "application", cargo_target=name,
                  cargo_package_id=item["cargo_package_id"], features=item["features"],
                  packaged_path=name)
            # Personal tests discover the sibling application beside deps/.
            # Keep a byte/mode-bound regular copy; do not weaken /src's no-symlink guard.
            shutil.copy2(path, destination / name)
        for name in FIXTURES:
            path = fixtures / name
            staged = provenance["fixtures"][name]
            if staged["origin"] == "source_absent":
                relative = f"crates/core/tests/fixtures/{name}"
                require_absent(root, relative)
                require_absent(staging, relative)
                absent.append(absent_record(relative))
                continue
            if set(staged) != {"origin", "archive", "fallback_allowed", "sha256", "git_blob_sha1"}:
                raise ValueError(f"invalid present fixture provenance: {name}")
            if path.is_symlink() or not path.is_file():
                raise ValueError(f"missing or invalid runtime fixture: {name}")
            identity = blob_identity(path)
            staged = provenance["fixtures"][name]
            if identity != {key: staged[key] for key in identity}:
                raise ValueError(f"runtime fixture differs from staged input: {name}")
            if type(staged["fallback_allowed"]) is not bool:
                raise ValueError(f"invalid fallback capability receipt: {name}")
            if staged["origin"] == "reviewed_harness_fallback":
                if (not staged["fallback_allowed"] or staged["archive"] != {"state": "absent"}
                        or identity != FIXTURE_ANCHOR["fixtures"][name]):
                    raise ValueError(f"runtime fallback lacks capability or differs from reviewed blob: {name}")
            elif staged["origin"] == "source_archive":
                if staged["archive"] != {"state": "present", **identity}:
                    raise ValueError(f"runtime fixture differs from archived source: {name}")
            else:
                raise ValueError(f"unknown runtime fixture origin: {name}")
            stage(path, f"crates/core/tests/fixtures/{name}", "fixture",
                  fixture_provenance={"reviewed_anchor": FIXTURE_ANCHOR, **staged})
        # Preserve ed56's archive-only startup provider policy: no fallback and
        # no substitution into historical sources in which it did not exist.
        relative = "crates/cli/tests/fixtures/startup_provider.py"
        path = root / relative
        if not path.exists() and not path.is_symlink():
            require_absent(staging, relative)
            absent.append(absent_record(relative))
        else:
            stage(path, relative, "fixture", fixture_provenance={
                "origin": "source_archive", "archive": {"state": "present", **blob_identity(path)},
                "fallback_allowed": False})
        inputs = {"schema": 2, "files": files,
                  "absent_fixtures": sorted(absent, key=lambda item: item["path"]),
                  "historical_admission": provenance["historical_admission"]}
        (destination / "runtime-inputs.json").write_text(json.dumps(inputs, indent=2, sort_keys=True) + "\n")
    (destination / "binaries.json").write_text(json.dumps(manifest, sort_keys=True, indent=2) + "\n")
    print(f"Packaged {len(manifest)} Rust test identities")



def native_absence_self_test():
    """Source controls need the four local historical Git object closures; no skip."""
    import copy
    import runpy
    import subprocess
    from unittest.mock import patch

    repo = Path(__file__).resolve().parents[1]
    runtime = runpy.run_path(str(repo / "scripts/reliability-runtime.py"))
    assert runtime["NATIVE_ABSENCE_REGISTRY"] == NATIVE_ABSENCE_REGISTRY
    def refuses(operation):
        try:
            operation()
        except (ValueError, AssertionError, FileNotFoundError, KeyError):
            return
        raise AssertionError("invalid native absence accepted")
    for text in ('{"schema":2,"schema":2}', '{"value":NaN}'):
        refuses(lambda: strict_json_loads(text))
        refuses(lambda: runtime["strict_json_loads"](text))
    with tempfile.TemporaryDirectory(prefix="reposync-native-absence-") as temporary:
        for admission in NATIVE_ABSENCE_REGISTRY:
            case = Path(temporary) / admission["role"]
            context = case / "source"
            context.mkdir(parents=True)
            # Reconstruct exact immutable native bytes; do not fabricate a passing
            # inventory or use the current candidate as a historical fixture.
            listing = subprocess.check_output(["git", "-C", str(repo), "ls-tree", "-r", "-z",
                admission["source_head"], "--", "crates"])
            for row in listing.split(b"\0"):
                if not row:
                    continue
                metadata, raw_path = row.split(b"\t", 1)
                mode, kind, blob = metadata.decode().split()
                assert kind == "blob" and mode in ("100644", "100755")
                path = context / raw_path.decode()
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(subprocess.check_output(["git", "-C", str(repo), "cat-file", "blob", blob]))
                path.chmod(0o755 if mode == "100755" else 0o644)
            if admission["role"] == "base":
                overlay = context / "crates/core/tests/team_mode_e2e.rs"
                original = overlay.read_text()
                needle = '            message,\n            wc_path.to_str().unwrap(),\n            "--non-interactive",'
                assert original.count(needle) == 1
                # The unadapted base is not the admitted reviewed overlay.
                refuses(lambda: verify_prepared_admission(context, admission))
                overlay.write_text(original.replace(needle,
                    '            message,\n            wc_path.to_str().unwrap(),\n            "--username",\n            "fixture",\n            "--non-interactive",'))
            verify_prepared_admission(context, admission)
            debug = context / "target/debug"
            (debug / "deps").mkdir(parents=True)
            test = debug / "deps/reposync_core-control"
            test.write_text("compiled-test artifact control; never executed")
            def artifact(name, executable, is_test=False):
                return {"reason": "compiler-artifact", "target": {"name": name, "kind": ["bin"]},
                        "profile": {"test": is_test}, "executable": str(executable),
                        "features": [], "package_id": "native-absence-control"}
            applications = []
            for name in APPLICATIONS:
                app = debug / name
                app.write_text("application artifact control; never executed")
                app.chmod(0o755)
                applications.append(artifact(name, app))
            tests, apps = context / "tests.jsonl", context / "apps.jsonl"
            tests.write_text(json.dumps(artifact("reposync_core", test, True)) + "\n")
            apps.write_text("\n".join(json.dumps(item) for item in applications) + "\n")
            fixtures = context / "crates/core/tests/fixtures"
            destination = case / "package"
            overrides = {"REPOSYNC_SOURCE_HEAD_OVERRIDE": admission["source_head"],
                         "REPOSYNC_SOURCE_TREE_OVERRIDE": admission["source_tree"]}
            with patch.dict(os.environ, overrides):
                if admission["role"] == "previous":
                    stage_fixtures(context, repo, case / "external-staging.json")
                # Other roles exercise package -> missing receipt -> stage first.
                package(tests, destination, apps, fixtures, context)
            inputs = strict_json_loads((destination / "runtime-inputs.json").read_text())
            assert inputs["historical_admission"] == admission
            assert {item["path"] for item in inputs["absent_fixtures"]} >= set(CORE_FIXTURE_PATHS)
            for relative in CORE_FIXTURE_PATHS:
                require_absent(context, relative)
            runtime_src = case / "runtime-src"
            shutil.copytree(case / "reliability-runtime-inputs", runtime_src)
            (runtime_src / "web-ui").mkdir()
            def validate(**changes):
                identity = {"source_head": admission["source_head"],
                            "source_tree": admission["source_tree"], "mode": "baseline"}
                identity.update(changes)
                return runtime["validate_runtime_inputs"](runtime_src, destination, **identity)
            validate()
            for mode in (None, "candidate", "diagnostics", "all"):
                refuses(lambda: validate(mode=mode))
            refuses(lambda: validate(source_head="unadmitted"))
            refuses(lambda: validate(source_tree="unadmitted"))
            # The runtime producer/consumer partition must reject every ambiguity.
            mutations = []
            def altered(change):
                value = copy.deepcopy(inputs)
                change(value)
                mutations.append(value)
            altered(lambda v: v.update(schema=1))
            altered(lambda v: v.update(schema=2.0))
            altered(lambda v: v.update(historical_admission=None))
            altered(lambda v: v["historical_admission"].update(role="unknown"))
            altered(lambda v: v["absent_fixtures"].append(copy.deepcopy(v["absent_fixtures"][0])))
            altered(lambda v: v["absent_fixtures"].pop())
            altered(lambda v: v["absent_fixtures"].__setitem__(0, v["absent_fixtures"][0]["path"]))
            altered(lambda v: v["absent_fixtures"][0].update(fallback_allowed=True))
            altered(lambda v: v["absent_fixtures"][0].update(extra="unknown"))
            altered(lambda v: v["absent_fixtures"][0].update(path="unexpected/path"))
            altered(lambda v: v["files"].update({CORE_FIXTURE_PATHS[0]: {}}))
            def partial(v):
                v["absent_fixtures"] = [x for x in v["absent_fixtures"] if x["path"] != CORE_FIXTURE_PATHS[0]]
                v["files"][CORE_FIXTURE_PATHS[0]] = {}
            altered(partial)
            for value in mutations:
                refuses(lambda: runtime["runtime_absences"](value))
            # Physical absence must hold even if its provenance object is valid.
            missing = runtime_src / CORE_FIXTURE_PATHS[0]
            created = [p for p in reversed(list(missing.parents))
                       if p != runtime_src.parent and runtime_src in p.parents and not p.exists()]
            missing.parent.mkdir(parents=True, exist_ok=True)
            missing.write_text("unapproved substitution")
            refuses(validate)
            missing.unlink()
            missing.symlink_to(case / "nonexistent")
            refuses(validate)
            missing.unlink()
            for directory in reversed(created):
                directory.rmdir()
            validate()
            # A canonical receipt is evidence, not permission to change native input.
            receipt = fixtures / "reliability-inputs.json"
            original_receipt = receipt.read_text()
            for field in ("role", "source_head", "source_tree", "reviewed_overlay",
                          "prepared_crates_inventory_sha256", "prepared_crates_file_count"):
                value = strict_json_loads(original_receipt)
                value["historical_admission"][field] = "unreviewed"
                receipt.write_text(json.dumps(value))
                refuses(lambda: prepare_fixture_inputs(context, fixtures))
            receipt.write_text(original_receipt)
            native = context / "crates/core/Cargo.toml"
            original, original_mode = native.read_bytes(), native.stat().st_mode
            native.write_bytes(original + b"\n# changed source")
            refuses(lambda: prepare_fixture_inputs(context, fixtures))
            native.write_bytes(original)
            native.chmod(stat.S_IMODE(original_mode) ^ 0o100)
            refuses(lambda: prepare_fixture_inputs(context, fixtures))
            native.chmod(stat.S_IMODE(original_mode))
            native.unlink()
            refuses(lambda: prepare_fixture_inputs(context, fixtures))
            native.write_bytes(original)
            native.chmod(stat.S_IMODE(original_mode))
            injected = fixtures / FIXTURES[0]
            injected.write_text("unapproved substitution")
            refuses(lambda: prepare_fixture_inputs(context, fixtures))
            injected.unlink()
            injected.symlink_to(case / "nonexistent")
            refuses(lambda: prepare_fixture_inputs(context, fixtures))
            injected.unlink()
            extra = context / "crates/unreviewed.rs"
            extra.write_text("unreviewed overlay")
            refuses(lambda: prepare_fixture_inputs(context, fixtures))
            extra.unlink()
            # Unknown/partial caller identity must not generate native admission.
            for identity in ({}, {"REPOSYNC_SOURCE_HEAD_OVERRIDE": admission["source_head"]},
                             {**overrides, "REPOSYNC_SOURCE_TREE_OVERRIDE": "wrong"}):
                receipt.unlink()
                with patch.dict(os.environ, identity, clear=True):
                    refuses(lambda: prepare_fixture_inputs(context, fixtures))
                receipt.write_text(original_receipt)
            prepare_fixture_inputs(context, fixtures)


def self_test():
    with tempfile.TemporaryDirectory(prefix="reposync-package-") as temporary:
        root = Path(temporary)
        debug = root / "target/debug"
        (debug / "deps").mkdir(parents=True)
        fixtures = root / "fixtures"
        fixtures.mkdir()
        fixture_source = Path(__file__).resolve().parents[1] / "crates/core/tests/fixtures"
        for name in FIXTURES:
            shutil.copy2(fixture_source / name, fixtures / name)
        # This synthetic control covers present archived fixture bytes.
        # Historical native absence requires exact source/overlay admission; no fallback is implied.
        fixture_context = root
        staged_root = fixture_context / "crates/core/tests/fixtures"
        staged_root.mkdir(parents=True)
        for name in FIXTURES:
            shutil.copy2(fixtures / name, staged_root / name)
        stage_fixtures(fixture_context, root, root / "staged.json")
        fixtures = staged_root
        test = debug / "deps/reposync_core-123"
        test.write_text("test binary control")
        def artifact(name, path, test=False):
            return {"reason": "compiler-artifact", "target": {"name": name, "kind": ["bin"]},
                    "profile": {"test": test}, "executable": str(path), "features": [], "package_id": "synthetic"}
        records = []
        for name in APPLICATIONS:
            (debug / name).write_text(name)
            (debug / name).chmod(0o755)
            records.append(artifact(name, debug / name))
        source, apps = root / "tests.jsonl", root / "apps.jsonl"
        source.write_text(json.dumps(artifact("reposync_core", test, True)) + "\n")
        def write_apps(items):
            apps.write_text("\n".join(json.dumps(i) for i in items) + "\n")
        write_apps(records)
        package(source, root / "valid", apps, fixtures)
        inputs = json.loads((root / "valid/runtime-inputs.json").read_text())
        startup_relative = "crates/cli/tests/fixtures/startup_provider.py"
        assert inputs["schema"] == 2 and inputs["absent_fixtures"] == [absent_record(startup_relative)]
        assert inputs["historical_admission"] is None
        for name in APPLICATIONS:
            entry = inputs["files"][f"target/debug/{name}"]
            assert entry["packaged_path"] == name and entry["mode"] == 0o755
            assert (root / "valid" / name).read_bytes() == (debug / name).read_bytes()
        startup = root / startup_relative
        startup.parent.mkdir(parents=True)
        startup.write_text("# historical startup provider control")
        package(source, root / "with-startup", apps, fixtures)
        enrolled = json.loads((root / "with-startup/runtime-inputs.json").read_text())
        assert enrolled["absent_fixtures"] == []
        assert enrolled["files"][startup_relative]["sha256"] == sha(startup)
        startup.unlink()
        manifest = json.loads((root / "valid/binaries.json").read_text())
        assert manifest == {"reposync_core": "deps/reposync_core-123", "reposync-core": "deps/reposync_core-123"}
        package(source, root / "legacy")
        assert json.loads((root / "legacy/binaries.json").read_text())["reposync_core"] == test.name
        def rejects(items, fixture_root=fixtures):
            write_apps(items)
            try:
                package(source, root / "rejected", apps, fixture_root)
            except (ValueError, FileNotFoundError):
                return
            raise AssertionError("invalid prerequisite accepted")
        provenance_file = fixtures / "reliability-inputs.json"
        original_provenance = provenance_file.read_text()
        invalid_provenance = json.loads(original_provenance)
        invalid_provenance["reviewed_anchor"]["source_head"] = "unreviewed"
        provenance_file.write_text(json.dumps(invalid_provenance))
        rejects(records)
        provenance_file.write_text(original_provenance)
        fixture = fixtures / FIXTURES[0]
        original_fixture = fixture.read_bytes()
        fixture.write_text("changed since staging")
        rejects(records)
        fixture.write_bytes(original_fixture)
        rejects(records[:1])
        other = root / "other/debug/reposync"
        other.parent.mkdir(parents=True)
        other.write_text("ambiguous")
        rejects(records + [artifact("reposync", other)])
        missing = fixtures / FIXTURES[0]
        missing.unlink()
        rejects(records)
        missing.symlink_to(fixtures / FIXTURES[1])
        rejects(records)
        harness = root / "harness"
        fixture_source = harness / "crates/core/tests/fixtures"
        fixture_source.mkdir(parents=True)
        for name in FIXTURES:
            shutil.copy2(Path(__file__).resolve().parents[1] / "crates/core/tests/fixtures" / name, fixture_source / name)
        context = root / "archive"
        existing = context / "crates/core/tests/fixtures" / FIXTURES[0]
        existing.parent.mkdir(parents=True)
        existing.write_text("original historical fixture")
        receipt = root / "fixture-inputs.json"
        stage_fixtures(context, harness, receipt)
        staged = json.loads(receipt.read_text())
        assert existing.read_text() == "original historical fixture"
        assert staged["fixtures"][FIXTURES[0]]["origin"] == "source_archive"
        assert staged["fixtures"][FIXTURES[0]]["archive"]["state"] == "present"
        assert staged["fixtures"][FIXTURES[1]] == {
            "origin": "source_absent", "archive": {"state": "absent"}, "fallback_allowed": False}
        missing = context / "crates/core/tests/fixtures" / FIXTURES[1]
        assert not missing.exists()
        # A metadata claim cannot cause a copy; the default caller capability is empty.
        forged = {"reviewed_anchor": FIXTURE_ANCHOR, "fixtures": {
            FIXTURES[1]: {"origin": "reviewed_harness_fallback", "fallback_allowed": True}}}
        (missing.parent / "reliability-inputs.json").write_text(json.dumps(forged))
        stage_fixtures(context, harness, receipt)
        assert not missing.exists()
        stage_fixtures(context, harness, receipt, allowed_fallbacks=(FIXTURES[1],))
        staged = json.loads(receipt.read_text())
        assert staged["fixtures"][FIXTURES[1]]["origin"] == "reviewed_harness_fallback"
        assert staged["fixtures"][FIXTURES[1]]["archive"] == {"state": "absent"}
        assert staged["fixtures"][FIXTURES[1]]["fallback_allowed"] is True
        assert existing.read_text() == "original historical fixture"
        (fixture_source / FIXTURES[1]).write_text("tampered fallback bytes")
        (context / "crates/core/tests/fixtures" / FIXTURES[1]).unlink()
        try:
            stage_fixtures(context, harness, receipt, allowed_fallbacks=(FIXTURES[1],))
        except ValueError as error:
            assert "reviewed head blob" in str(error)
        else:
            raise AssertionError("changed fallback blob accepted")
    native_absence_self_test()
    print("Runtime prerequisite packaging controls: PASS")


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
    elif sys.argv[1:2] == ["--stage-fixtures"]:
        parser = argparse.ArgumentParser(description="Record archived fixture provenance; no fallback by default")
        parser.add_argument("context", type=Path)
        parser.add_argument("source", type=Path)
        parser.add_argument("receipt", type=Path)
        parser.add_argument("--allow-anchored-fallback", action="append", choices=FIXTURES, default=[],
                            help="Explicit caller capability; requires separately recorded source/role substitution approval")
        args = parser.parse_args(sys.argv[2:])
        stage_fixtures(args.context, args.source, args.receipt, args.allow_anchored_fallback)
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("source", type=Path)
        parser.add_argument("destination", type=Path)
        parser.add_argument("--runtime-inputs", nargs=2, metavar=("SOURCE_ROOT", "APPLICATION_MESSAGES"))
        parser.add_argument("--applications", type=Path)
        parser.add_argument("--fixtures", type=Path)
        args = parser.parse_args()
        runtime_root = None
        if args.runtime_inputs:
            if args.applications or args.fixtures:
                parser.error("--runtime-inputs cannot be combined with --applications/--fixtures")
            runtime_root, args.applications = map(Path, args.runtime_inputs)
            args.fixtures = runtime_root / "crates/core/tests/fixtures"
        if bool(args.applications) != bool(args.fixtures):
            parser.error("--applications and --fixtures must be supplied together")
        package(args.source, args.destination, args.applications, args.fixtures, runtime_root)
