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
import stat
import sys
import tomllib
from urllib.parse import quote, urlsplit

VOCABULARY_PATH = Path(__file__).with_name("legacy-evidence-vocabulary.json")
if not VOCABULARY_PATH.exists():
    VOCABULARY_PATH = Path(__file__).resolve().parent.parent / "docs/reliability/legacy-evidence-vocabulary.json"
VOCABULARY = json.loads(VOCABULARY_PATH.read_text())


def receipt_owner(key, repo_ids):
    owners = [rid for rid in repo_ids if key.startswith(VOCABULARY["no_target_prefix"] + rid + "_")]
    return max(owners, key=len) if owners else None


PINNED_SOURCE = "87379741779a6259f7eeb52a68cc6f061174e5ef"
EXPECTED_SCHEMA = 12
OID = re.compile(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}")
SAFE_ID = re.compile(r"[A-Za-z0-9_-][A-Za-z0-9._-]*")
SAFE_REF_PART = re.compile(r"[A-Za-z0-9_-][A-Za-z0-9._-]*")


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


def safe_components(value, kind):
    if not isinstance(value, str) or not value or value.startswith("/") or "\\" in value:
        fail("unsupported " + kind)
    parts = value.split("/")
    if any(part in ("", ".", "..") or not SAFE_REF_PART.fullmatch(part)
           or part.endswith(".") or ".lock" in part for part in parts):
        fail("unsupported " + kind)
    return parts


def sealed_read(root, relative, files):
    """Open only a sealed regular file through no-follow directory descriptors."""
    if not isinstance(relative, str) or relative.startswith("/") or "\\" in relative or "\x00" in relative:
        fail("unsupported sealed file reference")
    parts = relative.split("/")
    if any(part in ("", ".", "..") for part in parts):
        fail("unsupported sealed file reference")
    if relative not in files:
        fail("requested file is outside the sealed input")
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for part in parts[:-1]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = child
        file_fd = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW, dir_fd=fd)
        try:
            if not stat.S_ISREG(os.fstat(file_fd).st_mode):
                fail("sealed file is not regular")
            with os.fdopen(file_fd, "rb", closefd=False) as stream:
                return stream.read()
        finally:
            os.close(file_fd)
    finally:
        os.close(fd)


def optional_sealed_read(root, relative, files):
    return sealed_read(root, relative, files) if relative in files else None


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
        "semantic_proof_present": value.get("version") == 3 and isinstance(target, dict)
            and target.get("semantic_projection") == "regular_file_bytes_no_properties_v1"
            and isinstance(target.get("paths"), dict)
            and all(entry is None or (isinstance(entry, dict)
                and entry.get("git_mode") == 33188 and entry.get("svn_executable") is False
                and isinstance(entry.get("sha256"), str)) for entry in target["paths"].values()),
    }


def local_ref(root, files, rid, branch):
    if not isinstance(rid, str) or not SAFE_ID.fullmatch(rid) or rid in (".", ".."):
        fail("unsupported repository identifier")
    branch_parts = safe_components(branch, "Git branch ref")
    base = "repos/" + rid + "/git-repo/.git"
    if base in files:
        return None, "linked_worktree_unsupported"
    ref = base + "/refs/heads/" + "/".join(branch_parts)
    data = optional_sealed_read(root, ref, files)
    if data is not None:
        value = data.decode("ascii").strip()
        return (value if oid(value) else None), "loose"
    packed = optional_sealed_read(root, base + "/packed-refs", files)
    if packed is not None:
        matches = []
        wanted = "refs/heads/" + "/".join(branch_parts)
        for line in packed.decode("ascii").splitlines():
            if line.startswith(("#", "^")) or not line.strip():
                continue
            fields = line.split(" ", 1)
            if len(fields) == 2 and fields[1] == wanted:
                matches.append(fields[0])
        if len(matches) > 1:
            fail("ambiguous packed Git ref")
        if matches:
            return (matches[0] if oid(matches[0]) else None), "packed"
    return None, "missing"


def credential_owner(present_keys, rows_by_id, rid, prefix, env_reference):
    current = rid
    seen = set()
    while current is not None:
        if current in seen or current not in rows_by_id:
            return {"source": "UNKNOWN_PARENT_CHAIN", "repository_id": None}
        seen.add(current)
        if prefix + "_" + current in present_keys:
            return {"source": "repository" if current == rid else "ancestor_repository",
                    "repository_id": current}
        current = rows_by_id[current]["parent_id"]
    if prefix in present_keys:
        return {"source": "global_kv", "repository_id": None}
    if env_reference:
        return {"source": "config_env_reference", "repository_id": None}
    return {"source": "not_present", "repository_id": None}


def schema_shape(db, tables):
    relevant = ("repositories", "kv_state", "watermarks", "commit_map", "sync_records", "import_progress", "encrypted_secrets")
    result = {}
    for table in relevant:
        if table not in tables:
            result[table] = {"present": False}
            continue
        columns = [dict(row) for row in db.execute("PRAGMA table_info('" + table + "')")]
        indexes = [{"name": row["name"], "unique": bool(row["unique"]),
                    "columns": [entry["name"] for entry in db.execute(
                        "PRAGMA index_info('" + row["name"].replace("'", "''") + "')")]}
                   for row in db.execute("PRAGMA index_list('" + table + "')")]
        result[table] = {"present": True,
                         "columns": [{"name": row["name"], "type": row["type"],
                                      "not_null": bool(row["notnull"]), "pk": row["pk"]} for row in columns],
                         "indexes": indexes,
                         "foreign_keys": [dict(row) for row in db.execute("PRAGMA foreign_key_list('" + table + "')")]}
    return result


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
        required = {"repositories", "kv_state", "watermarks", "sync_records", "commit_map", "import_progress"}
        tables = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        if not required <= tables:
            fail("incomplete fixture database")
        if db.execute("PRAGMA foreign_key_check").fetchone() is not None:
            fail("fixture foreign-key check failed")
        config = tomllib.loads(sealed_read(root, "config.toml", expected["files"]).decode("utf-8"))
        keys = {row["key"]: row["value"] for row in db.execute("SELECT key, value FROM kv_state")}
        present_secrets = {key for key, value in keys.items() if value and key.startswith("secret_")}
        if "encrypted_secrets" in tables:
            present_secrets.update(row[0] for row in db.execute("SELECT key FROM encrypted_secrets"))
        import_watermarks = {row["source"]: row["value"] for row in db.execute(
            "SELECT source, value FROM watermarks WHERE source IN ('svn_rev','git_sha','svn','git') ORDER BY source")}
        unrecognized_watermark_count = db.execute(
            "SELECT COUNT(*) FROM watermarks WHERE source NOT IN ('svn_rev','git_sha','svn','git')").fetchone()[0]
        progress = [dict(row) for row in db.execute(
            "SELECT id, repo_id, phase, current_rev, total_revs, commits_created, batches_pushed, files_skipped FROM import_progress ORDER BY id")]
        global_mapping_svn_max = db.execute("SELECT MAX(svn_rev) FROM commit_map").fetchone()[0]
        global_mapping_git_latest = db.execute("SELECT git_sha FROM commit_map ORDER BY id DESC LIMIT 1").fetchone()
        global_mapping_git_latest = global_mapping_git_latest[0] if global_mapping_git_latest else None
        shape = schema_shape(db, tables)
        repos = []
        rows = db.execute("SELECT id, name, enabled, parent_id, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch, last_svn_rev, last_git_sha, allowed_paths, blocked_patterns, sync_status FROM repositories ORDER BY id").fetchall()
        rows_by_id = {row["id"]: row for row in rows}
        for row in rows:
            rid = row["id"]
            column = row["last_git_sha"] or None
            scoped = keys.get(VOCABULARY["scoped_git_prefix"] + rid) or None
            policy = json.dumps({"allowed_paths": json.loads(row["allowed_paths"] or "[]"),
                                 "blocked_patterns": json.loads(row["blocked_patterns"] or "[]")},
                                separators=(",", ":"), sort_keys=True)
            mappings = [dict(x) for x in db.execute(
                "SELECT direction, status, svn_rev, git_sha FROM sync_records WHERE repo_id = ? ORDER BY svn_rev, git_sha, direction", (rid,))]
            old_maps = [dict(x) for x in db.execute(
                "SELECT direction, svn_rev, git_sha FROM commit_map WHERE repo_id = ? ORDER BY svn_rev, git_sha", (rid,))]
            receipts = [receipt_view(raw, rid, key[len(VOCABULARY["no_target_prefix"] + rid + "_"):], policy)
                        for key, raw in sorted(keys.items()) if receipt_owner(key, rows_by_id) == rid]
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
            ref, ref_storage = local_ref(root, expected["files"], rid, row["git_branch"])
            scoped_svn = keys.get(VOCABULARY["scoped_svn_prefix"] + rid)
            global_svn = keys.get("last_svn_rev")
            global_git = keys.get("last_git_hash")
            source_disagreements = []
            if scoped_svn is not None and str(row["last_svn_rev"]) != scoped_svn:
                source_disagreements.append({"sources": ["repositories.last_svn_rev", "kv_state.last_svn_rev_<repo>"],
                                             "disposition": "reconcile_scoped_incoming_cursor"})
            if global_svn is not None and str(row["last_svn_rev"]) != global_svn:
                source_disagreements.append({"sources": ["repositories.last_svn_rev", "kv_state.last_svn_rev"],
                                             "disposition": "global_reference_not_repository_authority"})
            if column and scoped and column != scoped:
                source_disagreements.append({"sources": ["repositories.last_git_sha", "kv_state.last_git_sha_<repo>"],
                                             "disposition": "verify_handled_vs_emitted_frontiers"})
            for item in progress:
                if item["repo_id"] == rid and item["current_rev"] != row["last_svn_rev"]:
                    source_disagreements.append({"sources": ["repositories.last_svn_rev", "import_progress.current_rev"],
                                                 "disposition": "reconcile_owned_import_progress"})
            if import_watermarks.get("svn_rev") is not None and str(row["last_svn_rev"]) != import_watermarks["svn_rev"]:
                source_disagreements.append({"sources": ["repositories.last_svn_rev", "watermarks.svn_rev"],
                                             "disposition": "import_watermark_owner_unproved"})
            missing = []
            if not oid(column): missing.append("repository_column_missing_or_malformed")
            if scoped != column: missing.append("cursor_copies_differ_or_missing")
            if scoped_svn is not None and str(row["last_svn_rev"]) != scoped_svn:
                missing.append("scoped_incoming_svn_cursor_disagrees")
            if any(item["repo_id"] == rid and item["current_rev"] != row["last_svn_rev"] for item in progress):
                missing.append("owned_import_progress_disagrees")
            if import_watermarks.get("svn_rev") is not None and str(row["last_svn_rev"]) != import_watermarks["svn_rev"]:
                missing.append("import_watermark_owner_or_revision_unproved")
            if not applied_import: missing.append("scoped_import_mapping_missing")
            if column and column == scoped and not applied_import and not applied_outbound \
                    and not any(r.get("sha") == column and r.get("owner_matches") for r in receipts):
                missing.append("no_target_receipt_or_applied_mapping_missing")
            if ref != column: missing.append("local_git_ref_unproved")
            if ref_storage == "linked_worktree_unsupported": missing.append("linked_worktree_ref_storage_unqualified")
            if any(r.get("status") == "malformed" or not r.get("owner_matches")
                   or not r.get("policy_matches") or (r.get("outcome") == "no_svn_delta"
                   and not r.get("semantic_proof_present")) for r in receipts):
                missing.append("receipt_requires_reconciliation")
            svn_owner = credential_owner(present_secrets, rows_by_id, rid, "secret_svn_password",
                config.get("svn", {}).get("password_env"))
            git_owner = credential_owner(present_secrets, rows_by_id, rid, "secret_git_token",
                config.get("github", {}).get("token_env"))
            if svn_owner["source"] == "UNKNOWN_PARENT_CHAIN" or git_owner["source"] == "UNKNOWN_PARENT_CHAIN":
                missing.append("credential_parent_chain_unqualified")
            if keys.get(VOCABULARY["unknown_effect_prefix"] + rid):
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
                           "local_ref": ref, "local_ref_storage": ref_storage},
                "policy": {"allowed_paths": json.loads(row["allowed_paths"] or "[]"),
                           "blocked_patterns": json.loads(row["blocked_patterns"] or "[]")},
                "checkpoints": {"repository_svn_revision": row["last_svn_rev"],
                                "repository_git_column": column, "scoped_git_kv": scoped,
                                "legacy_global_git_kv_reference": global_git,
                                "scoped_svn_kv": scoped_svn,
                                "legacy_global_svn_kv_reference": global_svn,
                                "legacy_global_commit_map_svn_max_reference": global_mapping_svn_max,
                                "legacy_global_commit_map_latest_git_reference": global_mapping_git_latest,
                                "import_watermarks": import_watermarks,
                                "source_disagreements": source_disagreements,
                                "incoming_reader_authority": "repository_column_when_positive_else_scoped_kv_else_mapping_fallback",
                                "outgoing_reader_authority": "repository_scoped_receipt_and_mapping_checks",
                                "git_log_marker_fallback": "UNKNOWN_NOT_READ_BY_LOCAL_INVENTORY"},
                "applied_mappings": mappings, "legacy_commit_map": old_maps,
                "baseline_receipt": baseline, "no_target_receipts": receipts,
                "credentials": {"svn_username_present": bool(row["svn_username"]),
                                "repo_svn_secret_present": "secret_svn_password_" + rid in present_secrets,
                                "repo_git_secret_present": "secret_git_token_" + rid in present_secrets,
                                "global_svn_password_env": config.get("svn", {}).get("password_env"),
                                "global_git_token_env": config.get("github", {}).get("token_env"),
                                "svn_owner": svn_owner, "git_owner": git_owner,
                                "inheritance": "resolved_local_presence_only"},
                "missing_proof": sorted(set(missing + ["remote_svn_uuid_and_copy_ancestry_unknown",
                                                        "remote_git_ref_identity_unknown"])),
                "classification": classification,
            })
        result = {"version": 1, "input_kind": "sealed_quiesced_fixture_copy",
                  "pinned_source": expected["pinned_source"], "schema": schema,
                  "input_unchanged": True, "file_count": len(expected["files"]),
                  "production_eligibility": "NOT_ESTABLISHED", "repositories": repos,
                  "schema_shape": shape,
                  "foreign_key_check": "PASS",
                  "import_progress": progress,
                  "import_watermark_other_source_count": unrecognized_watermark_count}
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
    except (ValueError, sqlite3.Error, OSError, UnicodeError, tomllib.TOMLDecodeError):
        print("inventory refused: invalid, unsafe, or unsupported sealed fixture input", file=sys.stderr)
        sys.exit(2)
