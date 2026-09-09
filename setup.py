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

features = []
if sysconfig.get_config_var("Py_GIL_DISABLED") != 1:
    features.append("memory")
    if sys.platform.startswith("linux") and sys.version_info < (3, 12):
        features.append("gcp")

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
