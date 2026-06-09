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


def env_bool(name):
    value = os.environ[name].strip().lower()
    if value in ("1", "true", "yes", "on"):
        return True
    if value in ("0", "false", "no", "off"):
        return False
    raise ValueError("invalid boolean value for {}: {}".format(name, value))


def hash_value(string):
    return hashlib.sha256(string.encode()).hexdigest()


def multihash(string):
    while not stop_workers.is_set():
        time.sleep(0.2)
        end = time.time() + 0.1
        while time.time() < end:
            string = hash_value(string)
    return string


def multihash2(string):
    while not stop_workers.is_set():
        time.sleep(0.2)
        end = time.time() + 0.1
        while time.time() < end:
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
    canary = os.environ["CANARY"]
    application_name = os.environ["PYROSCOPE_APPLICATION_NAME"]
    server_address = os.environ["PYROSCOPE_SERVER_ADDRESS"]

    logger.info(
        "starting workload application_name=%s server_address=%s oncpu=%s gil_only=%s canary=%s",
        application_name,
        server_address,
        oncpu,
        gil_only,
        canary,
    )
    pyroscope.configure(
        application_name=application_name,
        server_address=server_address,
        enable_logging=True,
        oncpu=oncpu,
        gil_only=gil_only,
        report_pid=True,
        report_thread_id=True,
        report_thread_name=True,
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
