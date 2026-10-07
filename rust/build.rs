use cmake::Config;
use std::env;
use std::path::{Path, PathBuf};

const NATIVE_SOURCES: &[&str] = &[
    "CMakeLists.txt",
    "BundleStaticLibrary.cmake",
    "pyroscope/Pyroscope.h",
    "ddtrace/profiling/collector/_memalloc.cpp",
    "ddtrace/profiling/collector/_memalloc_debug.h",
    "ddtrace/profiling/collector/_memalloc_frame.h",
    "ddtrace/profiling/collector/_memalloc_gc_guard.hpp",
    "ddtrace/profiling/collector/_memalloc_heap.cpp",
    "ddtrace/profiling/collector/_memalloc_heap.h",
    "ddtrace/profiling/collector/_memalloc_reentrant.cpp",
    "ddtrace/profiling/collector/_memalloc_reentrant.h",
    "ddtrace/profiling/collector/_memalloc_tb.cpp",
    "ddtrace/profiling/collector/_memalloc_tb.h",
    "ddtrace/profiling/collector/_pymacro.h",
    "ddtrace/internal/datadog/profiling/profiling_helpers/frame_accessors.h",
    "ddtrace/internal/datadog/profiling/profiling_helpers/linetable_parser.h",
    "ddtrace/internal/datadog/profiling/profiling_helpers/version_compat.h",
];

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let cpp_dir = manifest_dir.join("../dd-trace-py");
    let cpp_dir = cpp_dir.canonicalize().unwrap();

    rerun_if_native_sources_changed(&manifest_dir, &cpp_dir);

    let mut cfg = Config::new(&cpp_dir);
    cfg.define("PYROSCOPE_FFI_INCLUDE_DIR", manifest_dir.join("include"));

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
    println!("cargo:rustc-link-lib=static=datadog_mem_profiler_bundled");

    if env::var("CARGO_CFG_TARGET_OS").unwrap() == "macos" {
        println!("cargo:rustc-link-lib=static=c++");
        println!("cargo:rustc-link-arg=-undefined");
        println!("cargo:rustc-link-arg=dynamic_lookup");
    } else {
        println!("cargo:rustc-link-lib=static=stdc++");
    }
}

fn rerun_if_native_sources_changed(manifest_dir: &Path, cpp_dir: &Path) {
    for source in NATIVE_SOURCES {
        let path = cpp_dir.join(source);
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let ffi_header = manifest_dir.join("include/pyroscope_ffi.h");
    println!("cargo:rerun-if-changed={}", ffi_header.display());
}
