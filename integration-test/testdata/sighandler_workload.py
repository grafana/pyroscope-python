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
CPU_TIMER_RESTART_REFUSED = "the CPU timer stack profiler cannot be restarted in this process"

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


def configure_agent(**kwargs):
    kwargs.setdefault("cpu_implementation", pyroscope.ProfilerImplementation.Stack)
    return pyroscope.configure(
        application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
        server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
        **kwargs,
    )


def configure(cpu_timer_enabled=True, **kwargs):
    # Refused after the first call, which reconfigure_keeps_handlers relies on.
    pyroscope.configure_experimental_stack_profiler(cpu_timer_enabled=cpu_timer_enabled)
    expect(configure_agent(**kwargs), "configure() returned False")


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


def stack_offcpu_refused():
    pyroscope.configure_experimental_stack_profiler()
    expect(
        not pyroscope.configure(
            application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
            server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
            cpu_implementation=pyroscope.ProfilerImplementation.Stack,
            oncpu=False,
        ),
        "configure() with Stack and oncpu=False returned True",
    )
    expect(handlers() == (0, 0), f"refused configure installed handlers: {handlers()}")


def pyspy_and_memory_install_nothing():
    configure(cpu_implementation=pyroscope.ProfilerImplementation.PySpy, mem_enabled=True)
    expect(handlers() == (0, 0), f"py-spy + memory installed handlers: {handlers()}")
    shutdown()


def first_cpu_configure_wins():
    configure(cpu_timer_enabled=False)
    shutdown()
    expect(
        not pyroscope.configure_experimental_stack_profiler(cpu_timer_enabled=True),
        "configure_experimental_stack_profiler() was accepted a second time",
    )
    # A restart is refused with the CPU timer on, so this passes only if the first call won.
    configure()
    shutdown()


def stack_installs_both():
    configure()
    segv, bus = handlers()
    expect(segv != 0 and segv == bus, f"expected one handler on both signals, got {segv:#x} {bus:#x}")
    shutdown()


def faulthandler_unpatched():
    enable, disable = faulthandler.enable, faulthandler.disable
    configure()
    expect(faulthandler.enable is enable, "configure() patched faulthandler.enable")
    expect(faulthandler.disable is disable, "configure() patched faulthandler.disable")
    shutdown()


def reconfigure_keeps_handlers():
    configure(cpu_timer_enabled=False)
    ours = handlers()
    shutdown()
    expect(handlers() == ours, "shutdown changed the handlers")
    configure()
    expect(handlers() == ours, f"reconfigure changed the handlers: {ours} -> {handlers()}")
    shutdown()


def foreign_after_configure_is_not_reclaimed():
    configure(cpu_timer_enabled=False)
    ours = handlers()
    take_over()
    foreign = handlers()
    expect(foreign != ours, "take_over() did not replace the handlers")
    shutdown()
    configure()
    expect(handlers() == foreign, f"reconfigure reinstalled over a foreign handler: {foreign} -> {handlers()}")
    shutdown()


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
    segfault()


def faulthandler_enable_after_configure_stops_sampling():
    configure()
    faulthandler.enable()
    time.sleep(1)
    shutdown()


def takeover_stops_sampling():
    stop = threading.Event()
    thread = threading.Thread(target=burn, args=(stop,))
    thread.start()
    configure(cpu_timer_enabled=False)
    time.sleep(0.5)
    take_over()
    time.sleep(1)
    shutdown()
    configure()
    time.sleep(1)
    shutdown()
    stop.set()
    thread.join()


def cpu_timer_restart_refused():
    configure()
    shutdown()
    expect(not configure_agent(enable_logging=True), "configure() after shutdown() was accepted with the CPU timer on")


def cpu_timer_fork_child_refused():
    configure()
    pid = os.fork()
    if pid == 0:
        os._exit(0 if not configure_agent(enable_logging=True) else 1)
    _, status = os.waitpid(pid, 0)
    expect(os.waitstatus_to_exitcode(status) == 0, "configure() in a fork child was accepted with the CPU timer on")
    shutdown()


def cpu_timer_unsupported_fallback():
    # On CPython 3.10/3.11, enabling the timer option still selects the wall
    # sampler. It allocates no alternate stack and can restart after shutdown.
    configure()
    run_worker(lambda _: expect(not altstack_enabled(), "fallback installed a timer alternate stack"))
    shutdown()
    expect(configure_agent(), "fallback could not restart after shutdown()")
    shutdown()

    # Fork only after shutdown: forking with a running profiler is unsupported.
    pid = os.fork()
    if pid == 0:
        ok = configure_agent() and pyroscope.shutdown()
        os._exit(0 if ok else 1)
    expect(wait_exitcode(pid) == 0, "fallback could not configure in a fork child")


SA_ONSTACK = 0x08000000
SS_ONSTACK = 1
SS_DISABLE = 2
APP_ALTSTACK_SIZE = 1 << 20
AFTER_FORK_BURN_SECONDS = 15


class Sigaction(ctypes.Structure):
    _fields_ = [
        ("sa_handler", ctypes.c_void_p),
        ("sa_mask", ctypes.c_ulong * (128 // ctypes.sizeof(ctypes.c_ulong))),
        ("sa_flags", ctypes.c_int),
        ("sa_restorer", ctypes.c_void_p),
    ]


class StackT(ctypes.Structure):
    _fields_ = [("ss_sp", ctypes.c_void_p), ("ss_flags", ctypes.c_int), ("ss_size", ctypes.c_size_t)]


def install_onstack(signo, address):
    action = Sigaction(sa_handler=address, sa_flags=SA_ONSTACK)
    if libc.sigaction(signo, ctypes.byref(action), None) != 0:
        raise OSError(ctypes.get_errno(), "sigaction")


def install_harmless_onstack(signo):
    install_onstack(signo, ctypes.cast(libc.getpid, ctypes.c_void_p).value)


def altstack():
    current = StackT()
    if libc.sigaltstack(None, ctypes.byref(current)) != 0:
        raise OSError(ctypes.get_errno(), "sigaltstack")
    return current


def altstack_enabled():
    return not altstack().ss_flags & SS_DISABLE


def mapped(address):
    with open("/proc/self/maps") as f:
        for line in f:
            start, end = (int(x, 16) for x in line.split()[0].split("-"))
            if start <= address < end:
                return True
    return False


def wait_exitcode(pid):
    _, status = os.waitpid(pid, 0)
    return os.waitstatus_to_exitcode(status)


def fork_raising_onstack_signal():
    pid = os.fork()
    if pid == 0:
        signal.raise_signal(signal.SIGUSR1)
        os._exit(0)
    return wait_exitcode(pid)


def burn_for(seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        sum(i * i for i in range(10000))


def burn_after_fork():
    burn_for(AFTER_FORK_BURN_SECONDS)


def run_worker(target):
    result = {}

    def work():
        try:
            target(result)
        except BaseException as e:
            result["error"] = repr(e)

    thread = threading.Thread(target=work)
    thread.start()
    thread.join()
    expect("error" not in result, f"worker failed: {result.get('error')}")
    return result


def cpu_timer_worker_fork_onstack_signal():
    install_harmless_onstack(signal.SIGUSR1)
    canary = os.environ.get("CANARY")
    configure(**({"tags": {"canary": canary}} if canary else {}))

    def work(result):
        expect(altstack_enabled(), "profiled worker has no alternate signal stack")
        burn_for(1)
        result["child"] = fork_raising_onstack_signal()
        burn_after_fork()

    result = run_worker(work)
    expect(result["child"] == 0, f"fork child exited with {result['child']}")
    shutdown()


def cpu_timer_shutdown_elsewhere_then_fork():
    install_harmless_onstack(signal.SIGUSR1)
    configure()
    registered = threading.Event()
    stopped = threading.Event()

    def work(result):
        try:
            expect(altstack_enabled(), "profiled worker has no alternate signal stack")
        finally:
            registered.set()
        stopped.wait()
        expect(altstack_enabled(), "shutdown() disabled the worker's alternate signal stack")
        result["child"] = fork_raising_onstack_signal()

    def shut_down_then_release():
        registered.wait()
        try:
            shutdown()
        finally:
            stopped.set()

    stopper = threading.Thread(target=shut_down_then_release)
    stopper.start()
    result = run_worker(work)
    stopper.join()
    expect(result["child"] == 0, f"fork child exited with {result['child']}")


def cpu_timer_fork_keeps_app_altstack():
    install_harmless_onstack(signal.SIGUSR1)
    previous = altstack()
    with mmap.mmap(-1, APP_ALTSTACK_SIZE, flags=mmap.MAP_PRIVATE) as buf:
        sp = ctypes.addressof(ctypes.c_char.from_buffer(buf))
        if libc.sigaltstack(ctypes.byref(StackT(ss_sp=sp, ss_flags=0, ss_size=APP_ALTSTACK_SIZE)), None) != 0:
            raise OSError(ctypes.get_errno(), "sigaltstack")

        def unchanged():
            current = altstack()
            return not current.ss_flags & SS_DISABLE and current.ss_sp == sp and current.ss_size == APP_ALTSTACK_SIZE

        try:
            configure()
            expect(unchanged(), "configure() replaced the application's alternate signal stack")
            pid = os.fork()
            if pid == 0:
                ok = unchanged()
                signal.raise_signal(signal.SIGUSR1)
                os._exit(0 if ok else 1)
            code = wait_exitcode(pid)
            expect(code == 0, f"fork child exited with {code}")
            expect(unchanged(), "fork changed the parent's alternate signal stack")
            shutdown()
            expect(unchanged(), "shutdown() changed the application's alternate signal stack")
        finally:
            # sigaltstack borrows the mapping. Restore the previous stack before
            # mmap closes, or a pending SA_ONSTACK signal can use unmapped memory.
            if libc.sigaltstack(ctypes.byref(previous), None) != 0:
                raise OSError(ctypes.get_errno(), "sigaltstack")

    # Exercise SA_ONSTACK delivery after the application's mapping is released.
    signal.raise_signal(signal.SIGUSR1)


def cpu_timer_fork_on_altstack_keeps_mapping():
    configure()

    def work(result):
        expect(altstack_enabled(), "profiled worker has no alternate signal stack")

        def on_signal(_signo):
            pid = os.fork()
            if pid == 0:
                current = altstack()
                os._exit(0 if current.ss_flags & SS_ONSTACK and mapped(current.ss_sp) else 1)
            result["child"] = wait_exitcode(pid)

        handler = ctypes.CFUNCTYPE(None, ctypes.c_int)(on_signal)
        install_onstack(signal.SIGUSR2, ctypes.cast(handler, ctypes.c_void_p).value)
        getattr(libc, "raise")(signal.SIGUSR2)
        install_onstack(signal.SIGUSR2, None)

    result = run_worker(work)
    expect(result.get("child") == 0, f"fork child exited with {result.get('child')}")
    shutdown()


class Scenario(NamedTuple):
    returncode: int = 0
    env: dict = {}
    required: tuple = ()
    forbidden: tuple = ()


SCENARIOS = {
    "import_installs_nothing": Scenario(),
    "stack_offcpu_refused": Scenario(),
    "pyspy_and_memory_install_nothing": Scenario(),
    "first_cpu_configure_wins": Scenario(),
    "stack_installs_both": Scenario(),
    "faulthandler_unpatched": Scenario(),
    "reconfigure_keeps_handlers": Scenario(),
    "foreign_after_configure_is_not_reclaimed": Scenario(),
    "crash_with_our_handler": Scenario(-signal.SIGSEGV, forbidden=(FAULTHANDLER_BANNER,)),
    "sigbus_with_our_handler": Scenario(-signal.SIGBUS),
    "crash_chains_to_earlier_faulthandler": Scenario(-signal.SIGSEGV, required=(FAULTHANDLER_BANNER,)),
    "crash_after_faulthandler_takeover": Scenario(-signal.SIGSEGV, required=(FAULTHANDLER_BANNER,)),
    "faulthandler_enable_after_configure_stops_sampling": Scenario(required=(TAKEN_OVER,)),
    "takeover_stops_sampling": Scenario(required=(TAKEN_OVER,)),
    "cpu_timer_restart_refused": Scenario(required=(CPU_TIMER_RESTART_REFUSED,)),
    "cpu_timer_fork_child_refused": Scenario(required=(CPU_TIMER_RESTART_REFUSED,)),
    "cpu_timer_unsupported_fallback": Scenario(forbidden=(CPU_TIMER_RESTART_REFUSED,)),
    "cpu_timer_worker_fork_onstack_signal": Scenario(),
    "cpu_timer_shutdown_elsewhere_then_fork": Scenario(),
    "cpu_timer_fork_keeps_app_altstack": Scenario(),
    "cpu_timer_fork_on_altstack_keeps_mapping": Scenario(),
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
