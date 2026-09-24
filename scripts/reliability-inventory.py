#!/usr/bin/env python3
"""Read-only inventory of a sealed, quiesced RepoSync fixture copy.

Seal only after the fixture writer exits. The seal is local integrity metadata,
not a certificate that an arbitrary live installation was quiesced.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import sys
import tomllib
from urllib.parse import quote, urlsplit

PINNED_SOURCE = "87379741779a6259f7eeb52a68cc6f061174e5ef"
EXPECTED_SCHEMA = 12
OID = re.compile(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}")


def fail(message):
    raise ValueError(message)


def file_manifest(root):
    if not root.is_dir() or root.is_symlink():
        fail("fixture copy must be an existing ordinary directory")
    entries = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            fail("fixture copy contains a symlink")
        if path.is_dir():
            continue
        if not path.is_file():
            fail("fixture copy contains a non-file entry")
        rel = path.relative_to(root).as_posix()
        entries[rel] = {
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "mode": path.stat().st_mode & 0o7777,
        }
    for required in ("reposync.db", "config.toml"):
        if required not in entries:
            fail("incomplete fixture copy: missing " + required)
    if any(name.startswith("reposync.db-") for name in entries):
        fail("SQLite WAL/SHM sidecar present; use a consistent quiesced snapshot")
    return entries


def seal(root):
    before = file_manifest(root)
    after = file_manifest(root)
    if before != after:
        fail("fixture changed while sealing")
    return {"version": 1, "kind": "quiesced_fixture_copy",
            "pinned_source": PINNED_SOURCE, "files": before}


def endpoint(value):
    if not value:
        return None
    if "://" not in value:
        return {"name": value if not re.search(r"(?i)(token|password|secret|@|ghp_|github_pat_)", value)
                else "[REDACTED_ENDPOINT_NAME]"}
    parsed = urlsplit(value)
    # Never return userinfo, query or fragment; the path identifies the
    # fixture endpoint without repeating an embedded credential.
    path = parsed.path if not re.search(r"(?i)(token|password|secret|ghp_|github_pat_)", parsed.path) \
        else "[REDACTED_ENDPOINT_PATH]"
    return {"scheme": parsed.scheme, "host": parsed.hostname or "", "path": path}


def oid(value):
    return isinstance(value, str) and OID.fullmatch(value) is not None


def receipt_view(raw, rid, sha, policy):
    try:
        value = json.loads(raw)
    except (TypeError, ValueError):
        return {"sha": sha, "status": "malformed"}
    if not isinstance(value, dict):
        return {"sha": sha, "status": "malformed"}
    target = value.get("target")
    return {
        "sha": sha, "version": value.get("version"),
        "outcome": value.get("outcome"),
        "owner_matches": value.get("repo_id") == rid and value.get("git_sha") == sha,
        "policy_matches": value.get("projection") == policy,
        "target_proof_present": isinstance(target, dict)
            and isinstance(target.get("svn_revision"), int)
            and isinstance(target.get("svn_uuid"), str)
            and isinstance(target.get("paths"), dict),
    }


def local_ref(root, rid, branch):
    git = root / "repos" / rid / "git-repo" / ".git"
    ref = git / "refs" / "heads" / branch
    if ref.is_file() and not ref.is_symlink():
        value = ref.read_text().strip()
        return value if oid(value) else None
    return None


def inspect(root, expected):
    if file_manifest(root) != expected["files"]:
        fail("fixture copy differs from its quiesced seal")
    db_path = root / "reposync.db"
    uri = "file:" + quote(str(db_path)) + "?mode=ro&immutable=1"
    db = sqlite3.connect(uri, uri=True)
    db.row_factory = sqlite3.Row
    try:
        db.execute("PRAGMA query_only = ON")
        schema = db.execute("PRAGMA user_version").fetchone()[0]
        if schema != EXPECTED_SCHEMA:
            fail(f"unsupported schema {schema}; expected {EXPECTED_SCHEMA}")
        if db.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
            fail("SQLite integrity check failed")
        required = {"repositories", "kv_state", "sync_records", "commit_map", "import_progress"}
        tables = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        if not required <= tables:
            fail("incomplete fixture database")
        config = tomllib.loads((root / "config.toml").read_text())
        keys = {row["key"]: row["value"] for row in db.execute("SELECT key, value FROM kv_state")}
        repos = []
        rows = db.execute("SELECT id, name, enabled, parent_id, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch, last_svn_rev, last_git_sha, allowed_paths, blocked_patterns, sync_status FROM repositories ORDER BY id").fetchall()
        for row in rows:
            rid = row["id"]
            column = row["last_git_sha"] or None
            scoped = keys.get("last_git_sha_" + rid) or None
            policy = json.dumps({"allowed_paths": json.loads(row["allowed_paths"] or "[]"),
                                 "blocked_patterns": json.loads(row["blocked_patterns"] or "[]")},
                                separators=(",", ":"), sort_keys=True)
            mappings = [dict(x) for x in db.execute(
                "SELECT direction, status, svn_rev, git_sha FROM sync_records WHERE repo_id = ? ORDER BY svn_rev, git_sha, direction", (rid,))]
            old_maps = [dict(x) for x in db.execute(
                "SELECT direction, svn_rev, git_sha FROM commit_map WHERE repo_id = ? ORDER BY svn_rev, git_sha", (rid,))]
            receipts = [receipt_view(raw, rid, key[len("handled_git_no_target_" + rid + "_"):], policy)
                        for key, raw in sorted(keys.items()) if key.startswith("handled_git_no_target_" + rid + "_")]
            baseline_raw = keys.get("handled_git_baseline_" + rid)
            baseline = None
            if baseline_raw is not None:
                try:
                    record = json.loads(baseline_raw)
                    baseline = {"sha": record.get("git_sha"), "svn_rev": record.get("svn_rev"),
                                "owner_matches": record.get("repo_id") == rid,
                                "policy_matches": record.get("projection") == policy}
                except (TypeError, ValueError, AttributeError):
                    baseline = {"status": "malformed"}
            applied_import = any(x["direction"] == "svn_to_git" and x["status"] == "applied"
                                 and x["git_sha"] == column and x["svn_rev"] <= row["last_svn_rev"]
                                 for x in mappings)
            applied_outbound = any(x["direction"] == "git_to_svn" and x["status"] == "applied"
                                   and x["git_sha"] == column for x in mappings)
            ref = local_ref(root, rid, row["git_branch"])
            missing = []
            if not oid(column): missing.append("repository_column_missing_or_malformed")
            if scoped != column: missing.append("cursor_copies_differ_or_missing")
            if not applied_import: missing.append("scoped_import_mapping_missing")
            if column and column == scoped and not applied_import and not applied_outbound \
                    and not any(r.get("sha") == column and r.get("owner_matches") for r in receipts):
                missing.append("no_target_receipt_or_applied_mapping_missing")
            if ref != column: missing.append("local_git_ref_unproved")
            if any(r.get("status") == "malformed" or not r.get("owner_matches")
                   or not r.get("policy_matches") or (r.get("outcome") == "no_svn_delta"
                   and not r.get("target_proof_present")) for r in receipts):
                missing.append("receipt_requires_reconciliation")
            if keys.get("effect_unknown_" + rid):
                classification = "external_effect_unknown"
            elif not row["enabled"]:
                classification = "not_qualified"
                missing.append("repository_disabled")
            elif missing:
                classification = "needs_reconciliation"
            else:
                classification = "qualified_fixture_shape"
            repos.append({
                "id": rid, "name": row["name"], "enabled": bool(row["enabled"]),
                "parent_id": row["parent_id"], "sync_status": row["sync_status"],
                "source": {"svn_endpoint": endpoint(row["svn_url"]), "svn_branch": row["svn_branch"],
                           "svn_uuid": "UNKNOWN_LOCAL_ONLY"},
                "target": {"git_provider": row["git_provider"], "git_api": endpoint(row["git_api_url"]),
                           "git_repository": endpoint(row["git_repo"]), "git_branch": row["git_branch"],
                           "local_ref": ref},
                "policy": {"allowed_paths": json.loads(row["allowed_paths"] or "[]"),
                           "blocked_patterns": json.loads(row["blocked_patterns"] or "[]")},
                "checkpoints": {"repository_svn_revision": row["last_svn_rev"],
                                "repository_git_column": column, "scoped_git_kv": scoped,
                                "legacy_global_git_kv_reference": keys.get("last_git_hash")},
                "applied_mappings": mappings, "legacy_commit_map": old_maps,
                "baseline_receipt": baseline, "no_target_receipts": receipts,
                "credentials": {"svn_username_present": bool(row["svn_username"]),
                                "repo_svn_secret_present": "secret_svn_password_" + rid in keys,
                                "repo_git_secret_present": "secret_git_token_" + rid in keys,
                                "global_svn_password_env": config.get("svn", {}).get("password_env"),
                                "global_git_token_env": config.get("github", {}).get("token_env"),
                                "inheritance": "repository_secret_or_config_env_reference"},
                "missing_proof": sorted(set(missing + ["remote_svn_uuid_and_copy_ancestry_unknown",
                                                        "remote_git_ref_identity_unknown"])),
                "classification": classification,
            })
        result = {"version": 1, "input_kind": "sealed_quiesced_fixture_copy",
                  "pinned_source": expected["pinned_source"], "schema": schema,
                  "input_unchanged": True, "file_count": len(expected["files"]),
                  "production_eligibility": "NOT_ESTABLISHED", "repositories": repos}
    finally:
        db.close()
    if file_manifest(root) != expected["files"]:
        fail("fixture input changed during inventory")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--seal-copy", type=Path)
    group.add_argument("--copy", type=Path)
    parser.add_argument("--manifest", type=Path)
    args = parser.parse_args()
    if args.seal_copy:
        print(json.dumps(seal(args.seal_copy), sort_keys=True, separators=(",", ":")))
    else:
        if not args.manifest:
            fail("--manifest is required for inventory")
        expected = json.loads(args.manifest.read_text())
        if expected.get("version") != 1 or expected.get("kind") != "quiesced_fixture_copy" \
                or expected.get("pinned_source") != PINNED_SOURCE or not isinstance(expected.get("files"), dict):
            fail("unsupported or incomplete fixture seal")
        print(json.dumps(inspect(args.copy, expected), sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, sqlite3.Error, OSError, UnicodeError) as error:
        print("inventory refused: " + str(error), file=sys.stderr)
        sys.exit(2)
