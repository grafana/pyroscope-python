from setuptools import setup
from setuptools_rust import Binding, RustExtension
from pathlib import Path
import sys
import sysconfig
import os

# The C++ memalloc profiler reads version-specific CPython internal structs, so
# it must be compiled against the exact Python the wheel targets. Pass the
# building interpreter and its install root down to build.rs, which forwards
# them to CMake as Python3_EXECUTABLE / Python3_ROOT_DIR. Python3_EXECUTABLE
# pins the exact interpreter even when several Pythons share a prefix (the
# root dir alone is just a search hint). sys.base_prefix points at the real
# installation even when building inside an isolated (PEP 517) build
# environment.
python_root = Path(sys.base_prefix).resolve()

env = os.environ.copy()
env.update({
    "Python3_ROOT_DIR": f"{python_root}",
    "Python3_EXECUTABLE": sys.executable,
})

# Memory profiling requires CPython 3.13+ with the GIL enabled.
#
# The C++ memalloc profiler still works on 3.10-3.12 today; the floor is set
# ahead of the in-progress rewrite to Rust. Rust cannot include CPython's
# internal headers, so the Rust profiler locates the interpreter frame and code
# object fields through _Py_DebugOffsets -- the self-describing offset table
# that is the first member of _PyRuntimeState. CPython only exports it from
# 3.13 onwards, and there is no equivalent on 3.10-3.12.
#
# Free-threaded builds are excluded because the allocator hook relies on
# running with the GIL held; see the #error in cpp/_memalloc_frame.h.
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
