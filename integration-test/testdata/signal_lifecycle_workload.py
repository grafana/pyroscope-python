import ctypes
import faulthandler
import hashlib
import os
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

import pyroscope


def configure(**kwargs):
    return pyroscope.configure(
        application_name="signal-lifecycle",
        server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
        cpu_implementation=pyroscope.ProfilerImplementation.Signal,
        **kwargs,
    )


def zero_rate():
    for timer in (pyroscope.SignalTimer.ITimer, pyroscope.SignalTimer.CTimer):
        assert not configure(sample_rate=0, signal_timer=timer)
    assert configure()
    assert pyroscope.shutdown()


def ctimer_shutdown_fork():
    timer = pyroscope.SignalTimer.CTimer
    assert configure(signal_timer=timer)
    assert pyroscope.shutdown()
    pid = os.fork()
    if pid == 0:
        try:
            assert configure(signal_timer=timer)
            timers = Path("/proc/self/timers").read_text()
            assert f"tid.{threading.get_native_id()}" in timers, timers
            assert pyroscope.shutdown()
        except BaseException:
            import traceback

            traceback.print_exc()
            os._exit(1)
        os._exit(0)
    _, status = os.waitpid(pid, 0)
    assert os.waitstatus_to_exitcode(status) == 0, status


def ctimer_preexisting_thread_cleanup():
    assert sys.version_info >= (3, 11)
    release_old = threading.Event()
    release_new = threading.Event()
    ready_old = threading.Event()
    ready_new = threading.Event()

    def worker(ready, release):
        ready.set()
        assert release.wait(30)

    old = threading.Thread(target=worker, args=(ready_old, release_old))
    old.start()
    assert ready_old.wait(10)

    try:
        assert configure(signal_timer=pyroscope.SignalTimer.CTimer)
        old_timer = f"tid.{old.native_id}\n"
        assert old_timer in Path("/proc/self/timers").read_text()

        new = threading.Thread(target=worker, args=(ready_new, release_new))
        new.start()
        try:
            assert ready_new.wait(10)
            new_timer = f"tid.{new.native_id}\n"
            timers = Path("/proc/self/timers").read_text()
            assert old_timer in timers and new_timer in timers, timers

            release_old.set()
            old.join(10)
            assert not old.is_alive()
            timers = Path("/proc/self/timers").read_text()
            assert old_timer not in timers and new_timer in timers, timers

            assert pyroscope.shutdown()
            assert not Path("/proc/self/timers").read_text().strip()
        finally:
            release_new.set()
            new.join(10)
            assert not new.is_alive()
    finally:
        release_old.set()
        old.join(10)


def recovery_handlers():
    libc = ctypes.CDLL(None)
    result = []
    for signum in (signal.SIGSEGV, signal.SIGBUS):
        action = ctypes.create_string_buffer(256)
        assert libc.sigaction(signum, None, action) == 0
        result.append(ctypes.c_void_p.from_buffer(action).value)
    return tuple(result)


def recovery_handlers_preserved():
    # Install application handlers before initialization. Recovery handlers must
    # remain installed for the lifetime of the process, including across stops.
    faulthandler.enable()
    previous = recovery_handlers()
    assert configure()
    installed = recovery_handlers()
    assert all(installed)
    assert all(ours != earlier for ours, earlier in zip(installed, previous))
    signal.raise_signal(signal.SIGPROF)
    assert pyroscope.shutdown()
    assert recovery_handlers() == installed
    assert configure()
    assert recovery_handlers() == installed
    signal.raise_signal(signal.SIGPROF)
    assert pyroscope.shutdown()
    assert recovery_handlers() == installed
    assert faulthandler.is_enabled()


def bounded_drain():
    stop = threading.Event()
    blob = b"x" * 1048576

    def deep(depth):
        if depth:
            return deep(depth - 1)
        while not stop.is_set():
            hashlib.sha256(blob).digest()  # Native CPU work releases the GIL.

    timer = (
        pyroscope.SignalTimer.CTimer
        if sys.platform == "linux"
        else pyroscope.SignalTimer.ITimer
    )
    assert configure(signal_timer=timer, sample_rate=1000)
    threads = [threading.Thread(target=deep, args=(200,)) for _ in range(8)]
    for thread in threads:
        thread.start()
    try:
        time.sleep(1)
        # Keep producing until shutdown returns. A broken unbounded drain cannot rely on
        # the workers stopping to empty the queue; the harness times out such a process.
        assert pyroscope.shutdown()
    finally:
        stop.set()
        for thread in threads:
            thread.join()


def fault_fallback_child():
    signum = int(os.environ["FALLBACK_SIGNAL"])
    ignored = os.environ["FALLBACK_IGNORED"] == "1"
    signal.signal(signum, signal.SIG_IGN if ignored else signal.SIG_DFL)
    if sys.platform == "darwin" and ignored:
        # Darwin cannot distinguish sent recovery signals from memory faults.
        # Initialization rejects SIG_IGN without replacing either handler.
        previous = recovery_handlers()
        assert not configure()
        assert recovery_handlers() == previous
        signal.raise_signal(signum)
        return
    assert configure()
    if os.environ["FALLBACK_STOPPED"] == "1":
        assert pyroscope.shutdown()
    # Normal user-generated delivery tests fallback dispatch, not memory faults.
    signal.raise_signal(signum)
    assert ignored, "default SIGBUS/SIGSEGV unexpectedly returned"
    if os.environ["FALLBACK_STOPPED"] == "0":
        assert pyroscope.shutdown()


def fault_fallback():
    for signum in (signal.SIGBUS, signal.SIGSEGV):
        for ignored in (False, True):
            for stopped in (False, True):
                env = dict(
                    os.environ,
                    SCENARIO="fault_fallback_child",
                    FALLBACK_SIGNAL=str(int(signum)),
                    FALLBACK_IGNORED=str(int(ignored)),
                    FALLBACK_STOPPED=str(int(stopped)),
                )
                child = subprocess.run([sys.executable, __file__], env=env, timeout=30)
                expected = 0 if ignored else -int(signum)
                assert child.returncode == expected, (
                    signum,
                    ignored,
                    stopped,
                    child.returncode,
                )
                print("fallback passed", signum, ignored, stopped, flush=True)


globals()[os.environ["SCENARIO"]]()
