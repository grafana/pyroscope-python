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


app_name = 'pyroscopers.python.test.memory'
logger = logging.getLogger()

event = threading.Event()


def memhog():
    # Allocate 64 KiB chunks continuously, retaining a rotating window of them
    # so that both alloc_space and inuse_space accumulate well past the
    # default 512 KiB heap sampling interval.
    retained = []
    while not event.is_set():
        retained.append(bytearray(64 * 1024))
        if len(retained) >= 256:
            del retained[:128]
        time.sleep(0.005)
    return retained


def wait_render(profile_type, canary, needle):
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
            if code == 200 and body != b'' and needle in body:
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

    pyroscope.configure(
        application_name=app_name,
        server_address='http://localhost:4040',
        enable_logging=True,
        mem_enabled=True,
        tags={
            'canary': canary,
        },
    )

    thread = threading.Thread(target=memhog)
    thread.start()

    def watchdog():
        logging.info('Watchdog expired. Test timeout. Exiting...')
        os._exit(7)

    alarm = threading.Timer(120, watchdog)
    alarm.start()

    wait_render('memory:alloc_space:bytes:space:bytes', canary, b'memhog')
    wait_render('memory:inuse_space:bytes:space:bytes', canary, b'memhog')

    alarm.cancel()

    pyroscope.shutdown()

    event.set()
    thread.join()
    logging.info('done')


if __name__ == '__main__':
    main()
