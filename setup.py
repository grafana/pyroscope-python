from setuptools import setup
from setuptools_rust import Binding, RustExtension

setup(
    rust_extensions=[
        RustExtension(
            "pyroscope._native",
            path="rust/Cargo.toml",
            binding=Binding.PyO3,
            cargo_manifest_args=["--locked"],
        )
    ],
)
