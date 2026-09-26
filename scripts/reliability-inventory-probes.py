#!/usr/bin/env python3
"""Fixture-only inventory authority and sealed-read probes."""
import argparse
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys


def run_inventory(script, copy, seal_path, audited_canary=None):
    audit_code = r'''
import os, runpy, sys
script, mode, root, manifest, canary = sys.argv[1:]
opened = []
def audit(event, args):
    if event == "open" and args and isinstance(args[0], (str, bytes, os.PathLike)) and os.fspath(args[0]) == canary:
        opened.append(event)
sys.addaudithook(audit)
sys.argv = [script, "--seal-copy", root] if mode == "seal" else [script, "--copy", root, "--manifest", manifest]
try:
    runpy.run_path(script, run_name="__main__")
except SystemExit as exc:
    code = exc.code if isinstance(exc.code, int) else 1
else:
    code = 0
print("AUDITED_CANARY_OPENS=" + str(len(opened)), file=sys.stderr)
sys.exit(code)
'''
    def invoke(mode):
        if audited_canary is None:
            command = ([sys.executable, script, "--seal-copy", str(copy)] if mode == "seal" else
                       [sys.executable, script, "--copy", str(copy), "--manifest", str(seal_path)])
            return subprocess.run(command, capture_output=True, text=True), None
        result = subprocess.run([sys.executable, "-c", audit_code, script, mode, str(copy),
                                 str(seal_path), str(audited_canary)], capture_output=True, text=True)
        marker = [line for line in result.stderr.splitlines()
                  if line.startswith("AUDITED_CANARY_OPENS=")]
        assert len(marker) == 1, "audit hook did not report: " + result.stderr
        return result, int(marker[0].split("=", 1)[1])
    seal, seal_opened = invoke("seal")
    if seal.returncode:
        return {"seal_refused": True, "outside_canary_opens": seal_opened,
                "diagnostic": seal.stderr.splitlines()[0]}
    seal_path.write_text(seal.stdout)
    result, opened = invoke("inspect")
    reseal = subprocess.run([sys.executable, script, "--seal-copy", str(copy)],
                            capture_output=True, text=True)
    assert reseal.returncode == 0 and reseal.stdout == seal.stdout, "inventory mutated sealed input"
    if result.returncode:
        assert str(audited_canary or "") not in result.stderr, "refusal leaked path"
        return {"refused": True, "outside_canary_opens": opened,
                "diagnostic": result.stderr.splitlines()[0]}
    return {"report": json.loads(result.stdout), "outside_canary_opens": opened,
            "input_unchanged": True}


def mutation(db_path, statements):
    db = sqlite3.connect(db_path)
    try:
        for sql, params in statements:
            db.execute(sql, params)
        db.commit()
    finally:
        db.close()


def row(report, rid="pair"):
    return next(item for item in report["repositories"] if item["id"] == rid)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--inventory-script", required=True)
    parser.add_argument("--old-install", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    args = parser.parse_args()
    args.work.mkdir(parents=True)
    script = str(Path(args.inventory_script).resolve())
    old = args.old_install.resolve()
    work = args.work.resolve()
    canary = work / "outside-canary"
    canary.write_text("a" * 40 + "\n")
    reports = {}
    def overlay(name, statements=(), files=None, audited=False):
        copy = work / name
        shutil.copytree(old, copy)
        if statements:
            mutation(copy / "reposync.db", statements)
        if files:
            files(copy)
        evidence = run_inventory(script, copy, work / (name + ".private-seal.json"),
                                 canary if audited else None)
        reports[name] = evidence
        return evidence

    base = overlay("unchanged_old")
    assert row(base["report"])["classification"] == "qualified_fixture_shape"
    assert base["report"]["schema_shape"]["watermarks"]["present"]
    assert base["report"]["foreign_key_check"] == "PASS"
    assert base["report"]["import_progress"][0]["current_rev"] == 2
    assert row(base["report"])["target"]["local_ref_storage"] == "loose"

    scoped = overlay("synthetic_scoped_svn_999", [
        ("UPDATE kv_state SET value = '999' WHERE key = 'last_svn_rev_pair'", ())])
    assert "report" in scoped, scoped
    assert row(scoped["report"])["checkpoints"]["scoped_svn_kv"] == "999"
    assert row(scoped["report"])["classification"] == "needs_reconciliation"

    global_svn = overlay("synthetic_global_svn_888", [
        ("INSERT INTO kv_state(key,value,updated_at) VALUES('last_svn_rev','888','')", ())])
    assert row(global_svn["report"])["checkpoints"]["legacy_global_svn_kv_reference"] == "888"
    assert any(item["disposition"] == "global_reference_not_repository_authority"
               for item in row(global_svn["report"])["checkpoints"]["source_disagreements"])

    watermark = overlay("synthetic_import_watermark_777", [
        ("UPDATE watermarks SET value = '777' WHERE source = 'svn_rev'", ())])
    assert row(watermark["report"])["checkpoints"]["import_watermarks"]["svn_rev"] == "777"
    assert row(watermark["report"])["classification"] == "needs_reconciliation"

    progress = overlay("synthetic_owned_progress_666", [
        ("UPDATE import_progress SET repo_id = 'pair', current_rev = 666", ())])
    assert progress["report"]["import_progress"][0]["current_rev"] == 666
    assert row(progress["report"])["classification"] == "needs_reconciliation"

    combined = overlay("synthetic_four_disagreements", [
        ("UPDATE kv_state SET value = '999' WHERE key = 'last_svn_rev_pair'", ()),
        ("INSERT INTO kv_state(key,value,updated_at) VALUES('last_svn_rev','888','')", ()),
        ("UPDATE watermarks SET value = '777' WHERE source = 'svn_rev'", ()),
        ("UPDATE import_progress SET repo_id = 'pair', current_rev = 666", ())])
    cp = row(combined["report"])["checkpoints"]
    assert [cp["repository_svn_revision"], cp["scoped_svn_kv"],
            cp["legacy_global_svn_kv_reference"], cp["import_watermarks"]["svn_rev"]] == [2, "999", "888", "777"]
    assert len(cp["source_disagreements"]) >= 4
    assert row(combined["report"])["classification"] == "needs_reconciliation"

    inherited = overlay("synthetic_parent_credential_inheritance", [
        ("UPDATE repositories SET parent_id = 'pair' WHERE id = 'pair_disabled'", ()),
        ("DELETE FROM kv_state WHERE key IN ('secret_svn_password_pair_disabled','secret_git_token_pair_disabled')", ())])
    child = row(inherited["report"], "pair_disabled")
    assert child["classification"] == "not_qualified"
    assert child["credentials"]["svn_owner"] == {"source": "ancestor_repository", "repository_id": "pair"}
    assert child["credentials"]["git_owner"] == {"source": "ancestor_repository", "repository_id": "pair"}

    unsupported_parent = overlay("synthetic_missing_credential_parent", [
        ("UPDATE repositories SET parent_id = 'absent' WHERE id = 'pair_disabled'", ()),
        ("DELETE FROM kv_state WHERE key IN ('secret_svn_password_pair_disabled','secret_git_token_pair_disabled')", ())])
    assert row(unsupported_parent["report"], "pair_disabled")["credentials"]["svn_owner"]["source"] == "UNKNOWN_PARENT_CHAIN"

    def packed_ref(copy):
        ref = copy / "repos" / "pair" / "git-repo" / ".git" / "refs" / "heads" / "main"
        sha = ref.read_text().strip()
        ref.unlink()
        packed = copy / "repos" / "pair" / "git-repo" / ".git" / "packed-refs"
        with packed.open("a") as stream:
            stream.write(sha + " refs/heads/main\n")
    packed = overlay("synthetic_packed_ref", files=packed_ref, audited=True)
    assert row(packed["report"])["target"]["local_ref_storage"] == "packed"
    assert row(packed["report"])["classification"] == "qualified_fixture_shape"
    assert packed["outside_canary_opens"] == 0

    for name, sql in [
        ("absolute_branch", "UPDATE repositories SET git_branch = ? WHERE id = 'pair'"),
        ("traversal_branch", "UPDATE repositories SET git_branch = ? WHERE id = 'pair'"),
        ("invalid_id", "UPDATE repositories SET id = ? WHERE id = 'pair'"),
    ]:
        value = str(canary) if name == "absolute_branch" else (
            "../../outside-canary" if name == "traversal_branch" else "../outside-canary")
        unsafe = overlay("synthetic_" + name, [(sql, (value,))], audited=True)
        assert unsafe["refused"] and unsafe["outside_canary_opens"] == 0
        assert value not in unsafe["diagnostic"]

    def symlink_ancestor(copy):
        refs = copy / "repos" / "pair" / "git-repo" / ".git" / "refs"
        shutil.rmtree(refs)
        refs.symlink_to(work, target_is_directory=True)
    link = overlay("synthetic_symlink_ancestor", files=symlink_ancestor, audited=True)
    assert link["seal_refused"] and link["outside_canary_opens"] == 0
    assert str(canary) not in link["diagnostic"]

    def linked_gitdir(copy):
        gitdir = copy / "repos" / "pair" / "git-repo" / ".git"
        shutil.rmtree(gitdir)
        gitdir.write_text("gitdir: " + str(canary) + "\n")
    linked = overlay("synthetic_linked_worktree", files=linked_gitdir, audited=True)
    assert row(linked["report"])["classification"] == "needs_reconciliation"
    assert row(linked["report"])["target"]["local_ref_storage"] == "linked_worktree_unsupported"
    assert linked["outside_canary_opens"] == 0

    for value in reports.values():
        serialized = json.dumps(value)
        assert "synthetic-svn-pair" not in serialized and "synthetic-git-pair" not in serialized
    print(json.dumps({"case": "R10_INVENTORY_AUTHORITY_CONFINEMENT", "reports": reports,
                      "input_kind": "pinned_old_production_import_plus_explicit_synthetic_overlays",
                      "outside_canary_open_count": 0}, sort_keys=True))


if __name__ == "__main__":
    main()
