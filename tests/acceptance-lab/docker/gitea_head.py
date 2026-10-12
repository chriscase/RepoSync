#!/usr/bin/env python3
"""TLS proxy in front of the lab Gitea API.

Gitea 1.22.6 answers HEAD /api/v1/repos/{owner}/{repo} with 405. The shipped
personal client treats that as a missing repository (issue #197). This process
forwards the API and returns the GET status for HEAD. It decodes the upstream
body before forwarding so a chunked Gitea response stays valid JSON. It does
not create repositories and it does not sync.
"""

import http.client
import os
import socket
import ssl
import sys
import threading

UPSTREAM_HOST = os.environ.get("UPSTREAM_HOST", "gitea")
UPSTREAM_PORT = int(os.environ.get("UPSTREAM_PORT", "3000"))
LISTEN_PORT = int(os.environ.get("LISTEN_PORT", "3000"))
CA = os.environ.get("UPSTREAM_CA", "/certs/ca.crt")
CERT = os.environ.get("PROXY_CERT", "/certs/gitea-api.crt")
KEY = os.environ.get("PROXY_KEY", "/certs/gitea-api.key")

HOP = {
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "upgrade",
    "te",
    "trailer",
}


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
    length = 0
    for line in head.decode("iso-8859-1", errors="replace").split("\r\n")[1:]:
        if line.lower().startswith("content-length:"):
            length = int(line.split(":", 1)[1].strip())
    body = rest
    while len(body) < length:
        chunk = sock.recv(length - len(body))
        if not chunk:
            break
        body += chunk
    return head + b"\r\n\r\n" + body[:length]


def parse_request(request):
    head, _, body = request.partition(b"\r\n\r\n")
    lines = head.decode("iso-8859-1", errors="replace").split("\r\n")
    method, path, _version = lines[0].split(" ", 2)
    headers = {}
    for line in lines[1:]:
        if not line or ":" not in line:
            continue
        name, value = line.split(":", 1)
        if name.lower() in HOP or name.lower() == "host" or name.lower() == "content-length":
            continue
        headers[name] = value.strip()
    return method, path, headers, body


def header_line(text):
    return text.encode("latin-1", errors="replace")


def build_response(status, reason, header_items, body, was_head):
    if was_head:
        body = b""
    lines = [header_line(f"HTTP/1.1 {status} {reason}")]
    for name, value in header_items:
        if name.lower() in HOP or name.lower() == "content-length":
            continue
        lines.append(header_line(f"{name}: {value}"))
    lines.append(header_line(f"Content-Length: {len(body)}"))
    lines.append(b"Connection: close")
    return b"\r\n".join(lines) + b"\r\n\r\n" + body


def forward(method, path, headers, body):
    context = ssl.create_default_context(cafile=CA)
    context.check_hostname = True
    context.verify_mode = ssl.CERT_REQUIRED
    conn = http.client.HTTPSConnection(
        UPSTREAM_HOST,
        UPSTREAM_PORT,
        context=context,
        timeout=30,
    )
    was_head = method == "HEAD"
    upstream_method = "GET" if was_head else method
    conn.request(upstream_method, path, body=body if body else None, headers=headers)
    resp = conn.getresponse()
    data = resp.read()
    items = resp.getheaders()
    status = resp.status
    reason = resp.reason
    conn.close()
    return build_response(status, reason, items, data, was_head)


def handle_client(client):
    try:
        request = read_http_message(client)
        if not request:
            client.close()
            return
        method, path, headers, body = parse_request(request)
        response = forward(method, path, headers, body)
        client.sendall(response)
        sys.stderr.write(f"gitea-api {method} {path} -> {len(response)} bytes\n")
        sys.stderr.flush()
    except Exception as exc:
        sys.stderr.write(f"gitea-api proxy error: {exc}\n")
        sys.stderr.flush()
    try:
        client.close()
    except Exception:
        pass


def main():
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


if __name__ == "__main__":
    main()
