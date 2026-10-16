#!/usr/bin/env python3
"""Reproduce ddtrace's dangling alternate signal stack after fork on Linux."""

import ctypes
import os
import signal
import threading
import time


# These must be set before importing ddtrace.
os.environ["DD_PROFILING_OUTPUT_PPROF"] = "/tmp/ddtrace-postfork-altstack"
os.environ["_DD_PROFILING_STACK_ADAPTIVE_SAMPLING_ENABLED"] = "0"
os.environ["_DD_PROFILING_STACK_CPU_TIMER_ENABLED"] = "1"
os.environ["_DD_PROFILING_STACK_CPU_TIMER_INTERVAL_MS"] = "1"

from ddtrace.profiling import profiler


class Sigaction(ctypes.Structure):
    _fields_ = [
        ("sa_handler", ctypes.c_void_p),
        ("sa_mask", ctypes.c_ulong * (128 // ctypes.sizeof(ctypes.c_ulong))),
        ("sa_flags", ctypes.c_int),
        ("sa_restorer", ctypes.c_void_p),
    ]


libc = ctypes.CDLL(None, use_errno=True)
action = Sigaction(
    sa_handler=ctypes.cast(libc.getpid, ctypes.c_void_p).value,
    sa_flags=0x08000000,  # SA_ONSTACK
)
assert libc.sigaction(signal.SIGUSR1, ctypes.byref(action), None) == 0

p = profiler.Profiler()
p.start()
result = []


def fork_from_profiled_worker():
    # Give the sampler time to discover this worker and install its stack.
    deadline = time.thread_time() + 0.1
    while time.thread_time() < deadline:
        pass

    pid = os.fork()
    if pid == 0:
        # ddtrace's at-fork hook unmapped the still-registered alt stack.
        signal.raise_signal(signal.SIGUSR1)
        os._exit(0)
    result.append(os.waitpid(pid, 0)[1])


thread = threading.Thread(target=fork_from_profiled_worker)
thread.start()
thread.join()
p.stop()

assert os.waitstatus_to_exitcode(result[0]) == -signal.SIGSEGV, result[0]
print("reproduced: SA_ONSTACK signal crashed the fork child with SIGSEGV")
