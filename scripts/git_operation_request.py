#!/usr/bin/env python3
"""Manually request a RepoSync preview, status, or cancel through the HTTP API.

RepoSync remains the authority for locking, provenance, checkpoints, cancellation,
and recovery. This client does not run ``svn merge``, edit SQLite, delete remotes,
force-push, or interpolate branch names into a shell.

The pair-refresh execute path is still refused by the server
(``refresh_execute_not_implemented``). This client reports that result. It does
not invent an execution job.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import tempfile
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
            with self.opener.open(request, timeout=self.timeout) as response:
                raw = response.read()
                status = response.status
        except RedirectRefusedError:
            return 302, {"error": "redirect_refused"}
        except urllib.error.HTTPError as exc:
            raw = exc.read()
            status = exc.code
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
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
        "operation_id": extra.pop("operation_id", config.get("operation_id")),
        "needs_reconciliation": extra.pop("needs_reconciliation", False),
    }
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
        validate_branch(config.get("git_branch"))
        for label in ("repo_id", "pair_id", "operation_id"):
            check_id(config.get(label), label)
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
    if config.get("journal_path"):
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


def approved_execute(client: ApiClient, config: dict, preview_plan: dict, repeated: bool) -> dict:
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
    text = error_text(payload)
    code = not_implemented_code(payload if isinstance(payload, dict) else None, text)
    if status in {301, 302, 303, 307, 308}:
        return rejected(
            config,
            "execute",
            "redirect_refused",
            http_status=status,
            execute_requested=True,
            authenticated=True,
        )
    if status == 401:
        return rejected(
            config,
            "execute",
            "unauthorized",
            http_status=status,
            execute_requested=True,
        )
    if status == 404:
        return rejected(
            config,
            "execute",
            "wrong_pair",
            http_status=status,
            execute_requested=True,
            authenticated=True,
        )
    if code or status >= 400:
        return rejected(
            config,
            "execute",
            code or "execute_rejected",
            http_status=status,
            execute_requested=True,
            authenticated=True,
            durable_job_started=False,
            message=text,
            plan_digest=preview_plan.get("plan_digest"),
        )
    if not isinstance(payload, dict):
        return rejected(
            config,
            "execute",
            "invalid_response",
            execute_requested=True,
            authenticated=True,
        )

    operation_id = payload.get("operation_id")
    if operation_id is not None and not isinstance(operation_id, str):
        operation_id = None
    if operation_id:
        try:
            check_id(operation_id, "operation_id")
        except ValueError:
            return rejected(
                config,
                "execute",
                "invalid_operation_id",
                execute_requested=True,
                authenticated=True,
            )
    lifecycle = payload.get("lifecycle") if isinstance(payload.get("lifecycle"), str) else None
    result = report(
        ok=True,
        action="execute",
        outcome=lifecycle or "execute_accepted",
        exit_code=EXIT_OK,
        config=config,
        authenticated=True,
        http_status=status,
        execute_requested=True,
        durable_job_started=bool(payload.get("durable_job_started") or operation_id),
        repeated=repeated,
        operation_id=operation_id,
        plan_digest=preview_plan.get("plan_digest"),
        lifecycle=lifecycle,
        message="execute response received from RepoSync",
    )
    annotate_lifecycle(result, lifecycle)
    status_path = safe_status_path(
        payload.get("status_path"), config.get("repo_id"), config.get("pair_id")
    )
    if operation_id and lifecycle not in TERMINAL_LIFECYCLES and status_path:
        polled = poll_status(client, config, status_path)
        result.update(polled)
    elif lifecycle in TERMINAL_LIFECYCLES:
        result["outcome"] = lifecycle
    return result


def poll_status(client: ApiClient, config: dict, path: str) -> dict:
    attempts = int(config.get("poll_attempts") or 20)
    interval = float(config.get("poll_interval") or 0)
    last = {}
    for _ in range(attempts):
        status, payload = client.call("GET", path, None, config.get("idempotency_key"))
        if status == 401:
            return {
                "ok": False,
                "outcome": "unauthorized",
                "exit_code": EXIT_REJECTED,
                "http_status": status,
                "message": "status poll was rejected",
            }
        if status == 404:
            return {
                "ok": False,
                "outcome": "wrong_repo",
                "exit_code": EXIT_REJECTED,
                "http_status": status,
                "authenticated": True,
            }
        if status >= 500 or status in {301, 302, 303, 307, 308}:
            return {
                "ok": False,
                "outcome": "service_unavailable" if status >= 500 else "redirect_refused",
                "exit_code": EXIT_UNAVAILABLE,
                "http_status": status,
                "authenticated": True,
            }
        if not isinstance(payload, dict):
            return {
                "ok": False,
                "outcome": "invalid_response",
                "exit_code": EXIT_REJECTED,
                "authenticated": True,
            }
        lifecycle = payload.get("lifecycle")
        if not isinstance(lifecycle, str):
            lifecycle = None
        last = {
            "ok": True,
            "outcome": lifecycle or "status_ok",
            "exit_code": EXIT_OK,
            "authenticated": True,
            "http_status": status,
            "lifecycle": lifecycle,
            "operation_id": payload.get("operation_id") or config.get("operation_id"),
            "status": public_status(payload),
        }
        if lifecycle == "reconciliation_required":
            last["needs_reconciliation"] = True
            last["operator_action"] = RECONCILIATION_ACTION
        if lifecycle in TERMINAL_LIFECYCLES or lifecycle is None:
            return last
        if interval:
            import time

            time.sleep(interval)
    last["outcome"] = "poll_timeout"
    last["ok"] = False
    last["exit_code"] = EXIT_UNAVAILABLE
    return last


def public_status(payload: dict) -> dict:
    kept = {}
    for key in (
        "operation_id",
        "lifecycle",
        "phase",
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
    if kind == "refresh":
        viewed = dict(config)
        viewed["action"] = "preview"
        result = refresh_request(client, viewed, journal, previous, identity)
        result["action"] = "status"
        if result.get("ok"):
            result["outcome"] = "status_ok"
            result["message"] = (
                "refresh has no durable job; status is a new authenticated preview"
            )
        return result
    if kind == "import":
        if not config.get("repo_id"):
            return rejected(config, "status", "missing_pin")
        path = f"/api/repos/{urllib.parse.quote(config['repo_id'], safe='')}/import/status"
    elif kind == "svn-commit":
        if not config.get("repo_id") or not config.get("operation_id"):
            return rejected(config, "status", "missing_pin")
        path = (
            f"/api/repos/{urllib.parse.quote(config['repo_id'], safe='')}"
            f"/svn-commit/{urllib.parse.quote(config['operation_id'], safe='')}"
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
            message=(
                "refresh_execute_not_implemented: no durable refresh job exists to cancel"
            ),
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
        outcome = "wrong_repo" if config["operation_kind"] != "refresh" else "wrong_pair"
        return rejected(
            config,
            action,
            outcome,
            http_status=status,
            authenticated=True,
            message=error_text(payload),
        )
    if status >= 500:
        return rejected(
            config,
            action,
            "service_unavailable",
            http_status=status,
            authenticated=True,
        )
    if status >= 400:
        return rejected(
            config,
            action,
            "request_rejected",
            http_status=status,
            authenticated=True,
            message=error_text(payload),
        )
    if not isinstance(payload, dict):
        return rejected(config, action, "invalid_response", authenticated=True)
    body_repo = payload.get("repo_id") or payload.get("parent_id")
    if isinstance(body_repo, str) and config.get("repo_id") and body_repo != config["repo_id"]:
        return rejected(
            config,
            action,
            "wrong_repo",
            authenticated=True,
            http_status=status,
        )
    body_pair = payload.get("pair_id")
    if isinstance(body_pair, str) and config.get("pair_id") and body_pair != config["pair_id"]:
        return rejected(
            config,
            action,
            "wrong_pair",
            authenticated=True,
            http_status=status,
        )
    lifecycle = payload.get("lifecycle") if isinstance(payload.get("lifecycle"), str) else None
    operation_id = payload.get("operation_id") if isinstance(payload.get("operation_id"), str) else config.get(
        "operation_id"
    )
    outcome = lifecycle or ("cancel_requested" if action == "cancel" else "status_ok")
    result = report(
        ok=True,
        action=action,
        outcome=outcome,
        exit_code=EXIT_OK,
        config=config,
        authenticated=True,
        http_status=status,
        execute_requested=execute_requested,
        durable_job_started=lifecycle
        not in {None, "completed", "cancelled", "failed", "reconciliation_required"},
        operation_id=operation_id,
        lifecycle=lifecycle,
        status=public_status(payload),
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
        choices=["refresh", "import", "svn-commit"],
    )
    parser.add_argument(
        "--operation-id",
        default=nonempty(os.environ.get("REPOSYNC_OPERATION_ID")),
    )
    parser.add_argument(
        "--journal",
        default=nonempty(os.environ.get("REPOSYNC_REQUEST_JOURNAL"))
        or ".reposync/operation-journal.json",
    )
    parser.add_argument("--timeout", type=float, default=float(os.environ.get("REPOSYNC_TIMEOUT", "30")))
    parser.add_argument(
        "--poll-interval",
        type=float,
        default=float(os.environ.get("REPOSYNC_POLL_INTERVAL", "0")),
    )
    parser.add_argument(
        "--poll-attempts",
        type=int,
        default=int(os.environ.get("REPOSYNC_POLL_ATTEMPTS", "20")),
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
