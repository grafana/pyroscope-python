import _thread
import ctypes
import errno
import hashlib
import os
import resource
import signal
import threading
import time
import traceback
from pathlib import Path

import pyroscope


def configure():
    return pyroscope.configure(
        application_name=os.environ.get("PYROSCOPE_APPLICATION_NAME", "ctimer-caller"),
        server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
        # Exercise Signal's default CTimer on Linux.
        cpu_implementation=pyroscope.ProfilerImplementation.Signal,
        sample_rate=100,
        upload_interval=1,
        tags={"canary": os.environ.get("CANARY", "caller")},
        enable_logging=True,
    )


def raw_fork_child_burn():
    end = time.monotonic() + 180
    value = b"raw-fork-child"
    while time.monotonic() < end:
        value = hashlib.sha256(value).digest()


def cpu_bursts():
    assert configure()
    end = time.thread_time() + 2
    value = 0
    while time.thread_time() < end - 0.05:
        burst_end = time.thread_time() + 0.00001
        while time.thread_time() < burst_end:
            value = (value * 1664525 + 1013904223) & 0xFFFFFFFF
        time.sleep(0.000025)
    # Finish the same CPU budget with a runnable tail so overdue timer
    # notifications are delivered before the final drain and shutdown.
    while time.thread_time() < end:
        value = (value * 1664525 + 1013904223) & 0xFFFFFFFF
    # Let the asynchronous resolver drain and upload the final delivery.
    time.sleep(2)
    assert pyroscope.shutdown()


def fork_and_profile():
    fork = ctypes.PyDLL(None).fork
    fork.restype = ctypes.c_int
    pid = fork()
    assert pid >= 0
    if pid == 0:
        try:
            assert configure(), "configure failed in the raw-fork child"
            raw_fork_child_burn()
            assert pyroscope.shutdown()
        except BaseException:
            traceback.print_exc()
            os._exit(1)
        os._exit(0)
    return pid


def raw_fork(unregistered):
    threading.main_thread()
    if unregistered:
        ready = _thread.allocate_lock()
        ready.acquire()
        result = []

        def worker():
            try:
                assert threading.get_ident() not in threading._active
                result.append(fork_and_profile())
            finally:
                ready.release()

        _thread.start_new_thread(worker, ())
        ready.acquire()
        assert len(result) == 1
        pid = result[0]
    else:
        pid = fork_and_profile()
    _, status = os.waitpid(pid, 0)
    assert os.waitstatus_to_exitcode(status) == 0, status


def arming_failure():
    libc = ctypes.CDLL(None, use_errno=True)
    libc.timer_create.argtypes = [
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_void_p),
    ]
    libc.timer_create.restype = ctypes.c_int
    libc.timer_delete.argtypes = [ctypes.c_void_p]
    original_limit = resource.getrlimit(resource.RLIMIT_SIGPENDING)
    resource.setrlimit(resource.RLIMIT_SIGPENDING, (0, original_limit[1]))
    try:
        clock = ctypes.c_int((~threading.get_native_id() << 3) | 6).value
        timer_id = ctypes.c_void_p()
        result = libc.timer_create(clock, None, ctypes.byref(timer_id))
        error = ctypes.get_errno()
        if result == 0:
            libc.timer_delete(timer_id)
        assert result == -1 and error == errno.EAGAIN, (result, error)
        print(
            "timer_create probe failed with EAGAIN under RLIMIT_SIGPENDING=0",
            flush=True,
        )
        configured = configure()
    finally:
        resource.setrlimit(resource.RLIMIT_SIGPENDING, original_limit)
    if configured:
        pyroscope.shutdown()
    assert not configured, "configure accepted a caller without a CTimer"
    assert not Path("/proc/self/timers").read_text().strip()
    action = ctypes.create_string_buffer(256)
    assert libc.sigaction(signal.SIGPROF, None, action) == 0
    assert ctypes.c_void_p.from_buffer(action).value == signal.SIG_IGN
    assert not pyroscope.shutdown(), "failed configure left an agent running"
    assert configure(), "failed configure leaked the timer or handler guard"
    timers = Path("/proc/self/timers").read_text()
    assert f"tid.{threading.get_native_id()}" in timers, timers
    assert pyroscope.shutdown()
    print("caller timer failure was rejected and retry succeeded", flush=True)


scenario = os.environ["SCENARIO"]
if scenario == "arming_failure":
    arming_failure()
elif scenario == "overruns":
    cpu_bursts()
else:
    raw_fork(unregistered=scenario == "unregistered")
