#!/usr/bin/env python3
"""In-lab notification sink. Production Slack, Teams, and email stay unset."""

import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = os.environ.get("NOTIFY_LOG", "/logs/notify.log")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0") or "0")
        raw = self.rfile.read(length) if length else b""
        os.makedirs(os.path.dirname(LOG), exist_ok=True)
        with open(LOG, "a", encoding="utf-8") as handle:
            handle.write(json.dumps({"path": self.path, "bytes": len(raw)}) + "\n")
        body = b"ok\n"
        self.send_response(204)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_GET(self):
        body = b"notify-sink\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        return


if __name__ == "__main__":
    os.makedirs(os.path.dirname(LOG), exist_ok=True)
    ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
