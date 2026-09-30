import logging
import os
import threading
import time
import traceback
import uuid

from urllib.parse import quote

try:
    from urllib.request import Request, urlopen
except ImportError:
    from urllib2 import Request, urlopen

import pyroscope


app_name = 'pyroscopers.python.test.truncated'
logger = logging.getLogger()

event = threading.Event()

# Deeper than the max_nframe of 4 configured below, so every sample of this
# thread is truncated for both profilers.
recursion_depth = 32


def burn_and_hog():
    acc = 0
    retained = []
    while not event.is_set():
        for i in range(50000):
            acc = (acc + i * i) % 4294967291
        retained.append(bytearray(64 * 1024))
        if len(retained) >= 256:
            del retained[:128]
    return acc


def recurse(depth):
    if depth == 0:
        return burn_and_hog()
    return recurse(depth - 1)


def wait_render(profile_type, canary, *needles):
    while True:
        time.sleep(2)
        query = f'{profile_type}{{service_name="{app_name}", canary="{canary}"}}'
        u = 'http://localhost:4040/pyroscope/render?from=now-1h&until=now&query=' + quote(query)
        response = None
        try:
            logging.info('render %s', u)
            req = Request(u)
            response = urlopen(req)
            code = response.getcode()
            body = response.read()
            logging.info('render body %s', body.decode('utf-8'))
            if code == 200 and body != b'' and all(needle in body for needle in needles):
                print(f'good {profile_type} {canary}')
                return
        except Exception:
            if response is not None:
                response.close()
            traceback.print_exc()
            continue


def main():
    logger.setLevel(logging.INFO)
    canary = uuid.uuid4().hex
    logging.info('canary %s', canary)

    pyroscope.configure_cpu_profiler(max_nframe=4)
    pyroscope.configure(
        application_name=app_name,
        server_address='http://localhost:4040',
        enable_logging=True,
        cpu_enabled=True,
        cpu_implementation=pyroscope.ProfilerImplementation.Stack,
        mem_enabled=True,
        mem_max_nframe=4,
        tags={
            'canary': canary,
        },
    )

    thread = threading.Thread(target=recurse, args=(recursion_depth,))
    thread.start()

    def watchdog():
        logging.info('Watchdog expired. Test timeout. Exiting...')
        os._exit(7)

    alarm = threading.Timer(180, watchdog)
    alarm.start()

    # The leaf must survive alongside the marker: a marker without it would
    # mean the stack was lost, not truncated.
    wait_render('process_cpu:cpu:nanoseconds:cpu:nanoseconds', canary, b'burn_and_hog', b'truncated')
    wait_render('process_cpu:wall:nanoseconds:cpu:nanoseconds', canary, b'burn_and_hog', b'truncated')
    wait_render('memory:alloc_space:bytes:space:bytes', canary, b'burn_and_hog', b'truncated')

    alarm.cancel()

    pyroscope.shutdown()

    event.set()
    thread.join()
    logging.info('done')


if __name__ == '__main__':
    main()
