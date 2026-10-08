import logging
import os
import signal
import sys
import threading
import time

import pyroscope


logger = logging.getLogger(__name__)
shutdown_requested = threading.Event()

PHASE_SECONDS = 10


def burn_first(stop):
    x = 0
    while not stop.is_set():
        x = (x * 31 + 7) % 1000003
    return x


def burn_second(stop):
    x = 0
    while not stop.is_set():
        x = (x * 31 + 7) % 1000003
    return x


def request_shutdown(signum, _frame):
    logger.info("received signal %s, exiting", signum)
    shutdown_requested.set()


def run_phase(canary, burn):
    if not pyroscope.configure(
        application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
        server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
        enable_logging=True,
        cpu_enabled=True,
        cpu_implementation=pyroscope.ProfilerImplementation.Stack,
        mem_enabled=False,
        upload_interval=1,
        tags={"canary": canary},
    ):
        raise AssertionError(f"configure() returned False for canary {canary}")

    stop = threading.Event()
    burner = threading.Thread(target=burn, args=(stop,), name=burn.__name__)
    burner.start()
    try:
        time.sleep(PHASE_SECONDS)
    finally:
        stop.set()
        burner.join()

    if not pyroscope.shutdown():
        raise AssertionError(f"shutdown() returned False for canary {canary}")


def main():
    logging.basicConfig(level=logging.INFO)
    signal.signal(signal.SIGINT, request_shutdown)
    signal.signal(signal.SIGTERM, request_shutdown)

    run_phase(os.environ["CANARY_FIRST"], burn_first)
    run_phase(os.environ["CANARY_SECOND"], burn_second)

    # Both phases are uploaded by now, but the harness polls until its data is
    # queryable and treats an exit as a failed assertion, so idle until stopped.
    logger.info("both phases done, waiting for shutdown")
    while not shutdown_requested.wait(1):
        pass
    logger.info("restart workload stopped")


if __name__ == "__main__":
    try:
        main()
    except Exception:
        logger.exception("restart workload failed")
        sys.exit(1)
