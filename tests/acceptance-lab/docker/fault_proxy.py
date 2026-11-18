#!/usr/bin/env python3
"""TLS reverse proxy with a deterministic drop-after-forward control.

Used for lost-reply cases: the upstream write is allowed to finish, then the
proxy discards the response so the client sees a transport failure. This is
not a claim of distributed atomic rollback.
"""

import json
import os
import socket
import ssl
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

UPSTREAM_HOST = os.environ.get("UPSTREAM_HOST", "svn")
UPSTREAM_PORT = int(os.environ.get("UPSTREAM_PORT", "8443"))
LISTEN_PORT = int(os.environ.get("LISTEN_PORT", "8443"))
CONTROL_PORT = int(os.environ.get("CONTROL_PORT", "8090"))
CA = os.environ.get("UPSTREAM_CA", "/certs/ca.crt")
CERT = os.environ.get("PROXY_CERT", "/certs/fault.crt")
KEY = os.environ.get("PROXY_KEY", "/certs/fault.key")

state_lock = threading.Lock()
state = {
    "drop_responses_remaining": 0,
    "client_connections": 0,
    "upstream_connections": 0,
    "dropped_responses": 0,
    "forwarded_responses": 0,
    "tls_accepted": 0,
    "tls_rejected_ssl": 0,
    "handler_entered": 0,
    "handler_empty_request": 0,
    "handler_send_completed": 0,
    "handler_close_attempted": 0,
    "handler_close_completed": 0,
    "successful_tls_shutdown_attempted": 0,
    "successful_tls_shutdown_completed": 0,
    "successful_tls_shutdown_fallback": 0,
}


def diagnostic_count(key):
    # Fixed call-site keys only; diagnostics never change transport control flow.
    try:
        with state_lock:
            state[key] += 1
    except Exception:
        pass


# #199-only synthetic gate. Independent of legacy drop/reset state and disabled
# until explicitly armed for a run-specific exact request target.
import hashlib
import hmac
import re
import time

archive199_condition = threading.Condition()
archive199_gate = None
archive199_used = set()
ARCHIVE199_LIMIT = 1024


class Archive199Abort(RuntimeError):
    pass


def archive199_snapshot(gate):
    if gate is None:
        return {"state": "inactive"}
    return {key: value for key, value in gate.items()
            if key not in ("owner_token", "deadline")}


def archive199_expire(gate):
    if gate and gate["state"] in ("armed", "inflight", "blocked"):
        if time.monotonic() >= gate["deadline"]:
            gate["state"] = "timed_out"
            gate["finished_at"] = time.time()
            archive199_condition.notify_all()


def archive199_fail(gate, reason):
    gate["state"] = "aborted"
    gate["error"] = reason
    gate["finished_at"] = time.time()
    archive199_condition.notify_all()
    raise Archive199Abort(reason)


def archive199_framed(message, response=False, method=None):
    # Inspection only: the original parser is deliberately not replaced.
    # It does not support chunked or close-delimited bodies; never witness
    # those as a completed upstream response for a #199 gate.
    head, separator, body = message.partition(b"\r\n\r\n")
    if not separator:
        return False
    lines = head.split(b"\r\n")
    lengths = []
    for line in lines[1:]:
        name, colon, value = line.partition(b":")
        if not colon:
            return False
        if name.lower() == b"transfer-encoding":
            return False
        if name.lower() == b"content-length":
            value = value.strip()
            if not value.isdigit():
                return False
            lengths.append(int(value))
    if len(lengths) > 1:
        return False
    if response:
        parts = lines[0].split(b" ")
        if len(parts) < 2 or parts[0] not in (b"HTTP/1.0", b"HTTP/1.1"):
            return False
        if not parts[1].isdigit():
            return False
        status = int(parts[1])
        # Interim responses do not establish a completed operation.
        if not 200 <= status <= 599:
            return False
        if method == "HEAD" or status in (204, 304):
            return not body
        return len(lengths) == 1 and len(body) == lengths[0]
    return len(body) == (lengths[0] if lengths else 0)


def archive199_wait(gate):
    gate["state"] = "blocked"
    gate["witnessed_at"] = time.time()
    archive199_condition.notify_all()
    while gate["state"] == "blocked":
        archive199_expire(gate)
        if gate["state"] != "blocked":
            break
        archive199_condition.wait(max(0.001, gate["deadline"] - time.monotonic()))
    if gate["state"] != "released":
        raise Archive199Abort("owned #199 request aborted or timed out")


def archive199_before(request):
    first = request.split(b"\r\n", 1)[0].split(b" ")
    if len(first) != 3:
        return None
    try:
        method, target, protocol = [part.decode("ascii") for part in first]
    except UnicodeDecodeError:
        return None
    with archive199_condition:
        gate = archive199_gate
        archive199_expire(gate)
        if (not gate or gate["method"] != method or gate["target"] != target
                or protocol not in ("HTTP/1.0", "HTTP/1.1")):
            return None
        # Terminal witnesses remain readable, but the gate's finite lease
        # must not become a permanent denial of this endpoint. A later retry
        # passes normally and requires separate observation by the harness;
        # it is never evidence that the original request remained held.
        if gate["state"] in ("released", "aborted", "timed_out"):
            gate["terminal_bypasses"] += 1
            return None
        if gate["state"] != "armed":
            gate["rejected_repeats"] += 1
            raise Archive199Abort("owned #199 request already consumed")
        gate["matched_at"] = time.time()
        if not archive199_framed(request):
            archive199_fail(gate, "unsupported_or_incomplete_request_framing")
        if gate["phase"] == "before-forward":
            archive199_wait(gate)
        else:
            gate["state"] = "inflight"
        return gate



def archive199_upstream_timeout(gate, sock=None):
    if gate is None or gate["phase"] != "after-forward":
        return 30
    with archive199_condition:
        archive199_expire(gate)
        if gate["state"] != "inflight":
            if sock is not None:
                sock.close()
            raise Archive199Abort("owned #199 upstream lease ended")
        return min(30, max(0.001, gate["deadline"] - time.monotonic()))


def archive199_bound_upstream(gate, upstream):
    if gate is None or gate["phase"] != "after-forward":
        return
    upstream.settimeout(archive199_upstream_timeout(gate, upstream))

    def deadline():
        with archive199_condition:
            archive199_expire(gate)
            close_socket = (not gate["upstream_response_complete"]
                            and gate["state"] in ("inflight", "aborted", "timed_out"))
        if close_socket:
            # Per-recv timeouts alone cannot bound a trickled HTTP body.
            # Close only this captured socket, never a later matching request.
            try:
                upstream.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            upstream.close()

    timer = threading.Timer(max(0, gate["deadline"] - time.monotonic()), deadline)
    timer.daemon = True
    timer.start()


def archive199_after(gate, response):
    if gate is None or gate["phase"] != "after-forward":
        return
    with archive199_condition:
        archive199_expire(gate)
        if gate["state"] != "inflight":
            raise Archive199Abort("owned #199 request aborted or timed out upstream")
        if not archive199_framed(response, response=True, method=gate["method"]):
            archive199_fail(gate, "unsupported_or_incomplete_response_framing")
        gate["response_status"] = int(response.split(b" ", 2)[1])
        gate["upstream_response_complete"] = True
        archive199_wait(gate)


def archive199_control(body):
    global archive199_gate
    if not isinstance(body, dict):
        return 400, {"ok": False, "error": "object required"}
    action = body.get("action")
    token = body.get("owner_token")
    run = body.get("run_id")
    request = body.get("request_id")
    if (not isinstance(token, str) or not re.fullmatch(r"[A-Za-z0-9_-]{32,128}", token)
            or not isinstance(run, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", run)
            or not isinstance(request, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", request)):
        return 400, {"ok": False, "error": "bounded owner token, run_id and request_id required"}
    with archive199_condition:
        gate = archive199_gate
        archive199_expire(gate)
        if action == "arm":
            method, target, phase = body.get("method"), body.get("target"), body.get("phase")
            scope_path = body.get("scope_path")
            timeout = body.get("timeout_seconds")
            if (not isinstance(method, str) or not re.fullmatch(r"[A-Z]{1,20}", method)
                    or not isinstance(target, str) or len(target) > 2048
                    or not target.startswith("/") or target.startswith("//")
                    or any(ord(char) <= 32 or ord(char) >= 127 for char in target)
                    or "#" in target or "\\" in target
                    or "%" in target.split("?", 1)[0]
                    or any(segment in (".", "..") for segment in target.split("?", 1)[0].split("/"))
                    or not isinstance(scope_path, str)
                    or not re.fullmatch(r"/(?:svn|git)/[A-Za-z0-9_-]{1,120}", scope_path)
                    or not (target.split("?", 1)[0] == scope_path
                            or target.split("?", 1)[0].startswith(scope_path + "/"))
                    or phase not in ("before-forward", "after-forward")
                    or isinstance(timeout, bool) or not isinstance(timeout, (int, float))
                    or not 1 <= timeout <= 120):
                return 400, {"ok": False, "error": "exact run-scoped method/target, phase and 1..120 second timeout required"}
            if gate and gate["state"] in ("armed", "inflight", "blocked"):
                return 409, {"ok": False, "error": "gate already owned"}
            identity = (run, request)
            token_digest = hashlib.sha256(token.encode()).hexdigest()
            if identity in archive199_used or token_digest in archive199_used:
                return 409, {"ok": False, "error": "run/request or owner token already used"}
            if len(archive199_used) >= ARCHIVE199_LIMIT * 2:
                return 409, {"ok": False, "error": "gate identity capacity exhausted; no reset supported"}
            archive199_used.update((identity, token_digest))
            archive199_gate = {
                "state": "armed", "owner_token": token, "run_id": run,
                "request_id": request, "method": method, "target": target,
                "scope_path": scope_path, "phase": phase, "timeout_seconds": timeout,
                "deadline": time.monotonic() + timeout, "armed_at": time.time(),
                "rejected_repeats": 0, "terminal_bypasses": 0,
                "upstream_response_complete": False,
            }
            return 200, {"ok": True, "gate": archive199_snapshot(archive199_gate)}
        if action not in ("release", "abort"):
            return 400, {"ok": False, "error": "action must be arm, release or abort"}
        if (not gate or not hmac.compare_digest(token, gate["owner_token"])
                or run != gate["run_id"] or request != gate["request_id"]):
            return 403, {"ok": False, "error": "gate owner mismatch"}
        if action == "release" and gate["state"] != "blocked":
            return 409, {"ok": False, "error": "release requires a witnessed blocked request"}
        if action == "abort" and gate["state"] not in ("armed", "inflight", "blocked"):
            return 409, {"ok": False, "error": "gate is terminal"}
        gate["state"] = "released" if action == "release" else "aborted"
        gate["finished_at"] = time.time()
        archive199_condition.notify_all()
        return 200, {"ok": True, "gate": archive199_snapshot(gate)}


def take_drop():
    with state_lock:
        if state["drop_responses_remaining"] > 0:
            state["drop_responses_remaining"] -= 1
            state["dropped_responses"] += 1
            return True
        state["forwarded_responses"] += 1
        return False


def read_http_message(sock):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = sock.recv(4096)
        if not chunk:
            return data
        data += chunk
        if len(data) > 8_000_000:
            raise RuntimeError("header too large")
    head, rest = data.split(b"\r\n\r\n", 1)
    headers = head.decode("iso-8859-1", errors="replace").split("\r\n")
    length = 0
    for line in headers[1:]:
        if line.lower().startswith("content-length:"):
            length = int(line.split(":", 1)[1].strip())
    body = rest
    while len(body) < length:
        chunk = sock.recv(length - len(body))
        if not chunk:
            break
        body += chunk
    return head + b"\r\n\r\n" + body[:length]



def read_http_response(sock, method):
    """Read one complete bounded response; normalize only validated chunked bodies.

    Extensions/trailers, interim and close-delimited responses are deliberately
    unsupported. No incomplete transport can become a complete gate witness.
    """
    def framing_error(code):
        try:
            print(json.dumps({"event": "upstream_response_failure", "reason": code}), flush=True)
        except Exception:
            pass
        return RuntimeError(code)

    limit = 8_000_000
    buffered = bytearray()
    received = 0

    def receive():
        nonlocal received
        try:
            part = sock.recv(4096)
        except Exception:
            # The transport exception may contain sensitive endpoint details.
            framing_error("transport_error")
            raise
        if not part:
            raise framing_error("truncated_upstream_response")
        received += len(part)
        if received > limit:
            raise framing_error("upstream_response_too_large")
        buffered.extend(part)

    def line(bound):
        while b"\r\n" not in buffered:
            if len(buffered) > bound:
                raise framing_error("upstream_framing_line_too_large")
            receive()
        end = buffered.index(b"\r\n")
        if end > bound:
            raise framing_error("upstream_framing_line_too_large")
        value = bytes(buffered[:end])
        del buffered[:end + 2]
        return value

    def exact(length):
        if length > limit:
            raise framing_error("upstream_body_too_large")
        while len(buffered) < length:
            receive()
        value = bytes(buffered[:length])
        del buffered[:length]
        return value

    start = line(8192)
    if not re.fullmatch(rb"HTTP/1\.[01] [2-5][0-9]{2}(?: [^\r\n]*)?", start):
        raise framing_error("unsupported_upstream_status")
    status = int(start.split(b" ")[1])
    headers, lengths, encodings = [], [], []
    header_bytes = len(start) + 2
    while True:
        item = line(8192)
        header_bytes += len(item) + 2
        if header_bytes > 65536:
            raise framing_error("upstream_headers_too_large")
        if not item:
            break
        name, colon, value = item.partition(b":")
        if not colon or not re.fullmatch(rb"[!#$%&'*+.^_`|~0-9A-Za-z-]+", name):
            raise framing_error("malformed_upstream_header")
        value = value.strip(b" \t")
        if any(byte < 32 and byte != 9 or byte == 127 for byte in value):
            raise framing_error("invalid_upstream_header_value")
        lower = name.lower()
        if lower == b"content-length":
            if not re.fullmatch(rb"[0-9]{1,10}", value):
                raise framing_error("invalid_upstream_length")
            lengths.append(int(value))
        elif lower == b"transfer-encoding":
            encodings.append(value.lower())
        elif lower == b"trailer":
            raise framing_error("unsupported_upstream_trailers")
        headers.append(item)
    if len(lengths) > 1 or len(encodings) > 1 or lengths and encodings:
        raise framing_error("ambiguous_upstream_framing")
    if encodings and encodings != [b"chunked"]:
        raise framing_error("unsupported_upstream_transfer_encoding")
    if method == b"HEAD" or status in (204, 304):
        if encodings or buffered or status == 204 and lengths and lengths[0] != 0:
            raise framing_error("unexpected_bodyless_response_framing")
        body = b""
    elif encodings:
        body = bytearray()
        count = 0
        while True:
            size_line = line(128)
            if not re.fullmatch(rb"[0-9A-Fa-f]{1,8}", size_line):
                raise framing_error("unsupported_upstream_chunk_size")
            size = int(size_line, 16)
            count += 1
            if count > 100000 or len(body) + size > limit:
                raise framing_error("upstream_chunks_too_large")
            if size == 0:
                if line(8192):
                    raise framing_error("unsupported_upstream_trailers")
                break
            body.extend(exact(size))
            if exact(2) != b"\r\n":
                raise framing_error("invalid_upstream_chunk_terminator")
        headers = [item for item in headers if item.partition(b":")[0].lower() != b"transfer-encoding"]
        headers.append(b"Content-Length: " + str(len(body)).encode("ascii"))
        body = bytes(body)
    elif lengths:
        body = exact(lengths[0])
    else:
        raise framing_error("unsupported_close-delimited_upstream_response")
    if buffered:
        raise framing_error("unexpected_bytes_after_upstream_response")
    # This proxy handles exactly one request per connection. Never advertise
    # the upstream connection's persistence on the separate client connection.
    headers = [item for item in headers if item.partition(b":")[0].lower()
               not in (b"connection", b"keep-alive")]
    headers.append(b"Connection: close")
    try:
        print(json.dumps({"event": "upstream_response_framing", "status": status,
                          "framing": "chunked" if encodings else "content-length" if lengths else "bodyless",
                          "entity_bytes": len(body)}), flush=True)
    except Exception:
        pass  # Logging is not part of the completeness decision.
    return b"\r\n".join([start, *headers]) + b"\r\n\r\n" + body



def close_successful_response(client):
    """Only a fully sent, non-dropped response receives bounded TLS shutdown."""
    closing = client
    diagnostic_count("successful_tls_shutdown_attempted")
    try:
        client.settimeout(2.0)
        closing = client.unwrap()
        diagnostic_count("successful_tls_shutdown_completed")
    except (ssl.SSLError, OSError):
        # No exception strings: transport errors can contain endpoint details.
        diagnostic_count("successful_tls_shutdown_fallback")
    diagnostic_count("handler_close_attempted")
    closing.close()
    diagnostic_count("handler_close_completed")


def handle_client(client):
    diagnostic_count("handler_entered")
    with state_lock:
        state["client_connections"] += 1
    stage, method, framing, request_bytes, response_bytes = "request-read", "unknown", "unknown", 0, 0
    try:
        request = read_http_message(client)
        if not request:
            diagnostic_count("handler_empty_request")
            diagnostic_count("handler_close_attempted")
            client.close()
            diagnostic_count("handler_close_completed")
            return
        request_bytes = len(request)
        raw_method = request.split(b" ", 1)[0]
        method = raw_method.decode("ascii") if raw_method in (
            b"OPTIONS", b"REPORT", b"PROPFIND", b"GET", b"HEAD", b"PUT",
            b"POST", b"DELETE", b"MKCOL", b"COPY", b"MERGE", b"CHECKOUT",
            b"PROPPATCH", b"LOCK", b"UNLOCK") else "other"
        names = [item.partition(b":")[0].lower() for item in request.partition(b"\r\n\r\n")[0].split(b"\r\n")[1:]]
        framing = "ambiguous" if b"content-length" in names and b"transfer-encoding" in names else "transfer-encoding" if b"transfer-encoding" in names else "content-length" if b"content-length" in names else "none"
        stage = "before-gate"
        archive199_ticket = archive199_before(request)
        stage = "tls-context"
        context = ssl.create_default_context(cafile=CA)
        context.check_hostname = True
        context.verify_mode = ssl.CERT_REQUIRED
        stage = "upstream-connect"
        raw = socket.create_connection(
            (UPSTREAM_HOST, UPSTREAM_PORT),
            timeout=archive199_upstream_timeout(archive199_ticket),
        )
        stage = "upstream-timeout"
        raw.settimeout(archive199_upstream_timeout(archive199_ticket, raw))
        stage = "upstream-tls"
        upstream = context.wrap_socket(raw, server_hostname=UPSTREAM_HOST)
        stage = "upstream-gate-bound"
        archive199_bound_upstream(archive199_ticket, upstream)
        with state_lock:
            state["upstream_connections"] += 1
        stage = "upstream-send"
        upstream.sendall(request)
        stage = "response-read"
        response = read_http_response(upstream, request.split(b" ", 1)[0])
        response_bytes = len(response)
        stage = "upstream-close"
        upstream.close()
        stage = "after-gate"
        archive199_after(archive199_ticket, response)
        stage = "legacy-drop"
        if take_drop():
            diagnostic_count("handler_close_attempted")
            client.close()
            diagnostic_count("handler_close_completed")
            return
        stage = "client-send"
        client.sendall(response)
        diagnostic_count("handler_send_completed")
    except Exception as error:
        try:
            known = (Archive199Abort, ssl.SSLCertVerificationError, ssl.SSLError,
                     ConnectionResetError, BrokenPipeError, TimeoutError, OSError,
                     RuntimeError, ValueError)
            error_type = type(error).__name__ if type(error) in known else "other"
            print(json.dumps({"event": "proxy_handler_failure", "stage": stage,
                              "error_type": error_type, "method": method,
                              "request_framing": framing, "request_bytes": request_bytes,
                              "response_bytes": response_bytes}), flush=True)
        except Exception:
            pass
        try:
            diagnostic_count("handler_close_attempted")
            client.close()
            diagnostic_count("handler_close_completed")
        except Exception:
            pass
        return
    close_successful_response(client)


class Control(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _json(self, code, payload):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/archive199/gate":
            with archive199_condition:
                archive199_expire(archive199_gate)
                snapshot = archive199_snapshot(archive199_gate)
            self._json(200, {"ok": True, "gate": snapshot})
            return
        if self.path != "/stats":
            self._json(404, {"ok": False})
            return
        with state_lock:
            snapshot = dict(state)
        self._json(200, snapshot)

    def do_POST(self):
        if self.path == "/archive199/gate":
            # Only this namespace has new control framing/size requirements.
            lengths = self.headers.get_all("Content-Length", [])
            if (len(lengths) != 1 or len(lengths[0]) > 4 or not lengths[0].isdigit()
                    or not 0 < int(lengths[0]) <= 8192
                    or self.headers.get("Transfer-Encoding") is not None):
                self.close_connection = True
                self._json(400, {"ok": False, "error": "bounded Content-Length required"})
                return
            previous_timeout = self.connection.gettimeout()
            self.connection.settimeout(5)
            try:
                raw = self.rfile.read(int(lengths[0]))
                if len(raw) != int(lengths[0]):
                    raise ValueError("incomplete control body")
                body = json.loads(raw.decode("utf-8"))
            except (ValueError, UnicodeError, OSError):
                self.close_connection = True
                self._json(400, {"ok": False, "error": "invalid control body"})
                return
            finally:
                self.connection.settimeout(previous_timeout)
            code, payload = archive199_control(body)
            self._json(code, payload)
            return
        length = int(self.headers.get("Content-Length", "0") or "0")
        raw = self.rfile.read(length) if length else b"{}"
        try:
            body = json.loads(raw.decode() or "{}")
        except json.JSONDecodeError:
            body = {}
        if self.path == "/drop-next-response":
            count = int(body.get("count", 1))
            with state_lock:
                state["drop_responses_remaining"] += count
            self._json(200, {"ok": True, "armed": count})
            return
        if self.path == "/reset":
            with state_lock:
                state["drop_responses_remaining"] = 0
            self._json(200, {"ok": True})
            return
        self._json(404, {"ok": False})

    def log_message(self, fmt, *args):
        return


def serve_tls():
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(CERT, KEY)
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("0.0.0.0", LISTEN_PORT))
    server.listen(50)
    while True:
        client, _addr = server.accept()
        try:
            tls_client = context.wrap_socket(client, server_side=True)
        except ssl.SSLError:
            diagnostic_count("tls_rejected_ssl")
            client.close()
            continue
        diagnostic_count("tls_accepted")
        threading.Thread(target=handle_client, args=(tls_client,), daemon=True).start()


def main():
    threading.Thread(target=serve_tls, daemon=True).start()
    ThreadingHTTPServer(("0.0.0.0", CONTROL_PORT), Control).serve_forever()


if __name__ == "__main__":
    main()
