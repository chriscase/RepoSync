"""Loopback-only disposable Git smart HTTP(S) transport fixture."""
import argparse
import base64
from pathlib import Path
import http.server
import json
import os
import ssl
import subprocess
import urllib.parse

parser = argparse.ArgumentParser()
parser.add_argument('root')
parser.add_argument('--cert')
parser.add_argument('--key')
parser.add_argument('--token')
parser.add_argument('--redirect')
parser.add_argument('--redirect-after-push', action='store_true')
parser.add_argument('--request-log', required=True)
args = parser.parse_args()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *_):
        pass

    def do_GET(self):
        self.backend()

    def do_POST(self):
        self.backend()

    def do_CONNECT(self):
        with open(args.request_log, 'a', encoding='utf-8') as log:
            log.write(json.dumps({'method': self.command, 'path': self.path}) + '\n')
        self.send_error(502, 'fixture proxy refuses CONNECT')
        self.close_connection = True

    def backend(self):
        with open(args.request_log, 'a', encoding='utf-8') as log:
            log.write(json.dumps({'method': self.command, 'path': self.path}) + '\n')
        if args.token:
            expected = 'Basic ' + base64.b64encode(('x-access-token:' + args.token).encode()).decode()
            if self.headers.get('Authorization') != expected:
                self.send_response(401)
                self.send_header('WWW-Authenticate', 'Basic realm="fixture"')
                self.send_header('Content-Length', '0')
                self.send_header('Connection', 'close')
                self.end_headers()
                self.close_connection = True
                return
        if args.redirect and (not args.redirect_after_push or Path(args.root, 'published').exists()):
            self.send_response(302)
            self.send_header('Location', args.redirect + self.path)
            self.send_header('Content-Length', '0')
            self.send_header('Connection', 'close')
            self.end_headers()
            self.close_connection = True
            return
        if self.headers.get('Transfer-Encoding', '').lower() == 'chunked':
            chunks = []
            while True:
                count = int(self.rfile.readline().split(b';', 1)[0], 16)
                if not count:
                    while self.rfile.readline() not in (b'\r\n', b'\n', b''):
                        pass
                    break
                chunks.append(self.rfile.read(count))
                assert self.rfile.read(2) == b'\r\n'
                assert sum(map(len, chunks)) < 1024 * 1024
            body = b''.join(chunks)
        else:
            count = int(self.headers.get('Content-Length', '0'))
            assert count < 1024 * 1024
            body = self.rfile.read(count)
        request = urllib.parse.urlsplit(self.path)
        fixture_env = os.environ.copy()
        fixture_env.update({
            'GIT_PROJECT_ROOT': args.root,
            'GIT_HTTP_EXPORT_ALL': '1',
            'PATH_INFO': urllib.parse.unquote(request.path),
            'REQUEST_METHOD': self.command,
            'QUERY_STRING': request.query,
            'CONTENT_TYPE': self.headers.get('Content-Type', ''),
            'CONTENT_LENGTH': str(len(body)),
            'REMOTE_ADDR': '127.0.0.1',
            'REMOTE_USER': 'fixture',
        })
        result = subprocess.run(['git', 'http-backend'], input=body,
                                capture_output=True, env=fixture_env, timeout=5)
        assert result.returncode == 0, result.stderr.decode(errors='replace')
        if self.command == 'POST' and request.path.endswith('/git-receive-pack'):
            Path(args.root, 'published').touch()
        headers, content = result.stdout.split(b'\r\n\r\n', 1)
        values = []
        status = 200
        for header in headers.decode('ascii').split('\r\n'):
            name, value = header.split(':', 1)
            if name.lower() == 'status':
                status = int(value.strip().split()[0])
            else:
                values.append((name, value.strip()))
        self.send_response(status)
        for name, value in values:
            self.send_header(name, value)
        self.send_header('Content-Length', str(len(content)))
        self.send_header('Connection', 'close')
        self.end_headers()
        self.wfile.write(content)
        self.close_connection = True


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
if args.cert:
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(args.cert, args.key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
scheme = 'https' if args.cert else 'http'
print(json.dumps({'url': f'{scheme}://localhost:{server.server_port}/target.git'}), flush=True)
server.serve_forever()
