"""Fork behaviour for cpu_profiler=CpuProfiler.Ddtrace.

Intentionally not run in CI alongside the other suites; fork tests can deadlock
on unrelated library state. Run manually:

    python scripts/tests/test_ddtrace_cpu_fork.py

The thing being checked is that a forked child does NOT keep sampling. Upstream
dd-trace-py restarts the sampler from its pthread_atfork child handler; that is
patched out (see cpp/cpu/ddtrace_stack/VENDOR.md, "do not restart the sampler in
the fork child") because Pyroscope leaks the agent in the child, so a restarted
sampler would push into a sink nothing ever drains.

The child proves this by burning CPU and then configuring a fresh agent of its
own, which only succeeds if the inherited state was properly torn down.
"""

import hashlib
import os
import sys
import threading
import time

import pyroscope

APP_NAME = "pyroscope.ddtrace-cpu-fork-test"


def burn(seconds):
    value = "seed"
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        value = hashlib.sha256(value.encode()).hexdigest()
    return value


def child_body():
    """Runs in the forked child. Returns an exit code."""
    # If the sampler had been restarted here it would be walking threads and
    # pushing samples while we do this.
    burn(1.0)

    # A child that inherited a half-torn-down agent cannot start a new one.
    if not pyroscope.configure(
        application_name=APP_NAME + ".child",
        server_address="http://127.0.0.1:4040",
        cpu_profiler=pyroscope.CpuProfiler.Ddtrace,
        mem_enabled=False,
    ):
        return 2
    burn(1.0)
    if not pyroscope.shutdown():
        return 3
    return 0


def fork_and_check():
    pid = os.fork()
    if pid == 0:
        code = 1
        try:
            code = child_body()
        except BaseException:
            code = 4
        finally:
            os._exit(code)

    _, status = os.waitpid(pid, 0)
    code = os.waitstatus_to_exitcode(status)
    if code != 0:
        raise AssertionError(
            "forked child exited with %d; the ddtrace sampler probably survived "
            "the fork or left state the child could not recover from" % code
        )


def main():
    if sys.platform != "linux" or sys.version_info < (3, 11):
        print("skipped: cpu_profiler=Ddtrace requires Linux and CPython 3.11+")
        return

    if not pyroscope.configure(
        application_name=APP_NAME,
        server_address="http://127.0.0.1:4040",
        cpu_profiler=pyroscope.CpuProfiler.Ddtrace,
        upload_interval=1,
        mem_enabled=False,
    ):
        raise AssertionError("failed to start the agent with cpu_profiler=Ddtrace")

    try:
        # Fork while several threads are busy, so the child inherits a
        # thread_info_map full of threads that do not exist in it.
        stop = threading.Event()
        threads = [
            threading.Thread(target=lambda: [burn(0.1) for _ in iter(lambda: not stop.is_set(), False)])
            for _ in range(3)
        ]
        for t in threads:
            t.start()
        time.sleep(1.0)

        fork_and_check()

        stop.set()
        for t in threads:
            t.join()

        # The parent must still be profiling: its sampling thread survives the
        # fork untouched (Sampler::prefork does not stop it).
        burn(1.0)
    finally:
        pyroscope.shutdown()

    print("ok")


if __name__ == "__main__":
    main()
