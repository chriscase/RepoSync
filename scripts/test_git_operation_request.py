#!/usr/bin/env python3
"""Deterministic tests for the manual RepoSync operation client.

These tests use a loopback HTTP stub. They do not call GitHub, SVN, or a
live RepoSync service.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import git_operation_request as client

ROOT = Path(__file__).resolve().parents[1]
GOAL = "16003181005349892c486d92ac980951c7cb564ab6d45742588df581955eaec8"
DIGEST = "ab" * 32
OTHER_DIGEST = "cd" * 32
PAIR_TIP = "a" * 40
PARENT_TIP = "b" * 40
TOKEN = "test-token-value"


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        self._handle()

    def do_POST(self):
        self._handle()

    def _handle(self):
        length = int(self.headers.get("Content-Length", "0") or "0")
        raw = self.rfile.read(length) if length else b""
        record = {
            "method": self.command,
            "path": self.path,
            "authorization": self.headers.get("Authorization"),
            "idempotency": self.headers.get("Idempotency-Key"),
            "body": raw.decode("utf-8") if raw else "",
        }
        self.server.records.append(record)
        status, payload = self.server.respond(record)
        data = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, fmt, *args):
        return


class RedirectHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0") or "0")
        self.rfile.read(length)
        self.server.records.append({"authorization": self.headers.get("Authorization")})
        self.send_response(302)
        self.send_header("Location", self.server.location)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, fmt, *args):
        return


class Stub(ThreadingHTTPServer):
    def __init__(self, respond):
        super().__init__(("127.0.0.1", 0), Handler)
        self.records = []
        self.respond = respond

    @property
    def base_url(self):
        host, port = self.server_address[:2]
        return f"http://{host}:{port}"


def serve(stub):
    thread = threading.Thread(target=stub.serve_forever, daemon=True)
    thread.start()
    return stub


def plan(**overrides):
    body = {
        "mode": "preview",
        "operation": "update_pair_from_parent",
        "executed": False,
        "published": False,
        "durable_job_started": False,
        "policy_version": "pair_refresh_preview_v1",
        "pair_id": "pair1",
        "parent_id": "repo1",
        "plan_digest": DIGEST,
        "plan_id": DIGEST,
        "approval": {
            "eligible": False,
            "reason": "execution_not_implemented",
            "binds_to": "plan_digest",
        },
        "git": {
            "pair_branch": "feature/widget",
            "parent_branch": "main",
            "pair_tip": PAIR_TIP,
            "parent_tip": PARENT_TIP,
        },
        "svn": {
            "pair_url": "https://user:secret@svn.example/repo/branches/widget",
            "parent_url": "https://svn.example/repo/trunk",
            "pair_revision": 10,
            "parent_revision": 12,
        },
        "execute_status": "NOT_IMPLEMENTED",
        "reanchor_status": "NOT_IMPLEMENTED",
    }
    body.update(overrides)
    return body


class ClientTests(unittest.TestCase):
    def setUp(self):
        self._servers = []
        self._tmp = tempfile.TemporaryDirectory()
        self.journal = Path(self._tmp.name) / "journal.json"

    def tearDown(self):
        for server in self._servers:
            server.shutdown()
            server.server_close()
        self._tmp.cleanup()

    def start(self, respond):
        stub = serve(Stub(respond))
        self._servers.append(stub)
        return stub

    def run_client(self, server, action, **overrides):
        config = {
            "action": action,
            "base_url": server.base_url,
            "token": TOKEN,
            "repo_id": "repo1",
            "pair_id": "pair1",
            "idempotency_key": "idem-key-001",
            "expected_plan_digest": DIGEST,
            "expected_pair_tip": PAIR_TIP,
            "expected_parent_tip": PARENT_TIP,
            "git_branch": "feature/widget",
            "operation": "update_pair_from_parent",
            "operation_kind": "refresh",
            "operation_id": None,
            "journal_path": str(self.journal),
            "timeout": 5,
            "poll_interval": 0,
            "poll_attempts": 5,
            "allow_insecure_http": True,
        }
        config.update(overrides)
        return client.execute(config)

    def test_goal_md_unchanged(self):
        digest = hashlib.sha256((ROOT / "docs/reliability/GOAL.md").read_bytes()).hexdigest()
        self.assertEqual(digest, GOAL)

    def test_authorized_preview_pins_and_redacts(self):
        server = self.start(lambda _record: (200, plan()))
        result = self.run_client(server, "preview")
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["outcome"], "preview_ok")
        self.assertEqual(result["plan_digest"], DIGEST)
        self.assertTrue(result["authenticated"])
        self.assertFalse(result["execute_requested"])
        self.assertEqual(server.records[0]["authorization"], f"Bearer {TOKEN}")
        self.assertEqual(server.records[0]["idempotency"], "idem-key-001")
        body = json.loads(server.records[0]["body"])
        self.assertFalse(body["execute"])
        self.assertEqual(body["operation"], "update_pair_from_parent")
        rendered = client.redact(json.dumps(result), [TOKEN])
        self.assertNotIn(TOKEN, rendered)
        self.assertNotIn("user:secret", rendered)
        self.assertNotIn("svn.example", rendered)
        self.assertNotIn("pair_url", json.dumps(result))

    def test_execute_not_implemented_does_not_post_execute(self):
        server = self.start(lambda _record: (200, plan()))
        result = self.run_client(server, "execute")
        self.assertFalse(result["ok"])
        self.assertEqual(result["outcome"], "refresh_execute_not_implemented")
        self.assertEqual(result["exit_code"], client.EXIT_NOT_IMPLEMENTED)
        self.assertFalse(result["execute_requested"])
        self.assertFalse(result["durable_job_started"])
        self.assertEqual(len(server.records), 1)
        self.assertFalse(json.loads(server.records[0]["body"])["execute"])

    def test_server_execute_refusal_is_reported(self):
        calls = {"n": 0}

        def respond(record):
            body = json.loads(record["body"] or "{}")
            calls["n"] += 1
            if body.get("execute") is True:
                return 400, {
                    "error": (
                        "refresh_execute_not_implemented: update-pair-from-parent "
                        f"execution is a later slice. plan_digest={DIGEST}"
                    )
                }
            approved = plan()
            approved["approval"] = {
                "eligible": True,
                "reason": "approved",
                "binds_to": "plan_digest",
            }
            approved["execute_status"] = "READY"
            return 200, approved

        server = self.start(respond)
        result = self.run_client(server, "execute")
        self.assertEqual(result["outcome"], "refresh_execute_not_implemented")
        self.assertTrue(result["execute_requested"])
        self.assertFalse(result["durable_job_started"])
        self.assertEqual(calls["n"], 2)

    def test_approved_execute_polls_status(self):
        polls = {"n": 0}

        def respond(record):
            if record["method"] == "GET":
                polls["n"] += 1
                lifecycle = "running" if polls["n"] == 1 else "completed"
                return 200, {
                    "operation_id": "op123",
                    "lifecycle": lifecycle,
                    "repo_id": "repo1",
                }
            body = json.loads(record["body"] or "{}")
            if body.get("execute") is True:
                return 200, {
                    "operation_id": "op123",
                    "lifecycle": "queued",
                    "durable_job_started": True,
                    "status_path": "/api/repos/repo1/import/status",
                }
            approved = plan()
            approved["approval"] = {
                "eligible": True,
                "reason": "approved",
                "binds_to": "plan_digest",
            }
            approved["execute_status"] = "READY"
            return 200, approved

        server = self.start(respond)
        result = self.run_client(server, "execute")
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["outcome"], "completed")
        self.assertEqual(result["lifecycle"], "completed")
        self.assertTrue(result["execute_requested"])
        self.assertEqual(polls["n"], 2)
        self.assertTrue(all(item["authorization"] == f"Bearer {TOKEN}" for item in server.records))

    def test_status_path_requires_exact_id_segment(self):
        self.assertIsNone(
            client.safe_status_path("/api/repos/repo1-evil/import/status", "repo1", "pair1")
        )
        self.assertEqual(
            client.safe_status_path("/api/repos/repo1/import/status", "repo1", "pair1"),
            "/api/repos/repo1/import/status",
        )

    def test_status_path_rejects_off_host_and_other_repo(self):
        def respond(record):
            body = json.loads(record["body"] or "{}")
            if body.get("execute") is True:
                return 200, {
                    "operation_id": "op123",
                    "lifecycle": "queued",
                    "durable_job_started": True,
                    "status_path": "http://127.0.0.1:9/api/repos/repo1/import/status",
                }
            approved = plan()
            approved["approval"] = {"eligible": True, "reason": "approved", "binds_to": "plan_digest"}
            approved["execute_status"] = "READY"
            return 200, approved

        server = self.start(respond)
        result = self.run_client(server, "execute")
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["lifecycle"], "queued")
        self.assertTrue(all(item["method"] == "POST" for item in server.records))

    def test_authorized_import_status_and_cancel(self):
        def respond(record):
            if record["authorization"] != f"Bearer {TOKEN}":
                return 401, {"error": "missing or invalid Authorization header"}
            if record["method"] == "GET" and record["path"] == "/api/repos/repo1/import/status":
                return 200, {
                    "operation_id": "op123",
                    "lifecycle": "running",
                    "repo_id": "repo1",
                }
            if record["method"] == "POST" and record["path"] == "/api/repos/repo1/import/op123/cancel":
                return 200, {
                    "ok": True,
                    "operation_id": "op123",
                    "lifecycle": "cancel_requested",
                    "repo_id": "repo1",
                }
            return 404, {"error": "no such route"}

        server = self.start(respond)
        status = self.run_client(
            server,
            "status",
            operation_kind="import",
            operation_id="op123",
            git_branch=None,
            expected_plan_digest=None,
            expected_pair_tip=None,
            expected_parent_tip=None,
        )
        self.assertTrue(status["ok"], status)
        self.assertEqual(status["lifecycle"], "running")
        self.assertTrue(status["authenticated"])
        cancelled = self.run_client(
            server,
            "cancel",
            operation_kind="import",
            operation_id="op123",
            git_branch=None,
        )
        self.assertTrue(cancelled["ok"], cancelled)
        self.assertEqual(cancelled["outcome"], "cancel_requested")
        self.assertTrue(all(item["authorization"] == f"Bearer {TOKEN}" for item in server.records))

    def test_reconciliation_required_is_visible(self):
        server = self.start(
            lambda record: (
                200,
                {
                    "operation_id": "op123",
                    "lifecycle": "reconciliation_required",
                    "repo_id": "repo1",
                    "outcome_detail": "push result unverified",
                },
            )
            if record["method"] == "GET"
            else (404, {"error": "no"})
        )
        result = self.run_client(server, "status", operation_kind="import", operation_id="op123")
        self.assertTrue(result["ok"], result)
        self.assertTrue(result["needs_reconciliation"])
        self.assertIn("Do not force-push", result["operator_action"])
        self.assertIn("svn merge", result["operator_action"])

    def test_refresh_cancel_authenticates_then_refuses(self):
        def respond(record):
            if record["path"] == "/api/auth/me" and record["method"] == "GET":
                if record["authorization"] != f"Bearer {TOKEN}":
                    return 401, {"error": "missing or invalid Authorization header"}
                return 200, {"id": "user", "role": "admin"}
            return 500, {"error": "cancel must not call another route"}

        server = self.start(respond)
        result = self.run_client(server, "cancel")
        self.assertEqual(result["outcome"], "cancel_not_applicable")
        self.assertTrue(result["authenticated"])
        self.assertFalse(result["durable_job_started"])
        self.assertIn("refresh_execute_not_implemented", result["message"])
        self.assertEqual([item["path"] for item in server.records], ["/api/auth/me"])

    def test_wrong_token(self):
        def respond(record):
            if record["authorization"] != f"Bearer {TOKEN}":
                return 401, {"error": f"bad token {TOKEN}"}
            return 200, plan()

        server = self.start(respond)
        result = self.run_client(server, "preview", token="wrong-token-value")
        self.assertEqual(result["outcome"], "unauthorized")
        rendered = client.redact(json.dumps(result), ["wrong-token-value", TOKEN])
        self.assertNotIn("wrong-token-value", rendered)
        self.assertNotIn(TOKEN, rendered)

    def test_wrong_repo_and_pair(self):
        server = self.start(lambda _record: (200, plan(parent_id="other-repo")))
        wrong_repo = self.run_client(server, "preview")
        self.assertEqual(wrong_repo["outcome"], "wrong_repo")

        server2 = self.start(lambda _record: (404, {"error": "repository pair1 not found"}))
        wrong_pair = self.run_client(server2, "preview")
        self.assertEqual(wrong_pair["outcome"], "wrong_pair")

        server3 = self.start(lambda _record: (404, {"error": "repository not found"}))
        wrong_import = self.run_client(
            server3, "status", operation_kind="import", operation_id="op123"
        )
        self.assertEqual(wrong_import["outcome"], "wrong_repo")

    def test_stale_approval_does_not_execute(self):
        server = self.start(lambda _record: (200, plan(plan_digest=OTHER_DIGEST, plan_id=OTHER_DIGEST)))
        result = self.run_client(server, "execute")
        self.assertEqual(result["outcome"], "stale_approval")
        self.assertFalse(result["execute_requested"])
        self.assertEqual(len(server.records), 1)
        self.assertFalse(json.loads(server.records[0]["body"])["execute"])

    def test_repeated_request_same_key(self):
        server = self.start(lambda _record: (200, plan()))
        first = self.run_client(server, "preview")
        second = self.run_client(server, "preview")
        self.assertTrue(first["ok"], first)
        self.assertFalse(first["repeated"])
        self.assertTrue(second["ok"], second)
        self.assertTrue(second["repeated"])
        self.assertEqual(second["plan_digest"], DIGEST)
        self.assertEqual(len(server.records), 2)

    def test_repeated_key_rejects_moved_plan(self):
        calls = {"n": 0}

        def respond(_record):
            calls["n"] += 1
            digest = DIGEST if calls["n"] == 1 else OTHER_DIGEST
            return 200, plan(plan_digest=digest, plan_id=digest)

        server = self.start(respond)
        first = self.run_client(server, "preview", expected_plan_digest=None)
        second = self.run_client(server, "preview", expected_plan_digest=None)
        self.assertTrue(first["ok"], first)
        self.assertEqual(second["outcome"], "stale_approval")
        third = self.run_client(server, "execute", expected_plan_digest=OTHER_DIGEST)
        self.assertEqual(third["outcome"], "stale_approval")
        self.assertFalse(third["execute_requested"])

    def test_idempotency_conflict_different_pair(self):
        server = self.start(lambda _record: (200, plan()))
        self.run_client(server, "preview")
        result = self.run_client(server, "preview", pair_id="pair2")
        self.assertEqual(result["outcome"], "idempotency_conflict")
        self.assertEqual(len(server.records), 1)

    def test_untrusted_branch_makes_no_request(self):
        server = self.start(lambda _record: (200, plan()))
        for branch in ("feature; touch /tmp/pwned", "feature\nmain", "-main", "refs/../heads/main", "$(id)"):
            result = self.run_client(server, "preview", git_branch=branch)
            self.assertEqual(result["outcome"], "untrusted_branch", branch)
        self.assertEqual(server.records, [])

    def test_client_source_does_not_shell_out(self):
        source = (ROOT / "scripts/git_operation_request.py").read_text(encoding="utf-8")
        self.assertNotIn("subprocess", source)
        self.assertNotIn("os.system", source)
        self.assertNotIn("shell=True", source)
        self.assertNotIn("sqlite3", source)
        self.assertNotIn("Popen", source)

    def test_service_unavailable(self):
        result = client.execute(
            {
                "action": "preview",
                "base_url": "http://127.0.0.1:9",
                "token": TOKEN,
                "repo_id": "repo1",
                "pair_id": "pair1",
                "idempotency_key": "idem-key-001",
                "expected_plan_digest": DIGEST,
                "expected_pair_tip": None,
                "expected_parent_tip": None,
                "git_branch": None,
                "operation": "update_pair_from_parent",
                "operation_kind": "refresh",
                "operation_id": None,
                "journal_path": str(self.journal),
                "timeout": 1,
                "poll_interval": 0,
                "poll_attempts": 1,
                "allow_insecure_http": True,
            }
        )
        self.assertEqual(result["outcome"], "service_unavailable")
        self.assertEqual(result["exit_code"], client.EXIT_UNAVAILABLE)

    def test_redirect_is_not_followed(self):
        sink = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        sink.records = []
        sink.respond = lambda _record: (200, {"stolen": TOKEN})
        thread = threading.Thread(target=sink.serve_forever, daemon=True)
        thread.start()
        self._servers.append(sink)
        location = f"http://127.0.0.1:{sink.server_address[1]}/api/repos/repo1/refresh"
        redir = ThreadingHTTPServer(("127.0.0.1", 0), RedirectHandler)
        redir.records = []
        redir.location = location
        redir.base_url = f"http://127.0.0.1:{redir.server_address[1]}"
        threading.Thread(target=redir.serve_forever, daemon=True).start()
        self._servers.append(redir)
        result = self.run_client(redir, "preview")
        self.assertEqual(result["outcome"], "redirect_refused")
        self.assertEqual(sink.records, [])

    def test_cli_redacts_token_and_urls(self):
        server = self.start(
            lambda _record: (
                401,
                {"error": f"token {TOKEN} at https://user:secret@svn.example/repo"},
            )
        )
        env = os.environ.copy()
        env["REPOSYNC_API_TOKEN"] = TOKEN
        completed = subprocess.run(
            [
                sys.executable,
                str(ROOT / "scripts/git_operation_request.py"),
                "preview",
                "--base-url",
                server.base_url,
                "--repo-id",
                "repo1",
                "--pair-id",
                "pair1",
                "--idempotency-key",
                "idem-key-001",
                "--journal",
                str(self.journal),
                "--allow-insecure-http",
            ],
            check=False,
            capture_output=True,
            text=True,
            env=env,
        )
        self.assertEqual(completed.returncode, client.EXIT_REJECTED)
        self.assertNotIn(TOKEN, completed.stdout)
        self.assertNotIn("user:secret", completed.stdout)
        self.assertNotIn("svn.example", completed.stdout)
        self.assertIn("[redacted-url]", completed.stdout)

    def test_insecure_http_rejected_off_loopback(self):
        result = client.execute(
            {
                "action": "preview",
                "base_url": "http://example.com",
                "token": TOKEN,
                "repo_id": "repo1",
                "pair_id": "pair1",
                "idempotency_key": "idem-key-001",
                "expected_plan_digest": None,
                "expected_pair_tip": None,
                "expected_parent_tip": None,
                "git_branch": None,
                "operation": "update_pair_from_parent",
                "operation_kind": "refresh",
                "operation_id": None,
                "journal_path": str(self.journal),
                "timeout": 1,
                "poll_interval": 0,
                "poll_attempts": 1,
                "allow_insecure_http": True,
            }
        )
        self.assertEqual(result["outcome"], "base_url_rejected")

    def test_workflow_is_manual_and_least_privilege(self):
        text = (ROOT / ".github/workflows/git-operation-request.yml").read_text(encoding="utf-8")
        code_lines = [
            line for line in text.splitlines() if not line.strip().startswith("#")
        ]
        body = "\n".join(code_lines)
        self.assertIn("workflow_dispatch:", body)
        self.assertNotIn("pull_request", body)
        self.assertNotIn("\n  push:", "\n" + body)
        self.assertIn("contents: read", body)
        self.assertNotIn("contents: write", body)
        self.assertNotIn("id-token:", body)
        self.assertIn("environment: reposync-operations", body)
        self.assertIn("persist-credentials: false", body)
        self.assertIn("refs/heads/main", body)
        run_section = body.split("run:", 1)[1]
        self.assertNotIn("${{ inputs.", run_section)
        self.assertNotIn("${{ secrets.", run_section)
        self.assertNotIn("github.token", body.lower())


if __name__ == "__main__":
    unittest.main()
