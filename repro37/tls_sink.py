"""HTTPS ingest sink with a self-signed cert, so the agent's upload path goes
through native-tls/OpenSSL the way it does against a real (https) Pyroscope
endpoint, instead of plain HTTP."""
import os
import ssl
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

CERT = "/tmp/repro37-cert.pem"
KEY = "/tmp/repro37-key.pem"


def ensure_cert(host):
    if os.path.exists(CERT) and os.path.exists(KEY):
        return
    subprocess.run(
        ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
         "-keyout", KEY, "-out", CERT, "-days", "30",
         "-subj", f"/CN={host}",
         "-addext", f"subjectAltName=DNS:{host},IP:127.0.0.1"],
        check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        while length > 0:
            length -= len(self.rfile.read(min(length, 65536)))
        self.send_response(200)
        self.send_header("content-length", "2")
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, *args):
        pass


def _noop():
    pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 4443
    host = sys.argv[2] if len(sys.argv) > 2 else "localhost"
    ensure_cert(host)
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(CERT, KEY)
    srv = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    srv.socket = ctx.wrap_socket(srv.socket, server_side=True)
    srv.serve_forever()
