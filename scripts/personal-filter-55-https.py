#!/usr/bin/env python3
"""Opt-in loopback Apache DAV qualification. No service install or global trust."""
import argparse
import base64
from contextlib import contextmanager
import grp
import hashlib
import importlib.util
import os
from pathlib import Path
import pwd
import resource
import signal
import socket
import subprocess
import sys
import tempfile
import time

spec = importlib.util.spec_from_file_location("personal_fixture", Path(__file__).with_name("personal-filter-55.py"))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def checked(*argv):
    result = subprocess.run(list(map(str, argv)), text=True, capture_output=True, timeout=10)
    if result.returncode:
        raise RuntimeError(f"disposable fixture setup failed: {Path(argv[0]).name} exited {result.returncode}")


def make_ca(root, stem):
    ca, key = root / f"{stem}.pem", root / f"{stem}.key"
    checked("openssl", "req", "-x509", "-newkey", "rsa:2048", "-sha256", "-noenc", "-days", "1",
            "-subj", f"/CN=Disposable SVN fixture {stem}",
            "-addext", "basicConstraints=critical,CA:TRUE",
            "-addext", "keyUsage=critical,keyCertSign,cRLSign", "-keyout", key, "-out", ca)
    return ca, key


@contextmanager
def loopback_https(root, apache_bin, modules, mode, hidden):
    ca, cakey = make_ca(root, "trust")
    alternate, _ = make_ca(root, "untrusted")
    leaf, leafkey, csr = root / "leaf.pem", root / "leaf.key", root / "leaf.csr"
    ext = root / "leaf.conf"
    ext.write_text("subjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\n"
                   "keyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n")
    checked("openssl", "req", "-new", "-newkey", "rsa:2048", "-sha256", "-noenc",
            "-subj", "/CN=127.0.0.1", "-out", csr, "-keyout", leafkey)
    checked("openssl", "x509", "-req", "-in", csr, "-CA", ca, "-CAkey", cakey, "-CAcreateserial",
            "-sha256", "-days", "1", "-extfile", ext, "-out", leaf)
    checked("openssl", "verify", "-CAfile", ca, "-verify_ip", "127.0.0.1", leaf)
    passwd, authz = root / "htpasswd", root / "authz"
    encoded = base64.b64encode(hashlib.sha1(b"fixture").digest()).decode()
    passwd.write_text(f"fixture:{{SHA}}{encoded}\n")
    authz.write_text("[/]\nfixture = r\n" + ("[/foo/denied]\nfixture =\n" if hidden else ""))
    checked("svnauthz", "validate", authz)
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    server = root / "apache"
    server.mkdir(mode=0o700)
    config = server / "httpd.conf"
    docroot = server / "empty"
    docroot.mkdir()
    names = ("mpm_event", "authn_core", "authn_file", "authz_core", "authz_user",
             "auth_basic", "dav", "dav_svn", "authz_svn", "ssl")
    loads = "\n".join(f'LoadModule {name}_module "{modules}/mod_{name}.so"' for name in names)
    config.write_text(f"""
ServerRoot "{server}"
DefaultRuntimeDir "{server}"
PidFile "{server}/httpd.pid"
ServerName 127.0.0.1
Listen 127.0.0.1:{port}
{loads}
User {pwd.getpwuid(os.getuid()).pw_name}
Group {grp.getgrgid(os.getgid()).gr_name}
DocumentRoot "{docroot}"
ErrorLog "{server}/errors.log"
LogLevel warn
<VirtualHost 127.0.0.1:{port}>
  SSLEngine On
  SSLProtocol -all +TLSv1.2 +TLSv1.3
  SSLCertificateFile "{leaf}"
  SSLCertificateKeyFile "{leafkey}"
  <Location /svn>
    DAV svn
    SVNPath "{root}/svn"
    SVNPathAuthz On
    SVNAllowBulkUpdates {mode}
    AuthType Basic
    AuthName "Disposable SVN fixture"
    AuthUserFile "{passwd}"
    AuthzSVNAccessFile "{authz}"
    Require valid-user
  </Location>
</VirtualHost>
""")
    checked(apache_bin, "-f", config, "-t")
    with (server / "startup.log").open("w") as diagnostic:
        process = subprocess.Popen([apache_bin, "-f", config, "-D", "FOREGROUND"],
                                   start_new_session=True, stdout=subprocess.DEVNULL, stderr=diagnostic)
        try:
            for _ in range(80):
                if process.poll() is not None:
                    raise RuntimeError("disposable Apache exited during startup")
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                        break
                except OSError:
                    time.sleep(0.05)
            else:
                raise RuntimeError("disposable Apache did not start within four seconds")
            yield f"https://127.0.0.1:{port}/svn", (ca, alternate)
        finally:
            if process.poll() is None:
                assert os.getpgid(process.pid) == process.pid
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)


def abort(*_):
    raise RuntimeError("disposable HTTPS fixture stopped")


if __name__ == "__main__":
    assert os.getuid() != 0, "qualification must run without root"
    parser = argparse.ArgumentParser()
    for flag in ("personal", "cli", "apache-bin", "apache-modules"):
        parser.add_argument("--" + flag, required=True, type=Path)
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, abort)
    resource.setrlimit(resource.RLIMIT_FSIZE, (32 * 1024**2, 32 * 1024**2))
    with tempfile.TemporaryDirectory(prefix="reposync-dav-55-") as directory:
        base = Path(directory)
        for mode, hidden in [("Prefer", None), ("Prefer", "file"), ("Off", None), ("Off", "directory")]:
            root = base / f"{mode}-{hidden or 'full'}"
            root.mkdir(mode=0o700)
            with loopback_https(root, args.apache_bin.resolve(), args.apache_modules.resolve(), mode, hidden) as (url, tls):
                fixture.exercise(root, args.personal.resolve(), args.cli.resolve(), url, hidden, tls=tls)
    print("PASS actual Personal HTTPS CLI: Apache DAV bulk Prefer and Off, trust refusal and bound-CA replacement, verified complete visibility, no Git effects from denied or filtered revisions.")
