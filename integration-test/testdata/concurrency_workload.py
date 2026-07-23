import logging
import os
import threading

import pyroscope


logger = logging.getLogger(__name__)
THREADS = 4
ITERATIONS = 25


def hammer(barrier, counters, counters_lock):
    barrier.wait()
    configured = 0
    shut_down = 0
    for _ in range(ITERATIONS):
        if pyroscope.configure(
            application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
            server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
            mem_enabled=True,
        ):
            configured += 1
        pyroscope.add_thread_tag("hammer", "true")
        pyroscope.remove_thread_tag("hammer", "true")
        if pyroscope.shutdown():
            shut_down += 1
    with counters_lock:
        counters["configured"] += configured
        counters["shutdown"] += shut_down


def main():
    logging.basicConfig(level=logging.INFO)

    counters = {"configured": 0, "shutdown": 0}
    counters_lock = threading.Lock()
    barrier = threading.Barrier(THREADS)
    threads = [
        threading.Thread(target=hammer, args=(barrier, counters, counters_lock))
        for _ in range(THREADS)
    ]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()

    logger.info("counters %s", counters)
    diff = counters["configured"] - counters["shutdown"]
    if diff not in (0, 1):
        raise AssertionError(
            f"inconsistent configure/shutdown accounting: {counters}"
        )

    if pyroscope.shutdown() != (diff == 1):
        raise AssertionError(
            "agent running state does not match configure/shutdown accounting"
        )

    if not pyroscope.configure(
        application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
        server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
        mem_enabled=True,
    ):
        raise AssertionError("configure failed after the concurrency storm")
    if not pyroscope.shutdown():
        raise AssertionError("shutdown failed after the concurrency storm")

    logger.info("concurrency workload done")


if __name__ == "__main__":
    main()
