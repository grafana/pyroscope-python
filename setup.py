from setuptools import setup
from setuptools_rust import Binding, RustExtension
from pathlib import Path
import sys
import sysconfig
import os

# The C++ memalloc profiler reads version-specific CPython internal structs, so
# it must be compiled against the exact Python the wheel targets. Pass the
# interpreter's install root down to build.rs, which forwards it to CMake as
# Python3_ROOT_DIR. sys.base_prefix points at the real installation even when
# building inside an isolated (PEP 517) build environment.
python_root = Path(sys.base_prefix).resolve()

env = os.environ.copy()
env.update({
    "PYROSCOPE__Python3_ROOT_DIR": f"{python_root}",
})

features = []
# if sysconfig.get_config_vars().get("Py_GIL_DISABLED") != 1:
#     features.append("memory")

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
