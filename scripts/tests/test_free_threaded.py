"""Smoke-test a free-threaded wheel: build and import integrity only.

Asserting CPU behaviour would pin a bug -- py-spy cannot attach here (#163)
and configure() reports success regardless (#164). See #166.
"""

import subprocess
import sys
import sysconfig

MEMORY_WARNING = "does not include memory profiling support"

CHILD = """
import pyroscope
pyroscope.configure(
    application_name="free-threaded-smoke",
    enable_logging=True,
    cpu_enabled=False,
    mem_enabled=True,
    upload_interval=3600,
)
pyroscope.shutdown()
"""


def check_free_threaded():
    if sysconfig.get_config_var("Py_GIL_DISABLED") != 1:
        raise AssertionError("not a free-threaded interpreter; run this under python3.14t")
    print("ok: free-threaded build")


def check_import():
    import pyroscope

    if not hasattr(pyroscope, "configure"):
        raise AssertionError("pyroscope.configure is missing")
    from pyroscope import _native

    print(f"ok: imported {_native.__file__}")
    print(f"info: sys._is_gil_enabled() = {sys._is_gil_enabled()}")


def check_memory_profiling_is_refused():
    proc = subprocess.run(
        [sys.executable, "-c", CHILD], capture_output=True, text=True, timeout=120
    )
    if proc.returncode != 0:
        raise AssertionError(
            f"configure() with mem_enabled=True failed:\n{proc.stdout}\n{proc.stderr}"
        )
    if MEMORY_WARNING not in proc.stderr:
        raise AssertionError(
            f"expected {MEMORY_WARNING!r} on stderr, got:\n{proc.stderr}"
        )
    print("ok: mem_enabled=True warns instead of failing")


def main():
    check_free_threaded()
    check_import()
    check_memory_profiling_is_refused()
    print("all free-threaded smoke checks passed")


if __name__ == "__main__":
    main()
