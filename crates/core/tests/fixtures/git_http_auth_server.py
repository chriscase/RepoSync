#!/usr/bin/env python3
"""Minimal smart-HTTP Git server: 401 unless Authorization matches GIT_TEST_TOKEN."""

import base64
import os
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer


def expected_auth_header() -> str:
    token = os.environ["GIT_TEST_TOKEN"]
    basic = base64.b64encode(f"x-access-token:{token}".encode()).decode()
    return f"Basic {basic}"


class GitHttpBackendHandler(BaseHTTPRequestHandler):
    def log_message(self, _format, *_args):
        pass

    def _authorized(self) -> bool:
        auth = self.headers.get("Authorization", "")
        return auth == expected_auth_header()

    def _run_backend(self):
        if not self._authorized():
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Basic realm="git"')
            self.end_headers()
            return

        length = int(self.headers.get("Content-Length", "0") or "0")
        body = self.rfile.read(length) if length else b""

        path = self.path
        if "?" in path:
            path_info, query_string = path.split("?", 1)
        else:
            path_info, query_string = path, ""

        env = os.environ.copy()
        env.update(
            {
                "GIT_PROJECT_ROOT": os.environ["GIT_PROJECT_ROOT"],
                "GIT_HTTP_EXPORT_ALL": "1",
                "REQUEST_METHOD": self.command,
                "PATH_INFO": path_info,
                "QUERY_STRING": query_string,
                "CONTENT_TYPE": self.headers.get("Content-Type", ""),
                "CONTENT_LENGTH": str(len(body)),
            }
        )

        result = subprocess.run(
            ["git", "http-backend"],
            input=body if body else None,
            capture_output=True,
            env=env,
        )
        if result.returncode != 0:
            self.send_response(500)
            self.end_headers()
            self.wfile.write(result.stderr or b"git http-backend failed")
            return

        raw = result.stdout
        if b"\r\n\r\n" not in raw:
            self.send_response(500)
            self.end_headers()
            self.wfile.write(b"invalid CGI response")
            return

        header_block, payload = raw.split(b"\r\n\r\n", 1)
        status = 200
        extra_headers: list[tuple[str, str]] = []
        for line in header_block.decode("latin-1").split("\r\n"):
            if not line:
                continue
            lower = line.lower()
            if lower.startswith("status:"):
                parts = line.split()
                if len(parts) >= 2:
                    status = int(parts[1])
            elif ":" in line:
                name, value = line.split(":", 1)
                extra_headers.append((name.strip(), value.strip()))

        self.send_response(status)
        for name, value in extra_headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        self._run_backend()

    def do_POST(self):
        self._run_backend()


def main() -> int:
    host = os.environ.get("GIT_HTTP_HOST", "127.0.0.1")
    port = int(os.environ["GIT_HTTP_PORT"])
    root = os.environ["GIT_PROJECT_ROOT"]
    os.makedirs(root, exist_ok=True)
    server = HTTPServer((host, port), GitHttpBackendHandler)
    print(f"listening on {host}:{port} root={root}", flush=True)
    server.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
