from setuptools import setup
from setuptools_rust import Binding, RustExtension
import os
import sys
import sysconfig

# The memory profiler compiles a view of CPython's internal struct layouts that
# is specific to the target minor version, selected by cfgs build.rs derives
# from the interpreter pyo3-build-config finds. Pin that interpreter to the one
# this wheel is being built for, rather than whatever `python3` happens to be
# first on PATH. sys.base_prefix and sys.executable both point at the real
# installation even inside an isolated (PEP 517) build environment.
env = os.environ.copy()
env["PYO3_PYTHON"] = sys.executable

# Memory profiling requires CPython 3.13+ with the GIL enabled.
#
# Rust cannot include CPython's internal headers, so the profiler locates the
# interpreter frame and code object fields through _Py_DebugOffsets -- the
# self-describing offset table that is the first member of _PyRuntimeState.
# CPython only exports it from 3.13 onwards, and there is no equivalent on
# 3.10-3.12.
#
# Free-threaded builds are excluded because the allocator hook relies on
# running with the GIL held; see the compile_error! in
# rust/src/memalloc/mod.rs.
#
# With the feature off, mem_enabled is accepted and ignored with a warning (see
# memory::start in rust/src/memory.rs), so those wheels still build and still
# do CPU profiling.
MEMORY_MIN_PYTHON = (3, 13)

features = []
if (
    sys.version_info >= MEMORY_MIN_PYTHON
    and sysconfig.get_config_var("Py_GIL_DISABLED") != 1
):
    features.append("memory")

    # Opt-in diagnostics, used by scripts/check_debug_offsets.py and
    # scripts/check_frame_walk.py when bringing up a new CPython version.
    # Off in shipped wheels.
    if os.environ.get("PYROSCOPE_DEBUG_INTROSPECTION") == "1":
        features.append("debug-introspection")

setup(
    rust_extensions=[
        RustExtension(
            "pyroscope._native",
            path="rust/Cargo.toml",
            binding=Binding.PyO3,
            cargo_manifest_args=["--locked"],
            features=features,
            env=env,
        )
    ],
)
