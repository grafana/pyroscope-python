import hashlib
import http.server
import sys
import threading
import time

import pyroscope


class PushHandler(http.server.BaseHTTPRequestHandler):
    requests = 0

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        self.rfile.read(length)
        type(self).requests += 1
        self.send_response(200)
        self.end_headers()

    def log_message(self, _format, *_args):
        pass


def burn_cpu():
    value = b"gcp-profiler-smoke"
    deadline = time.monotonic() + 2.5
    while time.monotonic() < deadline:
        value = hashlib.sha256(value).digest()
    return value


def assert_configuration_rejected(**kwargs):
    try:
        pyroscope.configure(application_name="invalid", **kwargs)
    except ValueError:
        return
    raise AssertionError("configuration should have been rejected: {!r}".format(kwargs))


def main():
    assert_configuration_rejected(cpu_profiler="unknown")
    if sys.version_info[:2] > (3, 11):
        assert_configuration_rejected(cpu_profiler="gcp")
        return

    assert_configuration_rejected(cpu_profiler="gcp", oncpu=False)
    assert_configuration_rejected(cpu_profiler="gcp", sample_rate=0)

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), PushHandler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()

    configured = pyroscope.configure(
        application_name="gcp.profiler.smoke",
        server_address="http://127.0.0.1:{}".format(server.server_port),
        cpu_profiler="gcp",
        mem_enabled=True,
        upload_interval=1,
    )
    assert configured
    burn_cpu()
    assert pyroscope.shutdown()

    server.shutdown()
    server_thread.join()
    assert PushHandler.requests > 0


if __name__ == "__main__":
    main()
