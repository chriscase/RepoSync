#!/usr/bin/env python3
"""Loopback GitHub-shaped HEAD/PR endpoints beside the real Git HTTP backend."""
import os
from pathlib import Path
import sys
from http.server import HTTPServer

sys.path.insert(0, str(Path(sys.argv[1]).resolve()))
from git_http_auth_server import GitHttpBackendHandler


class Provider(GitHttpBackendHandler):
    def do_HEAD(self):
        if self.path != "/api/repos/fixture/repo":
            self.send_response(404)
        elif self.headers.get("Authorization") != f"Bearer {os.environ['GIT_TEST_TOKEN']}":
            self.send_response(401)
        else:
            self.send_response(200)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_GET(self):
        if self.path.startswith("/api/"):
            payload = b"[]"
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        else:
            super().do_GET()


server = HTTPServer(("127.0.0.1", 0), Provider)
print(server.server_address[1], flush=True)
server.serve_forever()
