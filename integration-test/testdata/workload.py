import hashlib
import logging
import os
import signal
import sys
import threading
import time

import pyroscope

logger = logging.getLogger(__name__)
shutdown_requested = threading.Event()
stop_workers = threading.Event()
# Stack's adaptive wall sampler can poll only once per 100ms. Keep its work
# phases much longer than that: CPU accrued between polls is charged to the
# stack at the next poll, which may already be sleeping at a phase boundary.
# Preserve the 1:2 work/sleep ratio and the same strict profile assertions.
# Polling attribution limitation: https://github.com/grafana/pyroscope-python/issues/276
hash_seconds = 2.0 if os.environ.get("CPU_IMPLEMENTATION") == "Stack" else 0.1
sleep_seconds = 2 * hash_seconds


def env_bool(name):
    value = os.environ[name].strip().lower()
    if value in ("1", "true", "yes", "on"):
        return True
    if value in ("0", "false", "no", "off"):
        return False
    raise ValueError("invalid boolean value for {}: {}".format(name, value))


def hash_value(string):
    # Keep nearly all active Python time in this leaf, so profiles have a
    # predictable shape even when interpreter speed or instrumentation changes.
    deadline = time.monotonic() + hash_seconds
    while time.monotonic() < deadline:
        string = hashlib.sha256(string.encode()).hexdigest()
    return string


def multihash(string):
    while not stop_workers.is_set():
        time.sleep(sleep_seconds)
        string = hash_value(string)
    return string


def multihash2(string):
    while not stop_workers.is_set():
        time.sleep(sleep_seconds)
        string = hash_value(string)
    return string


def request_shutdown(signum, _frame):
    logger.info("received signal %s, shutting down", signum)
    shutdown_requested.set()


def main():
    logging.basicConfig(level=logging.INFO)
    signal.signal(signal.SIGINT, request_shutdown)
    signal.signal(signal.SIGTERM, request_shutdown)

    oncpu = env_bool("ONCPU")
    gil_only = env_bool("GIL_ONLY")
    cpu_implementation = os.environ.get("CPU_IMPLEMENTATION", "PySpy")
    signal_timer = os.environ.get("SIGNAL_TIMER") or "ITimer"
    canary = os.environ["CANARY"]
    application_name = os.environ["PYROSCOPE_APPLICATION_NAME"]
    server_address = os.environ["PYROSCOPE_SERVER_ADDRESS"]

    logger.info(
        "starting workload application_name=%s server_address=%s oncpu=%s gil_only=%s cpu_implementation=%s canary=%s",
        application_name,
        server_address,
        oncpu,
        gil_only,
        cpu_implementation,
        canary,
    )
    pyroscope.configure(
        application_name=application_name,
        server_address=server_address,
        enable_logging=True,
        oncpu=oncpu,
        gil_only=gil_only,
        cpu_implementation=getattr(
            pyroscope.ProfilerImplementation, cpu_implementation
        ),
        **(
            {"signal_timer": getattr(pyroscope.SignalTimer, signal_timer)}
            if os.environ.get("CPU_IMPLEMENTATION") == "Signal"
            else {}
        ),
        report_pid=True,
        report_thread_id=True,
        report_thread_name=True,
        upload_interval=1,
        tags={
            "oncpu": str(oncpu).lower(),
            "gil_only": str(gil_only).lower(),
            "canary": canary,
        },
    )

    threads = [
        threading.Thread(target=multihash, args=("abc",)),
        threading.Thread(target=multihash2, args=("abc",)),
    ]
    for thread in threads:
        thread.start()

    try:
        while not shutdown_requested.wait(1):
            pass
    finally:
        pyroscope.shutdown()
        stop_workers.set()
        for thread in threads:
            thread.join()
        logger.info("workload stopped")


if __name__ == "__main__":
    try:
        main()
    except Exception:
        logger.exception("workload failed")
        sys.exit(1)
