#!/usr/bin/env python3
"""Internal canary. Every request is appended to a log the harness reads.

A denied target is reachable from the lab network on purpose. If a product
call reaches it, the log proves contact. The harness must not treat silence
from a host the product cannot route to as evidence.
"""

import json
import os
import ssl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = os.environ.get("DENIED_LOG", "/logs/access.log")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _record(self):
        length = int(self.headers.get("Content-Length", "0") or "0")
        if length:
            self.rfile.read(length)
        entry = {
            "method": self.command,
            "path": self.path,
            "host": self.headers.get("Host", ""),
            "client": self.client_address[0],
        }
        os.makedirs(os.path.dirname(LOG), exist_ok=True)
        with open(LOG, "a", encoding="utf-8") as handle:
            handle.write(json.dumps(entry, sort_keys=True) + "\n")
        body = b"denied-target\n"
        self.send_response(403)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self._record()

    def do_POST(self):
        self._record()

    def do_PUT(self):
        self._record()

    def do_PROPFIND(self):
        self._record()

    def do_OPTIONS(self):
        self._record()

    def do_REPORT(self):
        self._record()

    def log_message(self, fmt, *args):
        return


def serve(httpd):
    httpd.serve_forever()


def main():
    os.makedirs(os.path.dirname(LOG), exist_ok=True)
    httpd = ThreadingHTTPServer(("0.0.0.0", 8080), Handler)
    cert = os.environ.get("DENIED_CERT", "")
    key = os.environ.get("DENIED_KEY", "")
    if cert and key:
        tls = ThreadingHTTPServer(("0.0.0.0", 8443), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cert, key)
        tls.socket = context.wrap_socket(tls.socket, server_side=True)
        import threading

        threading.Thread(target=serve, args=(tls,), daemon=True).start()
    serve(httpd)


if __name__ == "__main__":
    main()
