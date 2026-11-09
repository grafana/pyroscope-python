"""Sample the configure caller and a worker retained across a profiler restart."""

import hashlib
import os
import threading
import time

import pyroscope


def burn():
    # Large hashes release the GIL: the cached state must also work while detached.
    data = b"x" * (1 << 20)
    deadline = time.thread_time() + 2
    while time.thread_time() < deadline:
        hashlib.sha256(data).digest()


def worker(starts, finished):
    for start, done in zip(starts, finished):
        assert start.wait(30)
        burn()
        done.set()


def main():
    # Startup enumerates this thread from the configure caller. Its state must
    # never replace the caller's TLS slot. On 3.15 it remains unregistered.
    idle_done = threading.Event()
    idle = threading.Thread(target=idle_done.wait)
    idle.start()
    starts = [threading.Event(), threading.Event()]
    finished = [threading.Event(), threading.Event()]
    for phase in range(2):
        assert pyroscope.configure(
            application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
            server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
            cpu_implementation=pyroscope.ProfilerImplementation.Signal,
            signal_timer=getattr(pyroscope.SignalTimer, os.environ["SIGNAL_TIMER"]),
            sample_rate=100,
            upload_interval=1,
            tags={"phase": str(phase)},
        )
        if phase == 0:
            active = threading.Thread(target=worker, args=(starts, finished))
            active.start()
        burn()
        starts[phase].set()
        assert finished[phase].wait(30)
        if phase == 1:
            active.join(5)
            assert not active.is_alive()
        assert pyroscope.shutdown()
    idle_done.set()
    idle.join(5)
    assert not idle.is_alive()


if __name__ == "__main__":
    main()
