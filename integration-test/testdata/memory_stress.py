"""Memory profiler stress workload.

Exercises the paths no unit test can reach, in the container images the wheels
actually ship for: realloc(ptr, 0) via bytearray shrink, many threads
allocating at once, fork with the profiler running, and repeated
configure/shutdown cycles under continuous allocation (which is the
in-flight-free-after-stop path).

Keeps a steady live set so the collector sees a plausible inuse_space, then
reports the stress results on stdout for the Go test to assert on.
"""

import gc
import logging
import os
import signal
import sys
import threading
import time

import pyroscope


logger = logging.getLogger(__name__)
shutdown_requested = threading.Event()

CHUNK = 64 * 1024
RETAINED_CHUNKS = 128


def configure():
    pyroscope.configure(
        application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
        server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
        enable_logging=True,
        cpu_enabled=False,
        mem_enabled=True,
        tags={"canary": os.environ["CANARY"]},
    )


def shrink_to_zero(rounds):
    """bytearray shrink hits realloc(ptr, 0), which glibc turns into a free."""
    for _ in range(rounds):
        buf = bytearray(4096)
        del buf[:]
        buf = bytearray(CHUNK)
        del buf[:]


def memhog():
    """Hold a steady live set while churning, so inuse_space stays meaningful."""
    retained = [bytearray(CHUNK) for _ in range(RETAINED_CHUNKS)]
    churn = []
    while not shutdown_requested.is_set():
        churn.append(bytearray(CHUNK))
        if len(churn) > 8:
            del churn[:4]
        shrink_to_zero(2)
        time.sleep(0.005)
    return retained


def stress_threads():
    stop = threading.Event()

    def worker():
        keep = []
        while not stop.is_set():
            keep.append(bytearray(8192))
            if len(keep) > 64:
                del keep[:32]
            shrink_to_zero(2)

    threads = [threading.Thread(target=worker) for _ in range(8)]
    for thread in threads:
        thread.start()
    time.sleep(2)
    stop.set()
    for thread in threads:
        thread.join()


def stress_restart_cycles(cycles=50):
    """configure/shutdown repeatedly while another thread keeps allocating."""
    stop = threading.Event()

    def noise():
        keep = []
        while not stop.is_set():
            keep.append(bytearray(8192))
            if len(keep) > 32:
                del keep[:16]

    thread = threading.Thread(target=noise)
    thread.start()
    try:
        for _ in range(cycles):
            configure()
            shrink_to_zero(20)
            pyroscope.shutdown()
    finally:
        stop.set()
        thread.join()


def stress_fork(forks=3):
    """fork() with the profiler running; the child must not inherit its state."""
    failures = 0
    for index in range(forks):
        pid = os.fork()
        if pid == 0:
            try:
                shrink_to_zero(200)
                keep = [bytearray(8192) for _ in range(50)]
                del keep
                gc.collect()
                os._exit(0)
            except BaseException:
                os._exit(9)
        _, status = os.waitpid(pid, 0)
        code = os.waitstatus_to_exitcode(status)
        if code != 0:
            logger.error("forked child %s exited with %s", index, code)
            failures += 1
    return failures


def request_shutdown(signum, _frame):
    logger.info("received signal %s, shutting down", signum)
    shutdown_requested.set()


def main():
    logging.basicConfig(level=logging.INFO)
    signal.signal(signal.SIGINT, request_shutdown)
    signal.signal(signal.SIGTERM, request_shutdown)

    failures = []

    # Run the destructive stress first, while nothing is being collected, so a
    # crash here is unambiguous.
    try:
        stress_restart_cycles()
        print("STRESS restart-cycles ok", flush=True)
    except BaseException as exc:
        failures.append(f"restart-cycles: {exc!r}")

    configure()
    try:
        stress_threads()
        print("STRESS threads ok", flush=True)
    except BaseException as exc:
        failures.append(f"threads: {exc!r}")

    try:
        fork_failures = stress_fork()
        if fork_failures:
            failures.append(f"fork: {fork_failures} children failed")
        else:
            print("STRESS fork ok", flush=True)
    except BaseException as exc:
        failures.append(f"fork: {exc!r}")

    if failures:
        for failure in failures:
            print(f"STRESS FAILED {failure}", file=sys.stderr, flush=True)
        pyroscope.shutdown()
        sys.exit(1)

    print("STRESS all ok", flush=True)

    # Then hold a steady live set so the collector has something to serve.
    thread = threading.Thread(target=memhog)
    thread.start()
    try:
        while not shutdown_requested.wait(1):
            pass
    finally:
        pyroscope.shutdown()
        shutdown_requested.set()
        thread.join()
        logger.info("memory stress workload stopped")


if __name__ == "__main__":
    main()
