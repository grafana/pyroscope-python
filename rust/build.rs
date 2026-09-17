use cmake::Config;
use std::env;
use std::path::{Path, PathBuf};

const NATIVE_SOURCES: &[&str] = &["CMakeLists.txt", "_memalloc.cpp", "_pymacro.h"];

/// CPython minor versions we have a transcribed `_Py_DebugOffsets` mirror for.
///
/// Emitting a cfg only for these makes the mirror selection fail closed: a
/// newer CPython gets no cfg, so the profiler reports the version as
/// unsupported instead of reading one layout out of another.
const SUPPORTED_MINORS: &[u8] = &[13, 14];

fn main() {
    // Must run before the early return below, and in every feature
    // configuration: it is what declares the `Py_3_*` cfgs, and referencing an
    // undeclared cfg is an `unexpected_cfgs` warning, which `--deny warnings`
    // turns into an error.
    pyo3_build_config::use_pyo3_cfgs();
    emit_target_python_cfgs();

    if cfg!(not(feature = "memory")) {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let cpp_dir = manifest_dir.join("../cpp");
    let cpp_dir = cpp_dir.canonicalize().unwrap();

    rerun_if_native_sources_changed(&manifest_dir, &cpp_dir);

    let mut cfg = Config::new(&cpp_dir);

    println!("cargo:rerun-if-env-changed=Python3_ROOT_DIR");
    let python_root = env::var_os("Python3_ROOT_DIR")
        .expect("Python3_ROOT_DIR must be set (passed from setup.py) so the C++ memalloc profiler is compiled against the target Python version");
    cfg.define("Python3_ROOT_DIR", &python_root);
    println!("cargo:rerun-if-env-changed=Python3_EXECUTABLE");
    let python_executable = env::var_os("Python3_EXECUTABLE")
        .expect("Python3_EXECUTABLE must be set (passed from setup.py) so the C++ memalloc profiler is compiled against the exact target Python interpreter");
    cfg.define("Python3_EXECUTABLE", &python_executable);
    cfg.define("Python3_FIND_STRATEGY", "LOCATION");

    let dst = cfg.build();

    println!("cargo:rustc-link-search=native={}", dst.display());
    println!("cargo:rustc-link-lib=static=datadog_mem_profiler");

    if env::var("CARGO_CFG_TARGET_OS").unwrap() == "macos" {
        println!("cargo:rustc-link-lib=static=c++");
        println!("cargo:rustc-link-arg=-undefined");
        println!("cargo:rustc-link-arg=dynamic_lookup");
    } else {
        println!("cargo:rustc-link-lib=static=stdc++");
    }
}

/// Tell the crate which CPython it is being built against.
///
/// `pyo3_build_config::use_pyo3_cfgs` gives us cumulative `Py_3_x` cfgs, which
/// cannot express "exactly 3.13". The profiler needs exactness, because a
/// `_Py_DebugOffsets` mirror is only valid for the minor version it was
/// transcribed from, so emit a dedicated cfg per supported version plus the
/// version itself for diagnostics.
fn emit_target_python_cfgs() {
    let version = pyo3_build_config::get().version();

    for minor in SUPPORTED_MINORS {
        println!("cargo::rustc-check-cfg=cfg(pyroscope_py_minor_{minor})");
    }

    if version.major == 3 && SUPPORTED_MINORS.contains(&version.minor) {
        println!("cargo::rustc-cfg=pyroscope_py_minor_{}", version.minor);
    }

    println!("cargo::rustc-env=PYROSCOPE_PY_MAJOR={}", version.major);
    println!("cargo::rustc-env=PYROSCOPE_PY_MINOR={}", version.minor);

    // Free-threaded builds are rejected in the crate itself, via the
    // `Py_GIL_DISABLED` cfg that `use_pyo3_cfgs` emits. See the
    // `compile_error!` in `src/memalloc/mod.rs`.
}

fn rerun_if_native_sources_changed(manifest_dir: &Path, cpp_dir: &Path) {
    for source in NATIVE_SOURCES {
        let path = cpp_dir.join(source);
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let ffi_header = manifest_dir.join("include/pyroscope_ffi.h");
    println!("cargo:rerun-if-changed={}", ffi_header.display());
}
