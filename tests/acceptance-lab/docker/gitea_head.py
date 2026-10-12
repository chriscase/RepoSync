#!/usr/bin/env python3
"""TLS proxy that answers Gitea HEAD requests with the GET status.

Gitea 1.22.6 returns 405 for HEAD /api/v1/repos/{owner}/{repo}. The shipped
personal client treats that as "repository does not exist". This process
forwards the API unchanged except for that method. It does not create
repositories and it does not sync.
"""

import os
import socket
import ssl
import threading

UPSTREAM_HOST = os.environ.get("UPSTREAM_HOST", "gitea")
UPSTREAM_PORT = int(os.environ.get("UPSTREAM_PORT", "3000"))
LISTEN_PORT = int(os.environ.get("LISTEN_PORT", "3000"))
CA = os.environ.get("UPSTREAM_CA", "/certs/ca.crt")
CERT = os.environ.get("PROXY_CERT", "/certs/gitea-api.crt")
KEY = os.environ.get("PROXY_KEY", "/certs/gitea-api.key")


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


def head_status(response):
    head = response.split(b"\r\n\r\n", 1)[0]
    lines = head.split(b"\r\n")
    kept = [lines[0]]
    for line in lines[1:]:
        lower = line.lower()
        if lower.startswith(b"content-length:") or lower.startswith(b"transfer-encoding:"):
            continue
        kept.append(line)
    kept.append(b"Content-Length: 0")
    return b"\r\n".join(kept) + b"\r\n\r\n"


def prepare(request):
    head, sep, body = request.partition(b"\r\n\r\n")
    lines = head.split(b"\r\n")
    was_head = bool(lines) and lines[0].startswith(b"HEAD ")
    if was_head:
        lines[0] = b"GET" + lines[0][4:]
    forwarded = []
    for line in lines:
        if line.lower().startswith(b"host:"):
            forwarded.append(b"Host: %s:%s" % (UPSTREAM_HOST.encode(), str(UPSTREAM_PORT).encode()))
        else:
            forwarded.append(line)
    return b"\r\n".join(forwarded) + sep + body, was_head


def handle_client(client):
    try:
        request = read_http_message(client)
        if not request:
            client.close()
            return
        forwarded, was_head = prepare(request)
        context = ssl.create_default_context(cafile=CA)
        context.check_hostname = True
        context.verify_mode = ssl.CERT_REQUIRED
        raw = socket.create_connection((UPSTREAM_HOST, UPSTREAM_PORT), timeout=30)
        upstream = context.wrap_socket(raw, server_hostname=UPSTREAM_HOST)
        upstream.sendall(forwarded)
        response = read_http_message(upstream)
        upstream.close()
        if was_head:
            response = head_status(response)
        client.sendall(response)
    except Exception:
        pass
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
