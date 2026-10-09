import asyncio
import logging
import os
import signal
import sys
import threading

import pyroscope

try:
    import uvloop
except ImportError:
    uvloop = None


logger = logging.getLogger(__name__)
shutdown_requested = threading.Event()


async def async_burn(stop):
    x = 0
    while not stop.is_set():
        for _ in range(100000):
            x = (x * 31 + 7) % 1000003
        await asyncio.sleep(0)
    return x


async def async_idle():
    await asyncio.sleep(3600)


def request_shutdown(signum, _frame):
    logger.info("received signal %s, exiting", signum)
    shutdown_requested.set()


async def amain():
    loop = type(asyncio.get_running_loop())
    logger.info("event loop %s.%s", loop.__module__, loop.__qualname__)

    idle = asyncio.create_task(async_idle())
    stop = threading.Event()
    burners = asyncio.gather(async_burn(stop), async_burn(stop))

    while not shutdown_requested.is_set():
        await asyncio.sleep(0.5)

    stop.set()
    await burners
    idle.cancel()
    try:
        await idle
    except asyncio.CancelledError:
        pass


def main():
    logging.basicConfig(level=logging.INFO)
    signal.signal(signal.SIGINT, request_shutdown)
    signal.signal(signal.SIGTERM, request_shutdown)

    runner = asyncio.run if uvloop is None else uvloop.run

    if not pyroscope.configure_experimental_stack_profiler(async_tracking=True):
        raise AssertionError("configure_experimental_stack_profiler() returned False")

    if not pyroscope.configure(
        application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
        server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
        enable_logging=True,
        oncpu=False,
        cpu_enabled=True,
        cpu_implementation=pyroscope.ProfilerImplementation.Stack,
        mem_enabled=False,
        upload_interval=1,
        tags={"canary": os.environ["CANARY"]},
    ):
        raise AssertionError("configure() returned False")

    runner(amain())

    if not pyroscope.shutdown():
        raise AssertionError("shutdown() returned False")
    logger.info("async workload stopped")


if __name__ == "__main__":
    try:
        main()
    except Exception:
        logger.exception("async workload failed")
        sys.exit(1)
