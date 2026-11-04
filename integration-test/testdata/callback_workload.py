import logging
import os
import time

import pyroscope


def callback(value):
    return value + 1


def main():
    logging.basicConfig(level=logging.INFO)
    if os.environ.get("STACK_WALL") == "true":
        if not pyroscope.configure_experimental_stack_profiler(cpu_timer_enabled=False):
            raise AssertionError("could not configure the Stack wall sampler")
    if not pyroscope.configure(
        application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
        server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
        cpu_implementation=getattr(
            pyroscope.ProfilerImplementation, os.environ["CPU_IMPLEMENTATION"]
        ),
        signal_timer=getattr(pyroscope.SignalTimer, os.environ["SIGNAL_TIMER"]),
        sample_rate=100,
        oncpu=True,
        gil_only=True,
        upload_interval=1,
        tags={"canary": os.environ["CANARY"]},
    ):
        raise AssertionError("configure() returned False")
    try:
        # map enters Python from C for every callback. Unlike ordinary Python
        # calls on 3.11+, these calls repeatedly return from the C evaluator.
        # On 3.12+ this retires its stack-local entry_frame as well as any
        # _PyCFrame: an asynchronous unwinder can retain either pointer.
        # Keep callbacks short to maximize the number of lifetime transitions.
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            result = sum(map(callback, range(10000)))
            if result != 50005000:
                raise AssertionError(f"unexpected callback sum: {result}")
    finally:
        if not pyroscope.shutdown():
            raise AssertionError("shutdown() returned False")


if __name__ == "__main__":
    main()
