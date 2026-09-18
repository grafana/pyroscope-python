"""Run against installed regular and free-threaded wheels; no backend required."""

import gzip
from http.server import BaseHTTPRequestHandler, HTTPServer
import queue
import subprocess
import sys
import sysconfig
import threading
import time
import unittest
import warnings

import pyroscope


FREE_THREADED = sysconfig.get_config_var("Py_GIL_DISABLED") == 1
WARNING = "Memory profiling is not supported on free-threaded CPython"


def cpu_workload():
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        sum(i * i for i in range(1000))


def memory_workload():
    return [bytearray(64 * 1024) for _ in range(256)]


class MemorySupportTest(unittest.TestCase):
    def setUp(self):
        self.uploads = queue.Queue()
        uploads = self.uploads

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                uploads.put(gzip.decompress(body))
                self.send_response(200)
                self.send_header("Content-Length", "0")
                self.end_headers()

            def log_message(self, *_args):
                pass

        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever)
        self.thread.start()

    def tearDown(self):
        pyroscope.shutdown()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    def configure(self, **kwargs):
        return pyroscope.configure(
            application_name="runtime-support-test",
            server_address=f"http://127.0.0.1:{self.server.server_port}",
            enable_logging=False,
            upload_interval=60,  # Shutdown flushes one complete profile.
            **kwargs,
        )

    def test_memory_only(self):
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            configured = self.configure(cpu_enabled=False, mem_enabled=True)
        if FREE_THREADED:
            self.assertFalse(configured)
            self.assertEqual(len(caught), 1)
            self.assertIs(caught[0].category, RuntimeWarning)
            self.assertIn(WARNING, str(caught[0].message))
            self.assertFalse(pyroscope.shutdown())
            self.assertTrue(self.uploads.empty())
        else:
            self.assertTrue(configured)
            self.assertEqual(caught, [])
            retained = memory_workload()
            self.assertTrue(pyroscope.shutdown())
            profile = self.uploads.get(timeout=10)
            for marker in (b"alloc_space", b"inuse_space", b"memory_workload"):
                self.assertIn(marker, profile)
            self.assertEqual(len(retained), 256)

    def test_cpu_with_memory_requested(self):
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            # GIL ownership filtering is not useful on a free-threaded runtime.
            self.assertTrue(self.configure(mem_enabled=True, gil_only=False))
        self.assertEqual(len(caught), int(FREE_THREADED))
        if FREE_THREADED:
            self.assertIs(caught[0].category, RuntimeWarning)
            self.assertIn(WARNING, str(caught[0].message))
        cpu_workload()
        self.assertTrue(pyroscope.shutdown())
        if FREE_THREADED:
            # The pinned py-spy cannot discover some free-threaded runtimes.
            # Here we check that the agent starts and memory stays disabled;
            # CPU sample delivery is checked on regular Python below.
            while not self.uploads.empty():
                self.assertNotIn(b"alloc_space", self.uploads.get_nowait())
        else:
            self.assertIn(b"cpu_workload", self.uploads.get(timeout=10))

    def test_memory_not_requested(self):
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            self.assertFalse(self.configure(cpu_enabled=False, mem_enabled=False))
            self.assertTrue(self.configure(mem_enabled=False, gil_only=False))
        self.assertEqual(caught, [])
        self.assertTrue(pyroscope.shutdown())

    @unittest.skipUnless(FREE_THREADED, "memory profiling is supported")
    def test_warning_as_error_leaves_agent_idle(self):
        with warnings.catch_warnings():
            warnings.simplefilter("error", RuntimeWarning)
            with self.assertRaisesRegex(RuntimeWarning, WARNING):
                self.configure(mem_enabled=True)
        self.assertFalse(pyroscope.shutdown())
        self.assertTrue(self.configure(mem_enabled=False, gil_only=False))
        self.assertTrue(pyroscope.shutdown())

    @unittest.skipUnless(FREE_THREADED, "exercises unsupported memory cleanup")
    def test_fork_cleanup(self):
        # Use a subprocess so a native crash or deadlock has a bounded failure.
        result = subprocess.run(
            [sys.executable, "-c", """
import os
import warnings
import pyroscope

with warnings.catch_warnings():
    warnings.simplefilter("ignore")
    assert pyroscope.configure(
        application_name="fork-runtime-support-test",
        mem_enabled=True,
        gil_only=False,
    )
    pid = os.fork()
if pid == 0:
    retained = [bytearray(64 * 1024) for _ in range(64)]
    os._exit(0)
_, status = os.waitpid(pid, 0)
assert os.waitstatus_to_exitcode(status) == 0
assert pyroscope.shutdown()
"""],
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
