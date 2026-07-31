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

sanitizer = os.environ.get("PYROSCOPE_SANITIZER")
if sanitizer not in (None, "address", "thread"):
    raise RuntimeError(
        "PYROSCOPE_SANITIZER must be unset, 'address', or 'thread'"
    )

cargo_args = []
if sanitizer:
    if not os.environ.get("CARGO_BUILD_TARGET"):
        raise RuntimeError(
            "CARGO_BUILD_TARGET must be set for sanitizer builds so Rust "
            "build scripts and host tools are not instrumented"
        )
    # Instrument std as well as the extension. This avoids false positives in
    # synchronization primitives and requires nightly plus the rust-src
    # component.
    cargo_args.append("-Zbuild-std=std")

features = []
if sysconfig.get_config_var("Py_GIL_DISABLED") != 1:
    features.append("memory")

setup(
    rust_extensions=[
        RustExtension(
            "pyroscope._native",
            path="rust/Cargo.toml",
            binding=Binding.PyO3,
            args=cargo_args,
            cargo_manifest_args=["--locked"],
            features=features,
            env=env,
        )
    ],
)
