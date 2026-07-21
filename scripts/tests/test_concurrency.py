import logging
import os
import threading

import pyroscope


app_name = 'pyroscopers.python.test.concurrency'
logger = logging.getLogger()

THREADS = 4
ITERATIONS = 25


def hammer(barrier, counters, counters_lock):
    barrier.wait()
    configured = 0
    shut_down = 0
    for _ in range(ITERATIONS):
        # mem_enabled makes shutdown join a thread that needs to attach to
        # Python for the final memory flush, which is the deadlock-prone path.
        if pyroscope.configure(
            application_name=app_name,
            server_address='http://localhost:4040',
            mem_enabled=True,
        ):
            configured += 1
        pyroscope.add_thread_tag('hammer', 'true')
        pyroscope.remove_thread_tag('hammer', 'true')
        if pyroscope.shutdown():
            shut_down += 1
    with counters_lock:
        counters['configured'] += configured
        counters['shutdown'] += shut_down


def main():
    logger.setLevel(logging.INFO)

    def watchdog():
        logging.error('Watchdog expired: concurrent configure/shutdown hung. Exiting...')
        os._exit(7)

    alarm = threading.Timer(120, watchdog)
    alarm.daemon = True
    alarm.start()

    counters = {'configured': 0, 'shutdown': 0}
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

    logging.info('counters %s', counters)

    # Every successful shutdown stops exactly one successfully configured
    # agent, so at most one successful configure (an agent left running) can
    # be unmatched.
    diff = counters['configured'] - counters['shutdown']
    if diff not in (0, 1):
        raise AssertionError(
            f'inconsistent configure/shutdown accounting: {counters}'
        )

    if pyroscope.shutdown() != (diff == 1):
        raise AssertionError(
            'agent running state does not match configure/shutdown accounting'
        )

    # The profiler must remain fully usable after the storm.
    if not pyroscope.configure(
        application_name=app_name,
        server_address='http://localhost:4040',
        mem_enabled=True,
    ):
        raise AssertionError('configure failed after the concurrency storm')
    if not pyroscope.shutdown():
        raise AssertionError('shutdown failed after the concurrency storm')

    alarm.cancel()
    logging.info('done')


if __name__ == '__main__':
    main()
