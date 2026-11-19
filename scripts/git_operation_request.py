#!/usr/bin/env python3
"""Manually request a RepoSync preview, status, or cancel through the HTTP API.

RepoSync remains the authority for locking, provenance, checkpoints, cancellation,
and recovery. This client does not run ``svn merge``, edit SQLite, delete remotes,
force-push, or interpolate branch names into a shell.

A v1 refresh preview is not execution approval. Named v2 refresh status reads
the durable receipt for the supplied child and request ID.
"""

from __future__ import annotations

import argparse
import http.client
import json
import os
import re
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

EXIT_OK = 0
EXIT_NOT_IMPLEMENTED = 2
EXIT_REJECTED = 3
EXIT_UNAVAILABLE = 4

NOT_IMPLEMENTED_EXIT = {
    "refresh_execute_not_implemented": EXIT_NOT_IMPLEMENTED,
    "reanchor_not_implemented": EXIT_NOT_IMPLEMENTED,
    "publish_not_implemented": EXIT_NOT_IMPLEMENTED,
    "cancel_not_applicable": EXIT_NOT_IMPLEMENTED,
}

ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
KEY_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{7,127}$")
SHA_RE = re.compile(r"^[0-9a-fA-F]{40}$|^[0-9a-fA-F]{64}$")
DIGEST_RE = re.compile(r"^[0-9a-fA-F]{64}$")
BRANCH_RE = re.compile(r"[A-Za-z0-9._/-]{1,200}\Z")
URL_RE = re.compile(r"https?://[^\s\"']+")
TERMINAL_LIFECYCLES = {
    "completed",
    "cancelled",
    "failed",
    "reconciliation_required",
}
WRITE_STATES = {"queued", "running", "completed", "failed", "reconciliation_required"}
IMPORT_STATES = WRITE_STATES | {"cancel_requested", "cancelling", "cancelled"}
IMPORT_PHASES = {
    "idle", "connecting", "importing", "verifying", "final_push", "completed", "failed", "cancelled",
}
REFRESH_PHASES = {
    "prepared", "svn_intent", "svn_observed", "git_intent", "git_observed",
    "completed", "cancelled_before_publication", "no_op",
}
TERMINAL_STATES = TERMINAL_LIFECYCLES | {"cancelled_before_publication", "no_op"}
REFRESH_UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
RECONCILIATION_ACTION = (
    "reconciliation_required: compare the recorded operation with the remote "
    "and leave the hold in place. Do not force-push, delete the ref, edit "
    "SQLite, or run an independent svn merge."
)


class RedirectRefusedError(Exception):
    """The API returned a redirect. The bearer token was not forwarded."""


class RejectRedirect(urllib.request.HTTPRedirectHandler):
    """Refuse redirects so the bearer token cannot follow to another host."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise RedirectRefusedError()


def nonempty(value: str | None) -> str | None:
    if value is None:
        return None
    text = str(value).strip()
    return text or None


def redact(text: str, secrets: list[str]) -> str:
    for secret in secrets:
        if secret and len(secret) >= 6:
            text = text.replace(secret, "[redacted]")
    text = URL_RE.sub("[redacted-url]", text)
    text = re.sub(r"(?i)bearer\s+\S+", "Bearer [redacted]", text)
    text = re.sub(
        r"\b(?:ghp_|github_pat_|glpat-|xox[baprs]-)[A-Za-z0-9_-]{8,}",
        "[redacted]",
        text,
    )
    return text


def validate_branch(name: str | None) -> str | None:
    """Accept a branch only as data. Never pass it to a shell or URL path."""
    if name is None:
        return None
    if (
        name != name.strip()
        or name.startswith("-")
        or name.startswith("/")
        or name.endswith("/")
        or name.endswith(".lock")
        or "@{" in name
        or ".." in name.split("/")
    ):
        raise ValueError("untrusted_branch")
    if not BRANCH_RE.fullmatch(name):
        raise ValueError("untrusted_branch")
    return name


def validate_base_url(url: str, allow_insecure_http: bool) -> str:
    parsed = urllib.parse.urlsplit(url)
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("base_url_rejected")
    host = parsed.hostname
    if parsed.scheme == "https" and host:
        return url.rstrip("/")
    if (
        allow_insecure_http
        and parsed.scheme == "http"
        and host in {"127.0.0.1", "localhost", "::1"}
    ):
        return url.rstrip("/")
    raise ValueError("base_url_rejected")


def check_id(value: str | None, label: str) -> str | None:
    if value is None:
        return None
    if not ID_RE.fullmatch(value):
        raise ValueError(f"invalid_{label}")
    return value


def identity_of(config: dict) -> dict:
    return {
        "repo_id": config.get("repo_id"),
        "pair_id": config.get("pair_id"),
        "operation": config.get("operation"),
        "operation_kind": config.get("operation_kind"),
        "expected_pair_tip": config.get("expected_pair_tip"),
        "expected_parent_tip": config.get("expected_parent_tip"),
        "git_branch": config.get("git_branch"),
    }


class Journal:
    def __init__(self, path: Path):
        self.path = path
        self.data = {"version": 1, "keys": {}}
        if path.exists():
            try:
                loaded = json.loads(path.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError) as exc:
                raise ValueError("journal_unreadable") from exc
            if not isinstance(loaded, dict) or not isinstance(loaded.get("keys"), dict):
                raise ValueError("journal_unreadable")
            self.data = loaded

    def get(self, key: str) -> dict | None:
        found = self.data["keys"].get(key)
        return found if isinstance(found, dict) else None

    def put(self, key: str, identity: dict, observed_digest: str | None) -> None:
        self.data["keys"][key] = {
            "identity": identity,
            "observed_digest": observed_digest,
        }
        self.path.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp_name = tempfile.mkstemp(
            prefix=".journal-", dir=str(self.path.parent)
        )
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                json.dump(self.data, handle, indent=2, sort_keys=True)
                handle.write("\n")
            os.chmod(tmp_name, 0o600)
            os.replace(tmp_name, self.path)
        except Exception:
            try:
                os.unlink(tmp_name)
            except OSError:
                pass
            raise


class ApiClient:
    def __init__(self, base_url: str, token: str, timeout: float, allow_insecure_http: bool):
        self.base_url = validate_base_url(base_url, allow_insecure_http)
        self.token = token
        self.timeout = timeout
        self.opener = urllib.request.build_opener(RejectRedirect)

    def call(
        self,
        method: str,
        path: str,
        body: dict | None = None,
        idempotency_key: str | None = None,
    ) -> tuple[int, dict | str]:
        if not path.startswith("/api/") or ".." in path or "://" in path:
            raise ValueError("unsafe_path")
        url = self.base_url + path
        payload = None if body is None else json.dumps(body).encode("utf-8")
        headers = {
            "Authorization": f"Bearer {self.token}",
            "Accept": "application/json",
        }
        if payload is not None:
            headers["Content-Type"] = "application/json"
        if idempotency_key:
            headers["Idempotency-Key"] = idempotency_key
            headers["X-Request-ID"] = idempotency_key
        request = urllib.request.Request(url, data=payload, headers=headers, method=method)
        try:
            try:
                response = self.opener.open(request, timeout=self.timeout)
            except urllib.error.HTTPError as exc:
                response = exc
            with response:
                raw = response.read()
                status = response.status
        except RedirectRefusedError:
            return 302, {"error": "redirect_refused"}
        except (urllib.error.URLError, TimeoutError, OSError, http.client.HTTPException) as exc:
            raise ConnectionError("service_unavailable") from exc
        text = raw.decode("utf-8", errors="replace")
        if not text:
            return status, {}
        try:
            parsed = json.loads(text)
        except json.JSONDecodeError:
            return status, text
        return status, parsed


def report(
    *,
    ok: bool,
    action: str,
    outcome: str,
    exit_code: int,
    config: dict,
    **extra,
) -> dict:
    body = {
        "ok": ok,
        "action": action,
        "outcome": outcome,
        "exit_code": exit_code,
        "authenticated": extra.pop("authenticated", False),
        "execute_requested": extra.pop("execute_requested", False),
        "durable_job_started": extra.pop("durable_job_started", False),
        "repeated": extra.pop("repeated", False),
        "repo_id": config.get("repo_id"),
        "pair_id": config.get("pair_id"),
        "idempotency_key": config.get("idempotency_key"),
        "operation": config.get("operation"),
        "operation_kind": config.get("operation_kind"),
        "operation_id": extra.pop("operation_id", None if action == "execute" else config.get("operation_id")),
        "needs_reconciliation": extra.pop("needs_reconciliation", False),
    }
    if action == "execute" and config.get("operation_id") is not None:
        body["expected_operation_id"] = config["operation_id"]
    if config.get("request_id") is not None:
        body["request_id"] = config["request_id"]
    body.update(extra)
    return body


def rejected(config: dict, action: str, outcome: str, **extra) -> dict:
    code = NOT_IMPLEMENTED_EXIT.get(outcome, EXIT_REJECTED)
    if outcome in {"service_unavailable", "redirect_refused"}:
        code = EXIT_UNAVAILABLE
    return report(
        ok=False,
        action=action,
        outcome=outcome,
        exit_code=code,
        config=config,
        **extra,
    )


def summarize_plan(plan: dict) -> dict:
    git = plan.get("git") if isinstance(plan.get("git"), dict) else {}
    svn = plan.get("svn") if isinstance(plan.get("svn"), dict) else {}
    approval = plan.get("approval") if isinstance(plan.get("approval"), dict) else {}
    return {
        "mode": plan.get("mode"),
        "operation": plan.get("operation"),
        "executed": plan.get("executed"),
        "published": plan.get("published"),
        "durable_job_started": plan.get("durable_job_started"),
        "policy_version": plan.get("policy_version"),
        "pair_id": plan.get("pair_id"),
        "parent_id": plan.get("parent_id"),
        "pair_generation": plan.get("pair_generation"),
        "plan_digest": plan.get("plan_digest"),
        "approval_eligible": approval.get("eligible"),
        "approval_reason": approval.get("reason"),
        "approval_binds_to": approval.get("binds_to"),
        "pair_branch": git.get("pair_branch"),
        "parent_branch": git.get("parent_branch"),
        "pair_tip": git.get("pair_tip"),
        "parent_tip": git.get("parent_tip"),
        "pair_revision": svn.get("pair_revision"),
        "parent_revision": svn.get("parent_revision"),
        "execute_status": plan.get("execute_status"),
        "reanchor_status": plan.get("reanchor_status"),
        "conflicts": plan.get("conflicts"),
    }


def not_implemented_code(plan: dict | None, error_text: str) -> str | None:
    text = error_text or ""
    if "reanchor_not_implemented" in text:
        return "reanchor_not_implemented"
    if "publish_not_implemented" in text:
        return "publish_not_implemented"
    if "refresh_execute_not_implemented" in text:
        return "refresh_execute_not_implemented"
    if isinstance(plan, dict):
        if plan.get("execute_status") == "NOT_IMPLEMENTED":
            return "refresh_execute_not_implemented"
        reason = ""
        approval = plan.get("approval")
        if isinstance(approval, dict) and isinstance(approval.get("reason"), str):
            reason = approval["reason"]
        if "not_implemented" in reason:
            return "refresh_execute_not_implemented"
    return None


def error_text(payload) -> str:
    if isinstance(payload, dict):
        value = payload.get("error") or payload.get("message") or ""
        return value if isinstance(value, str) else json.dumps(payload, sort_keys=True)
    if isinstance(payload, str):
        return payload
    return ""


def request_segment(request: str) -> str:
    # The service accepts opaque UTF-8 request IDs up to 256 bytes. Encode them
    # as a single path segment, including dots that could form relative paths.
    if not isinstance(request, str) or not request or len(request.encode()) > 256:
        raise ValueError("invalid_request_id")
    encoded = urllib.parse.quote(request, safe="")
    if encoded == "." or ".." in encoded:
        encoded = encoded.replace(".", "%2E")
    return encoded


def safe_status_path(path, repo_id: str | None, pair_id: str | None) -> str | None:
    if not isinstance(path, str):
        return None
    if not path.startswith("/api/") or "://" in path or "\\" in path:
        return None
    parts = path.split("/")
    if ".." in parts:
        return None
    parsed = urllib.parse.urlsplit(path)
    if parsed.scheme or parsed.netloc or parsed.query or parsed.fragment:
        return None
    segments = [segment for segment in parsed.path.split("/") if segment]
    if repo_id and repo_id in segments:
        return parsed.path
    if pair_id and pair_id in segments:
        return parsed.path
    return None


def pins_match(plan: dict, config: dict) -> tuple[bool, str | None]:
    if plan.get("pair_id") != config["pair_id"]:
        return False, "wrong_pair"
    if plan.get("parent_id") != config["repo_id"]:
        return False, "wrong_repo"
    git = plan.get("git") if isinstance(plan.get("git"), dict) else {}
    if config.get("expected_pair_tip") and git.get("pair_tip") != config["expected_pair_tip"]:
        return False, "stale_pair_tip"
    if (
        config.get("expected_parent_tip")
        and git.get("parent_tip") != config["expected_parent_tip"]
    ):
        return False, "stale_parent_tip"
    if config.get("git_branch") and git.get("pair_branch") != config["git_branch"]:
        return False, "branch_mismatch"
    digest = plan.get("plan_digest")
    if config.get("expected_plan_digest") and digest != config["expected_plan_digest"]:
        return False, "stale_approval"
    if not isinstance(digest, str) or not DIGEST_RE.fullmatch(digest):
        return False, "missing_plan_digest"
    return True, None


def annotate_lifecycle(result: dict, lifecycle: str | None) -> None:
    if lifecycle == "reconciliation_required":
        result["needs_reconciliation"] = True
        result["operator_action"] = RECONCILIATION_ACTION


def execute(config: dict) -> dict:
    action = config["action"]
    try:
        # Check timing before a submission can be accepted. Values outside the
        # system timer's range can also fail when passed to a socket or sleep.
        interval = config.get("poll_interval", 0)
        if not isinstance(interval, (int, float)) or not 0 <= interval <= threading.TIMEOUT_MAX:
            raise ValueError("invalid_poll_interval")
        attempts = config.get("poll_attempts", 20)
        if type(attempts) is not int or attempts <= 0:
            raise ValueError("invalid_poll_attempts")
        timeout = config.get("timeout", 30)
        if not isinstance(timeout, (int, float)) or not 0 < timeout <= threading.TIMEOUT_MAX:
            raise ValueError("invalid_timeout")
        validate_branch(config.get("git_branch"))
        for label in ("repo_id", "pair_id", "operation_id"):
            check_id(config.get(label), label)
        if config.get("request_id") is not None:
            if action != "status" or config["operation_kind"] != "refresh":
                raise ValueError("request_id_requires_refresh_status")
            request_segment(config["request_id"])
        if config.get("idempotency_key") and not KEY_RE.fullmatch(config["idempotency_key"]):
            raise ValueError("invalid_idempotency_key")
        for label in ("expected_pair_tip", "expected_parent_tip"):
            value = config.get(label)
            if value and not SHA_RE.fullmatch(value):
                raise ValueError(f"invalid_{label}")
        if config.get("expected_plan_digest") and not DIGEST_RE.fullmatch(
            config["expected_plan_digest"]
        ):
            raise ValueError("invalid_expected_plan_digest")
        if config.get("idempotency_key") and config["idempotency_key"] == config.get("token"):
            raise ValueError("invalid_idempotency_key")
    except ValueError as exc:
        return rejected(config, action, str(exc))

    if not config.get("token"):
        return rejected(config, action, "missing_token")
    if not config.get("base_url"):
        return rejected(config, action, "missing_base_url")

    journal = None
    lookup = action == "status" and (
        config["operation_kind"] != "refresh" or config.get("request_id") or config.get("operation_id")
    )
    if config.get("journal_path") and not lookup:
        try:
            journal = Journal(Path(config["journal_path"]))
        except ValueError as exc:
            return rejected(config, action, str(exc))

    identity = identity_of(config)
    previous = None
    if journal and config.get("idempotency_key"):
        previous = journal.get(config["idempotency_key"])
        if previous and previous.get("identity") != identity:
            return rejected(
                config,
                action,
                "idempotency_conflict",
                message="idempotency key is already bound to a different pin set",
            )

    try:
        client = ApiClient(
            config["base_url"],
            config["token"],
            float(config.get("timeout") or 30),
            bool(config.get("allow_insecure_http")),
        )
    except ValueError as exc:
        return rejected(config, action, str(exc))

    try:
        if action in {"preview", "execute"} and config["operation_kind"] == "refresh":
            return refresh_request(client, config, journal, previous, identity)
        if action == "status":
            return status_request(client, config, journal, previous, identity)
        if action == "cancel":
            return cancel_request(client, config)
        return rejected(config, action, "unsupported_action")
    except ConnectionError:
        return rejected(config, action, "service_unavailable", authenticated=False)
    except ValueError as exc:
        return rejected(config, action, str(exc))


def refresh_request(client: ApiClient, config: dict, journal, previous, identity) -> dict:
    if not config.get("repo_id") or not config.get("pair_id"):
        return rejected(config, config["action"], "missing_pin")
    if not config.get("idempotency_key"):
        return rejected(config, config["action"], "missing_idempotency_key")
    if config["action"] == "execute" and not config.get("expected_plan_digest"):
        return rejected(config, config["action"], "missing_plan_digest")

    status, payload = client.call(
        "POST",
        f"/api/repos/{urllib.parse.quote(config['pair_id'], safe='')}/refresh",
        {
            "operation": config["operation"],
            "execute": False,
        },
        config["idempotency_key"],
    )
    if status in {301, 302, 303, 307, 308}:
        return rejected(config, config["action"], "redirect_refused", http_status=status)
    if status == 401:
        return rejected(
            config,
            config["action"],
            "unauthorized",
            http_status=status,
            message=error_text(payload),
        )
    if status == 403:
        return rejected(
            config,
            config["action"],
            "forbidden",
            http_status=status,
            message=error_text(payload),
        )
    if status == 404:
        return rejected(
            config,
            config["action"],
            "wrong_pair",
            http_status=status,
            message=error_text(payload),
            authenticated=True,
        )
    if status >= 500:
        return rejected(
            config,
            config["action"],
            "service_unavailable",
            http_status=status,
            authenticated=True,
        )

    text = error_text(payload)
    code = not_implemented_code(payload if isinstance(payload, dict) else None, text)
    if status >= 400:
        outcome = code or "request_rejected"
        return rejected(
            config,
            config["action"],
            outcome,
            http_status=status,
            message=text,
            authenticated=True,
            server_error=text,
        )
    if not isinstance(payload, dict):
        return rejected(config, config["action"], "invalid_response", authenticated=True)

    matched, why = pins_match(payload, config)
    digest = payload.get("plan_digest")
    if (
        previous
        and previous.get("observed_digest")
        and digest
        and digest != previous.get("observed_digest")
    ):
        why = "stale_approval"
        matched = False
    if (
        matched
        and journal
        and isinstance(digest, str)
        and not (previous and previous.get("observed_digest"))
    ):
        journal.put(config["idempotency_key"], identity, digest)

    summary = summarize_plan(payload)
    repeated = previous is not None
    if not matched:
        return rejected(
            config,
            config["action"],
            why or "pin_mismatch",
            authenticated=True,
            http_status=status,
            execute_requested=False,
            repeated=repeated,
            plan=summary,
            plan_digest=digest,
            message="pinned inputs do not match the preview RepoSync returned",
        )

    implemented = not_implemented_code(payload, "")
    approval = payload.get("approval") if isinstance(payload.get("approval"), dict) else {}
    eligible = approval.get("eligible") is True and not implemented
    if config["action"] == "preview" or not eligible:
        outcome = "preview_ok" if config["action"] == "preview" else (
            implemented or "approval_not_eligible"
        )
        ok = config["action"] == "preview"
        return report(
            ok=ok,
            action=config["action"],
            outcome=outcome,
            exit_code=EXIT_OK if ok else NOT_IMPLEMENTED_EXIT.get(outcome, EXIT_NOT_IMPLEMENTED),
            config=config,
            authenticated=True,
            http_status=status,
            execute_requested=False,
            durable_job_started=False,
            repeated=repeated,
            plan_digest=digest,
            plan=summary,
            message=(
                "preview matched pinned inputs"
                if ok
                else "execute was not requested; the server preview is still not an approved execution"
            ),
        )

    return approved_execute(client, config, payload, repeated)


def definite_submit_refusal(config: dict, digest: str, status: int, payload) -> str | None:
    # AppError emits exactly {"error": message}. The same handler wraps errors
    # after an external write in HTTP 400, so neither a status code nor a word
    # occurring in an error's detail can prove that admission was refused.
    if not isinstance(payload, dict) or set(payload) != {"error"}:
        return None
    message = payload["error"]
    if not isinstance(message, str):
        return None
    if status == 401 and message in {
        "missing or invalid Authorization header", "user disabled", "session user unavailable",
        "session expired or invalid", "admin access required",
    }:
        return "unauthorized"
    if status == 403 and message == "initial setup required — please complete the setup wizard":
        return "forbidden"
    if status == 404 and message == f"repository {config['pair_id']} not found":
        return "wrong_pair"
    if status != 400:
        return None
    if message == "refresh service is not attached":
        return "execute_rejected"

    # The preview-only v1 server used these operation-specific refusals (#86).
    # When it includes a digest the refusal must name this client's preview.
    operation = config["operation"].strip().lower()
    if operation in {"", "update_pair_from_parent", "update-from-parent", "update"}:
        code = "refresh_execute_not_implemented"
        detail = (
            "update-pair-from-parent execution is a later slice and was not started. "
            "No durable job was created and no external Git or SVN write was performed."
        )
        if message in {code, f"{code}: {detail} plan_digest={digest}"}:
            return code
    if operation in {"reanchor", "re-anchor", "re_anchor", "recreate", "re-create", "recreate_pair"}:
        if message == "reanchor requires an explicit approval digest":
            return "execute_rejected"
        code = "reanchor_not_implemented"
        detail = (
            "recreate/re-anchor is a separate explicit mode and is NOT IMPLEMENTED. "
            "It is not an alias for update-from-parent, reset, force-push, or removal of the old SVN path."
        )
        if message in {code, f"{code}: {detail}"}:
            return code
    return None


def approved_execute(client: ApiClient, config: dict, preview_plan: dict, repeated: bool) -> dict:
    def unknown(http_status: int | None, outcome: str = "submission_unknown") -> dict:
        return report(
            ok=False, action="execute", outcome=outcome, exit_code=EXIT_UNAVAILABLE,
            config=config, authenticated=True, http_status=http_status,
            execute_requested=True, durable_job_started=None, repeated=repeated,
            plan_digest=preview_plan["plan_digest"], operation_id=None,
            message="The execution POST was attempted after preview. RepoSync acceptance is unknown.",
            operator_action=(
                "Before retrying, ask the RepoSync administrator to inspect the durable operation "
                "records for this parent and child pair. Share the idempotency key and preview "
                "plan digest as context. A v2 receipt lookup requires its own request ID."
            ),
        )

    try:
        status, payload = client.call(
            "POST",
            f"/api/repos/{urllib.parse.quote(config['pair_id'], safe='')}/refresh",
            {
                "operation": config["operation"],
                "execute": True,
                "expected_plan_digest": config["expected_plan_digest"],
                "idempotency_key": config["idempotency_key"],
            },
            config["idempotency_key"],
        )
    except ConnectionError:
        return unknown(None)
    if status in {301, 302, 303, 307, 308}:
        return unknown(status, "redirect_refused")
    refusal = definite_submit_refusal(config, preview_plan["plan_digest"], status, payload)
    if refusal is not None:
        return rejected(
            config, "execute", refusal, http_status=status, execute_requested=True,
            authenticated=status not in {401, 403}, message=payload["error"],
            plan_digest=preview_plan["plan_digest"], repeated=repeated,
        )
    if not 200 <= status < 300 or not isinstance(payload, dict):
        return unknown(status)

    # The submit response identifies the accepted operation. Pin all subsequent
    # reads to that identity even if a repository's latest operation changes.
    try:
        check_status_scope(config, payload)
        operation_id = returned_operation_id(config, payload)
        lifecycle = payload.get("lifecycle")
        if (
            not isinstance(lifecycle, str)
            or lifecycle not in IMPORT_STATES
            or payload.get("ok", True) is not True
            or payload.get("durable_job_started", True) is not True
            or "error" in payload
            or not_implemented_code(payload, "") is not None
        ):
            raise ValueError("invalid_response")
    except ValueError:
        return unknown(status)
    result = report(
        ok=True,
        action="execute",
        outcome=lifecycle,
        exit_code=EXIT_OK,
        config=config,
        authenticated=True,
        http_status=status,
        execute_requested=True,
        durable_job_started=True,
        repeated=repeated,
        operation_id=operation_id,
        plan_digest=preview_plan.get("plan_digest"),
        lifecycle=lifecycle,
        message="RepoSync accepted the operation",
    )
    if lifecycle in TERMINAL_LIFECYCLES:
        finish_execution(result, lifecycle)
        return result

    path = safe_status_path(payload.get("status_path"), config.get("repo_id"), config.get("pair_id"))
    target = poll_target(config, path, operation_id, payload.get("request_id"))
    if target is None:
        result.update(
            ok=False, outcome="invalid_status_path", exit_code=EXIT_UNAVAILABLE,
            message="Operation accepted; its outcome could not be verified",
        )
        return result
    return poll_status(client, target, result)


def finish_execution(result: dict, lifecycle: str) -> None:
    success = lifecycle in {"completed", "no_op"}
    result.update(
        ok=success, outcome=lifecycle, lifecycle=lifecycle,
        exit_code=EXIT_OK if success else EXIT_REJECTED,
    )
    annotate_lifecycle(result, lifecycle)


def poll_target(config: dict, path: str | None, operation_id: str, request_id) -> dict | None:
    if not path:
        return None
    parts = path.split("/")
    if len(parts) == 6 and parts[:3] == ["", "api", "repos"]:
        repo, kind, key = parts[3:]
        if repo not in (config.get("repo_id"), config.get("pair_id")):
            return None
        if kind == "import" and key == "status":
            return dict(config, operation_kind=kind, repo_id=repo, operation_id=operation_id)
        if kind in {"svn-commit", "git-push"} and key == operation_id:
            return dict(config, operation_kind=kind, repo_id=repo, operation_id=operation_id)
    if len(parts) == 8 and parts[:7] == ["", "api", "v2", "repos", config.get("pair_id"), "refresh", "requests"]:
        try:
            raw = urllib.parse.unquote_to_bytes(parts[7])
            lookup = raw.decode("utf-8")
        except UnicodeError:
            return None
        if 0 < len(raw) <= 256 and request_id in (None, lookup):
            return dict(config, operation_kind="refresh", request_id=lookup, operation_id=operation_id)
    return None


def poll_status(client: ApiClient, target: dict, result: dict) -> dict:
    attempts = int(target.get("poll_attempts") or 20)
    interval = float(target.get("poll_interval") or 0)
    for attempt in range(attempts):
        try:
            observed = read_named_or_latest_status(client, target)
        except ConnectionError:
            result.update(
                ok=False, outcome="service_unavailable", exit_code=EXIT_UNAVAILABLE, http_status=None,
                message="Operation accepted; the status service is unavailable",
            )
            return result
        result.update(
            http_status=observed.get("http_status"), ok=observed["ok"], outcome=observed["outcome"],
            exit_code=observed["exit_code"], authenticated=observed["authenticated"],
        )
        if not observed["ok"]:
            result["message"] = "Operation accepted; its status could not be verified"
            return result
        lifecycle = observed["lifecycle"]
        result.update(lifecycle=lifecycle, status=observed["status"])
        if lifecycle in TERMINAL_STATES:
            finish_execution(result, lifecycle)
            return result
        if interval and attempt + 1 < attempts:
            time.sleep(interval)
    result.update(
        ok=False, outcome="poll_timeout", exit_code=EXIT_UNAVAILABLE,
        message="Operation accepted; no verified terminal result before the polling limit",
    )
    return result


def public_status(payload: dict, kind: str) -> dict:
    if kind == "refresh":
        fields = ("version", "request_id", "operation_id", "child_id", "parent_id", "phase",
                  "svn_revision", "git_sha")
        return {field: payload[field] for field in fields}
    kept = {}
    for key in (
        "operation_id",
        "lifecycle",
        "phase",
        "can_start",
        "current_rev",
        "total_revs",
        "commits_created",
        "batches_pushed",
        "operation_type",
        "source_svn_rev",
        "source_git_sha",
        "intended_local_git_sha",
        "ok",
        "message",
        "outcome_detail",
        "intended_ref",
        "intended_git_sha",
        "last_local_svn_rev",
        "last_local_git_sha",
        "last_confirmed_svn_rev",
        "last_confirmed_git_sha",
        "durable_job_started",
    ):
        if key in payload:
            kept[key] = payload[key]
    return kept


def status_request(client: ApiClient, config: dict, journal, previous, identity) -> dict:
    kind = config["operation_kind"]
    if kind == "refresh" and config.get("request_id") is None:
        if config.get("operation_id") is not None:
            return rejected(config, "status", "missing_request_id")
        viewed = dict(config)
        viewed["action"] = "preview"
        result = refresh_request(client, viewed, journal, previous, identity)
        result["action"] = "status"
        if result.get("ok"):
            result["outcome"] = "status_ok"
            result["message"] = (
                "No named request was selected; this is a new authenticated preview"
            )
        return result
    return read_named_or_latest_status(client, config)


def read_named_or_latest_status(client: ApiClient, config: dict) -> dict:
    kind = config["operation_kind"]
    if kind == "refresh":
        if not config.get("pair_id"):
            return rejected(config, "status", "missing_pin")
        segment = request_segment(config["request_id"])
        path = f"/api/v2/repos/{config['pair_id']}/refresh/requests/{segment}"
    elif kind == "import":
        if not config.get("repo_id"):
            return rejected(config, "status", "missing_pin")
        path = f"/api/repos/{urllib.parse.quote(config['repo_id'], safe='')}/import/status"
    elif kind in {"svn-commit", "git-push"}:
        if not config.get("repo_id") or not config.get("operation_id"):
            return rejected(config, "status", "missing_pin")
        path = (
            f"/api/repos/{urllib.parse.quote(config['repo_id'], safe='')}"
            f"/{kind}/{urllib.parse.quote(config['operation_id'], safe='')}"
        )
    else:
        return rejected(config, "status", "unsupported_operation_kind")
    status, payload = client.call("GET", path, None, config.get("idempotency_key"))
    return interpret_job_response(config, "status", status, payload, False)


def cancel_request(client: ApiClient, config: dict) -> dict:
    kind = config["operation_kind"]
    if kind == "refresh":
        status, payload = client.call("GET", "/api/auth/me")
        if status in {301, 302, 303, 307, 308}:
            return rejected(config, "cancel", "redirect_refused", http_status=status)
        if status == 401:
            return rejected(config, "cancel", "unauthorized", http_status=status)
        if status != 200:
            return rejected(
                config,
                "cancel",
                "service_unavailable" if status >= 500 else "unauthorized",
                http_status=status,
                authenticated=status < 500,
            )
        return rejected(
            config,
            "cancel",
            "cancel_not_applicable",
            authenticated=True,
            http_status=status,
            execute_requested=False,
            durable_job_started=False,
            message="This client does not cancel refresh operations",
        )
    if kind == "import":
        if not config.get("repo_id") or not config.get("operation_id"):
            return rejected(config, "cancel", "missing_pin")
        path = (
            f"/api/repos/{urllib.parse.quote(config['repo_id'], safe='')}"
            f"/import/{urllib.parse.quote(config['operation_id'], safe='')}/cancel"
        )
        status, payload = client.call("POST", path, {}, config.get("idempotency_key"))
        return interpret_job_response(config, "cancel", status, payload, False)
    if kind == "svn-commit":
        status, _payload = client.call("GET", "/api/auth/me")
        if status == 401:
            return rejected(config, "cancel", "unauthorized", http_status=status)
        if status != 200:
            return rejected(
                config,
                "cancel",
                "service_unavailable" if status >= 500 else "unauthorized",
                http_status=status,
            )
        return rejected(
            config,
            "cancel",
            "cancel_not_applicable",
            authenticated=True,
            http_status=status,
            message="svn-commit cancellation is not a route on this server",
        )
    return rejected(config, "cancel", "unsupported_operation_kind")


def check_status_scope(config: dict, payload: dict) -> None:
    # Managed import/push/commit handlers bind the repository in the route but
    # do not echo it. If an envelope also names its scope it must agree.
    for field, pin, refusal in (
        ("repo_id", "repo_id", "wrong_repo"), ("parent_id", "repo_id", "wrong_repo"),
        ("pair_id", "pair_id", "wrong_pair"), ("child_id", "pair_id", "wrong_pair"),
    ):
        if field in payload and config.get(pin) is not None and payload[field] != config[pin]:
            raise ValueError(refusal)


def returned_operation_id(config: dict, payload: dict) -> str:
    operation_id = payload.get("operation_id")
    if not isinstance(operation_id, str) or not ID_RE.fullmatch(operation_id):
        raise ValueError("invalid_response")
    if config.get("operation_id") is not None and operation_id != config["operation_id"]:
        raise ValueError("wrong_operation")
    return operation_id


def refresh_phase(config: dict, payload: dict, operation_id: str) -> str:
    if (
        type(payload.get("version")) is not int or payload["version"] != 2
        or not REFRESH_UUID.fullmatch(operation_id)
        or not all(isinstance(payload.get(k), str) and payload[k] for k in ("request_id", "child_id", "parent_id"))
    ):
        raise ValueError("invalid_response")
    if payload["request_id"] != config["request_id"]:
        raise ValueError("wrong_request")
    phase = payload.get("phase")
    if not isinstance(phase, str) or phase not in REFRESH_PHASES:
        raise ValueError("invalid_response")

    # RefreshReceiptView exposes each confirmed coordinate once its publication
    # is observed. A no-op or pre-publication cancellation has neither.
    rev, sha = payload.get("svn_revision"), payload.get("git_sha")
    if "svn_revision" not in payload or "git_sha" not in payload:
        raise ValueError("invalid_response")
    if phase in {"svn_observed", "git_intent", "git_observed", "completed"}:
        if type(rev) is not int or rev <= 0:
            raise ValueError("invalid_response")
    elif rev is not None:
        raise ValueError("invalid_response")
    if phase in {"git_observed", "completed"}:
        if not isinstance(sha, str) or not SHA_RE.fullmatch(sha):
            raise ValueError("invalid_response")
    elif sha is not None:
        raise ValueError("invalid_response")
    return phase


def observed_lifecycle(config: dict, payload: dict) -> str | None:
    kind = config["operation_kind"]
    if "error" in payload or payload.get("ok", True) is not True:
        raise ValueError("invalid_response")
    if kind == "import" and "phase" in payload:
        phase = payload.get("phase")
        if not isinstance(phase, str) or phase not in IMPORT_PHASES:
            raise ValueError("invalid_response")
        if config.get("operation_id") is None and "operation_id" not in payload and "lifecycle" not in payload:
            return None

    operation_id = returned_operation_id(config, payload)
    if kind == "refresh":
        return refresh_phase(config, payload, operation_id)
    if kind in {"git-push", "svn-commit"}:
        journal_type = {"git-push": "svn_to_git_push", "svn-commit": "git_to_svn_commit"}[kind]
        if "operation_type" in payload and payload["operation_type"] != journal_type:
            raise ValueError("invalid_response")
    state = payload.get("lifecycle")
    allowed = IMPORT_STATES if kind == "import" else WRITE_STATES
    if not isinstance(state, str) or state not in allowed:
        raise ValueError("invalid_response")
    return state


def interpret_job_response(
    config: dict, action: str, status: int, payload, execute_requested: bool
) -> dict:
    if status in {301, 302, 303, 307, 308}:
        return rejected(config, action, "redirect_refused", http_status=status)
    if status == 401:
        return rejected(
            config,
            action,
            "unauthorized",
            http_status=status,
            message=error_text(payload),
        )
    if status == 403:
        return rejected(config, action, "forbidden", http_status=status, authenticated=True)
    if status == 404:
        kind = config["operation_kind"]
        message = error_text(payload)
        if kind == "refresh":
            outcome = "receipt_not_found"
        elif message == "repository not found" or (kind == "import" and action == "status"):
            outcome = "wrong_repo"
        else:
            outcome = "operation_not_found"
        return rejected(
            config,
            action,
            outcome,
            http_status=status,
            authenticated=True,
            message=message,
        )
    if status >= 500:
        return rejected(
            config,
            action,
            "service_unavailable",
            http_status=status,
            authenticated=True,
        )
    if not 200 <= status < 300:
        return rejected(
            config,
            action,
            "request_rejected",
            http_status=status,
            authenticated=True,
            message=error_text(payload),
        )
    if not isinstance(payload, dict):
        return rejected(config, action, "invalid_response", authenticated=True, http_status=status)
    try:
        check_status_scope(config, payload)
        lifecycle = observed_lifecycle(config, payload)
        if action == "cancel" and (
            payload.get("ok") is not True
            or lifecycle not in TERMINAL_LIFECYCLES | {"cancel_requested"}
        ):
            raise ValueError("invalid_response")
    except ValueError as exc:
        return rejected(config, action, str(exc), http_status=status, authenticated=True)
    result = report(
        ok=True,
        action=action,
        outcome=lifecycle or "status_ok",
        exit_code=EXIT_OK,
        config=config,
        authenticated=True,
        http_status=status,
        execute_requested=execute_requested,
        durable_job_started=lifecycle is not None and lifecycle not in TERMINAL_STATES,
        operation_id=payload.get("operation_id"),
        lifecycle=lifecycle,
        status=public_status(payload, config["operation_kind"]),
        message="RepoSync returned the operation result",
    )
    annotate_lifecycle(result, lifecycle)
    return result


def parse_args(argv: list[str] | None) -> dict:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "action",
        nargs="?",
        default=nonempty(os.environ.get("REPOSYNC_ACTION")),
        choices=["preview", "execute", "status", "cancel"],
    )
    parser.add_argument("--base-url", default=nonempty(os.environ.get("REPOSYNC_BASE_URL")))
    parser.add_argument("--repo-id", default=nonempty(os.environ.get("REPOSYNC_REPO_ID")))
    parser.add_argument("--pair-id", default=nonempty(os.environ.get("REPOSYNC_PAIR_ID")))
    parser.add_argument(
        "--idempotency-key",
        default=nonempty(os.environ.get("REPOSYNC_IDEMPOTENCY_KEY")),
    )
    parser.add_argument(
        "--expected-plan-digest",
        default=nonempty(os.environ.get("REPOSYNC_EXPECTED_PLAN_DIGEST")),
    )
    parser.add_argument(
        "--expected-pair-tip",
        default=nonempty(os.environ.get("REPOSYNC_EXPECTED_PAIR_TIP")),
    )
    parser.add_argument(
        "--expected-parent-tip",
        default=nonempty(os.environ.get("REPOSYNC_EXPECTED_PARENT_TIP")),
    )
    parser.add_argument("--git-branch", default=nonempty(os.environ.get("REPOSYNC_GIT_BRANCH")))
    parser.add_argument(
        "--operation",
        default=nonempty(os.environ.get("REPOSYNC_OPERATION")) or "update_pair_from_parent",
    )
    parser.add_argument(
        "--operation-kind",
        default=nonempty(os.environ.get("REPOSYNC_OPERATION_KIND")) or "refresh",
        choices=["refresh", "import", "svn-commit", "git-push"],
    )
    parser.add_argument(
        "--operation-id",
        default=nonempty(os.environ.get("REPOSYNC_OPERATION_ID")),
    )
    parser.add_argument(
        "--request-id", default=os.environ.get("REPOSYNC_REQUEST_ID") or None,
        help="Read this v2 refresh request receipt for --pair-id",
    )
    parser.add_argument(
        "--journal",
        default=nonempty(os.environ.get("REPOSYNC_REQUEST_JOURNAL"))
        or ".reposync/operation-journal.json",
    )
    parser.add_argument("--timeout", type=float, default=os.environ.get("REPOSYNC_TIMEOUT", "30"))
    parser.add_argument(
        "--poll-interval",
        type=float,
        default=os.environ.get("REPOSYNC_POLL_INTERVAL", "0"),
    )
    parser.add_argument(
        "--poll-attempts",
        type=int,
        default=os.environ.get("REPOSYNC_POLL_ATTEMPTS", "20"),
    )
    parser.add_argument(
        "--allow-insecure-http",
        action="store_true",
        default=os.environ.get("REPOSYNC_ALLOW_INSECURE_HTTP") == "1",
    )
    args = parser.parse_args(argv)
    token = nonempty(os.environ.get("REPOSYNC_API_TOKEN"))
    return {
        "action": args.action,
        "base_url": args.base_url,
        "token": token,
        "repo_id": args.repo_id,
        "pair_id": args.pair_id,
        "idempotency_key": args.idempotency_key,
        "expected_plan_digest": args.expected_plan_digest,
        "expected_pair_tip": args.expected_pair_tip,
        "expected_parent_tip": args.expected_parent_tip,
        "git_branch": args.git_branch,
        "operation": args.operation,
        "operation_kind": args.operation_kind,
        "operation_id": args.operation_id,
        "request_id": args.request_id,
        "journal_path": args.journal,
        "timeout": args.timeout,
        "poll_interval": args.poll_interval,
        "poll_attempts": args.poll_attempts,
        "allow_insecure_http": args.allow_insecure_http,
    }


def emit(result: dict, secrets: list[str]) -> None:
    text = json.dumps(result, indent=2, sort_keys=True)
    sys.stdout.write(redact(text, secrets) + "\n")


def main(argv: list[str] | None = None) -> int:
    config = parse_args(argv)
    if not config.get("action"):
        emit(
            rejected(config, "unknown", "missing_action"),
            [config.get("token") or ""],
        )
        return EXIT_REJECTED
    result = execute(config)
    emit(result, [config.get("token") or ""])
    return int(result.get("exit_code", EXIT_REJECTED))


if __name__ == "__main__":
    sys.exit(main())
