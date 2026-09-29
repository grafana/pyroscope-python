import asyncio
import logging
import os
import sys
import threading
import time
import traceback
import uuid

from urllib.parse import quote
from urllib.request import Request, urlopen

import pyroscope


app_name = 'pyroscopers.python.test.stack.async'
logger = logging.getLogger()

cpu_async = os.getenv('CPU_ASYNC', '1') == '1'
use_uvloop = os.getenv('UVLOOP', '0') == '1'
configure_inside_loop = os.getenv('CONFIGURE_INSIDE_LOOP', '0') == '1'


async def async_cpuburn(done):
    acc = 0
    while not done.is_set():
        for i in range(100000):
            acc = (acc + i * i) % 4294967291
        await asyncio.sleep(0)
    return acc


async def async_idle_forever():
    await asyncio.sleep(3600)


async def exercise_wrappers():
    async def noop():
        return 1

    total = await asyncio.shield(noop())
    finished, _ = await asyncio.wait([asyncio.create_task(noop()), asyncio.create_task(noop())])
    total += sum(task.result() for task in finished)
    for future in asyncio.as_completed([noop(), noop()]):
        total += await future
    if sys.version_info >= (3, 11):
        async with asyncio.TaskGroup() as group:
            total += await group.create_task(noop())
    return total


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


def configure(canary):
    pyroscope.configure(
        application_name=app_name,
        server_address='http://localhost:4040',
        enable_logging=True,
        cpu_enabled=True,
        cpu_implementation=pyroscope.ProfilerImplementation.Stack,
        cpu_async=cpu_async,
        mem_enabled=False,
        tags={
            'canary': canary,
        },
    )


async def amain(canary, done):
    if configure_inside_loop:
        configure(canary)

    idle = asyncio.create_task(async_idle_forever())
    wrappers = await exercise_wrappers()
    expected = 6 if sys.version_info >= (3, 11) else 5
    assert wrappers == expected, f'the asyncio wrappers changed a result: {wrappers} != {expected}'

    await asyncio.gather(async_cpuburn(done), async_cpuburn(done))

    idle.cancel()
    try:
        await idle
    except asyncio.CancelledError:
        pass


def main():
    logger.setLevel(logging.INFO)
    canary = uuid.uuid4().hex
    logging.info('canary %s', canary)

    runner = asyncio.run
    if use_uvloop:
        import uvloop
        runner = uvloop.run

    if not configure_inside_loop:
        configure(canary)

    done = threading.Event()

    def check():
        wait_render('process_cpu:cpu:nanoseconds:cpu:nanoseconds', canary, b'async_cpuburn')
        # A coroutine parked in await is not on the thread stack, so this frame
        # can only have come from echion's task unwinder.
        wait_render('process_cpu:wall:nanoseconds:cpu:nanoseconds', canary, b'async_idle_forever')
        done.set()

    checker = threading.Thread(target=check)
    checker.start()

    def watchdog():
        logging.info('Watchdog expired. Test timeout. Exiting...')
        os._exit(7)

    alarm = threading.Timer(180, watchdog)
    alarm.start()

    runner(amain(canary, done))

    alarm.cancel()
    checker.join()
    pyroscope.shutdown()
    logging.info('done')


if __name__ == '__main__':
    main()
