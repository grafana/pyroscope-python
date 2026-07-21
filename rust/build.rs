use cmake::Config;
use std::env;
use std::path::{Path, PathBuf};

const NATIVE_SOURCES: &[&str] = &[
    "CMakeLists.txt",
    "Pyroscope.h",
    "_memalloc.cpp",
    "_memalloc_debug.h",
    "_memalloc_frame.h",
    "_memalloc_gc_guard.hpp",
    "_memalloc_heap.cpp",
    "_memalloc_heap.h",
    "_memalloc_reentrant.cpp",
    "_memalloc_reentrant.h",
    "_memalloc_tb.cpp",
    "_memalloc_tb.h",
    "_pymacro.h",
    "profiling_helpers/frame_accessors.h",
    "profiling_helpers/linetable_parser.h",
    "profiling_helpers/version_compat.h",
];

fn main() {
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
