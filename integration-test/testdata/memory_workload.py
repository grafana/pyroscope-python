import logging
import os
import signal
import threading
import time

import pyroscope


logger = logging.getLogger(__name__)
shutdown_requested = threading.Event()


def memhog():
    retained = []
    while not shutdown_requested.is_set():
        retained.append(bytearray(64 * 1024))
        if len(retained) >= 256:
            del retained[:128]
        time.sleep(0.005)
    return retained


def request_shutdown(signum, _frame):
    logger.info("received signal %s, shutting down", signum)
    shutdown_requested.set()


def main():
    logging.basicConfig(level=logging.INFO)
    signal.signal(signal.SIGINT, request_shutdown)
    signal.signal(signal.SIGTERM, request_shutdown)

    pyroscope.configure(
        application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
        server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
        enable_logging=True,
        cpu_enabled=False,
        mem_enabled=True,
        tags={
            "canary": os.environ["CANARY"],
        },
    )

    thread = threading.Thread(target=memhog)
    thread.start()
    try:
        while not shutdown_requested.wait(1):
            pass
    finally:
        pyroscope.shutdown()
        shutdown_requested.set()
        thread.join()
        logger.info("memory workload stopped")


if __name__ == "__main__":
    main()
