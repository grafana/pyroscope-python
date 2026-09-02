"""Minimal ingest sink so the agent's uploads succeed instead of erroring out."""
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


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


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 4040
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
