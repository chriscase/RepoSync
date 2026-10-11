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
}


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


def handle_client(client):
    with state_lock:
        state["client_connections"] += 1
    try:
        request = read_http_message(client)
        if not request:
            client.close()
            return
        context = ssl.create_default_context(cafile=CA)
        context.check_hostname = True
        context.verify_mode = ssl.CERT_REQUIRED
        raw = socket.create_connection((UPSTREAM_HOST, UPSTREAM_PORT), timeout=30)
        upstream = context.wrap_socket(raw, server_hostname=UPSTREAM_HOST)
        with state_lock:
            state["upstream_connections"] += 1
        upstream.sendall(request)
        response = read_http_message(upstream)
        upstream.close()
        if take_drop():
            client.close()
            return
        client.sendall(response)
    except Exception:
        try:
            client.close()
        except Exception:
            pass
        return
    client.close()


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
        if self.path != "/stats":
            self._json(404, {"ok": False})
            return
        with state_lock:
            snapshot = dict(state)
        self._json(200, snapshot)

    def do_POST(self):
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
            client.close()
            continue
        threading.Thread(target=handle_client, args=(tls_client,), daemon=True).start()


def main():
    threading.Thread(target=serve_tls, daemon=True).start()
    ThreadingHTTPServer(("0.0.0.0", CONTROL_PORT), Control).serve_forever()


if __name__ == "__main__":
    main()
