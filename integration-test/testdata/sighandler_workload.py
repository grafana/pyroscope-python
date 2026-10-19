import ctypes
import faulthandler
import mmap
import os
import signal
import subprocess
import sys
import tempfile
import threading
import time
from typing import NamedTuple

import pyroscope


FAULTHANDLER_BANNER = "Fatal Python error: Segmentation fault"
TAKEN_OVER = "SIGSEGV/SIGBUS handler was taken over by another component"

libc = ctypes.CDLL(None)


def handler(signo):
    buf = ctypes.create_string_buffer(256)
    if libc.sigaction(signo, None, buf) != 0:
        raise OSError(ctypes.get_errno(), "sigaction")
    return ctypes.c_void_p.from_buffer(buf).value or 0


def handlers():
    return handler(signal.SIGSEGV), handler(signal.SIGBUS)


def expect(cond, message):
    if not cond:
        raise AssertionError(message)


def configure(expected=True, **kwargs):
    kwargs.setdefault("cpu_implementation", pyroscope.ProfilerImplementation.Stack)
    result = pyroscope.configure(
        application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
        server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
        **kwargs,
    )
    expect(result == expected, f"configure() returned {result}, expected {expected}")


def shutdown():
    expect(pyroscope.shutdown(), "shutdown() returned False")


def segfault():
    ctypes.string_at(0)


def sigbus():
    with tempfile.TemporaryFile() as f:
        f.truncate(mmap.PAGESIZE)
        m = mmap.mmap(f.fileno(), mmap.PAGESIZE)
        f.truncate(0)
        m[0]


def take_over():
    signal.signal(signal.SIGSEGV, signal.SIG_DFL)
    signal.signal(signal.SIGBUS, signal.SIG_DFL)


def burn(stop):
    while not stop.is_set():
        sum(i * i for i in range(10000))


def import_installs_nothing():
    expect(handlers() == (0, 0), f"import installed handlers: {handlers()}")


def pyspy_and_memory_install_nothing():
    configure(cpu_implementation=pyroscope.ProfilerImplementation.PySpy, mem_enabled=True)
    expect(handlers() == (0, 0), f"py-spy + memory installed handlers: {handlers()}")
    shutdown()


def stack_refuses_offcpu():
    configure(expected=False, oncpu=False)
    expect(handlers() == (0, 0), f"a refused configure() installed handlers: {handlers()}")


def first_cpu_configure_wins():
    expect(pyroscope.configure_experimental_stack_profiler(), "the first configure_experimental_stack_profiler() was refused")
    configure()
    shutdown()
    expect(
        not pyroscope.configure_experimental_stack_profiler(),
        "configure_experimental_stack_profiler() was accepted a second time",
    )


def stack_installs_both():
    configure()
    segv, bus = handlers()
    expect(segv != 0 and segv == bus, f"expected one handler on both signals, got {segv:#x} {bus:#x}")
    shutdown()


def reconfigure_keeps_handlers():
    configure()
    ours = handlers()
    shutdown()
    expect(handlers() == ours, "shutdown changed the handlers")
    configure()
    expect(handlers() == ours, f"reconfigure changed the handlers: {ours} -> {handlers()}")
    shutdown()


def foreign_after_configure_is_not_reclaimed():
    configure()
    ours = handlers()
    take_over()
    foreign = handlers()
    expect(foreign != ours, "take_over() did not replace the handlers")
    shutdown()
    configure(expected=False)
    expect(handlers() == foreign, f"reconfigure reinstalled over a foreign handler: {foreign} -> {handlers()}")


def crash_with_our_handler():
    configure()
    segfault()


def sigbus_with_our_handler():
    configure()
    sigbus()


def crash_chains_to_earlier_faulthandler():
    faulthandler.enable()
    foreign = handlers()
    configure()
    expect(handlers() != foreign, "configure() did not install on top of faulthandler")
    segfault()


def crash_after_faulthandler_takeover():
    configure()
    faulthandler.enable()
    time.sleep(1)
    segfault()


def takeover_stops_sampling():
    stop = threading.Event()
    thread = threading.Thread(target=burn, args=(stop,))
    thread.start()
    configure()
    time.sleep(1)
    take_over()
    time.sleep(1)
    shutdown()
    configure(expected=False)
    stop.set()
    thread.join()


class Scenario(NamedTuple):
    returncode: int = 0
    env: dict = {}
    required: tuple = ()
    forbidden: tuple = ()


SCENARIOS = {
    "import_installs_nothing": Scenario(),
    "pyspy_and_memory_install_nothing": Scenario(),
    "stack_refuses_offcpu": Scenario(),
    "first_cpu_configure_wins": Scenario(),
    "stack_installs_both": Scenario(),
    "reconfigure_keeps_handlers": Scenario(),
    "foreign_after_configure_is_not_reclaimed": Scenario(),
    "crash_with_our_handler": Scenario(-signal.SIGSEGV, forbidden=(FAULTHANDLER_BANNER,)),
    "sigbus_with_our_handler": Scenario(-signal.SIGBUS),
    "crash_chains_to_earlier_faulthandler": Scenario(-signal.SIGSEGV, required=(FAULTHANDLER_BANNER,)),
    "crash_after_faulthandler_takeover": Scenario(-signal.SIGSEGV, required=(FAULTHANDLER_BANNER, TAKEN_OVER)),
    "takeover_stops_sampling": Scenario(required=(TAKEN_OVER,)),
}


def main(scenario):
    want = SCENARIOS[scenario]
    env = dict(os.environ, **want.env)
    env.pop("PYTHONFAULTHANDLER", None)
    result = subprocess.run(
        [sys.executable, __file__, "child", scenario],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=120,
        text=True,
    )
    sys.stderr.write(result.stderr)

    expect(
        result.returncode == want.returncode,
        f"{scenario}: child exited with {result.returncode}, expected {want.returncode}",
    )
    for needle in want.required:
        expect(needle in result.stderr, f"{scenario}: stderr lacks {needle!r}")
    for needle in want.forbidden:
        expect(needle not in result.stderr, f"{scenario}: stderr contains {needle!r}")
    expect("panicked" not in result.stderr, f"{scenario}: child stderr contains a native panic")
    print(f"good {scenario}")


if __name__ == "__main__":
    if len(sys.argv) > 2 and sys.argv[1] == "child":
        globals()[sys.argv[2]]()
    else:
        main(os.environ["SCENARIO"])
