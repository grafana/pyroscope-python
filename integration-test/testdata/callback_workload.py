import logging
import os
import time

import pyroscope


def callback(value):
    # Keep enough work in Python for the complete callback stack to receive a
    # measurable share even with faster 3.14+ evaluators and asynchronous sampling.
    for _ in range(4):
        value = (value * 1664525 + 1013904223) & 0xFFFFFFFF
    return value


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
        **(
            {"signal_timer": getattr(pyroscope.SignalTimer, os.environ["SIGNAL_TIMER"])}
            if os.environ.get("CPU_IMPLEMENTATION") == "Signal"
            else {}
        ),
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
        # Each batch still crosses the C/Python boundary 10,000 times.
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            result = sum(map(callback, range(10000)))
            if result != 21_508_692_185_016:
                raise AssertionError(f"unexpected callback sum: {result}")
    finally:
        if not pyroscope.shutdown():
            raise AssertionError("shutdown() returned False")


if __name__ == "__main__":
    main()
