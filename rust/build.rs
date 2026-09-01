use cmake::Config;
use std::env;
use std::path::{Path, PathBuf};

const MEMALLOC_SOURCES: &[&str] = &[
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

const GCP_SOURCES: &[&str] = &[
    "CMakeLists.txt",
    "bridge.cc",
    "bridge.h",
    "clock.cc",
    "clock.h",
    "log.cc",
    "log.h",
    "populate_frames.cc",
    "populate_frames.h",
    "profiler.cc",
    "profiler.h",
    "stacktraces.cc",
    "stacktraces.h",
];

fn main() {
    if cfg!(not(any(feature = "memory", feature = "gcp"))) {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    if cfg!(feature = "memory") {
        build_memalloc(&manifest_dir, &out_dir);
    }
    if cfg!(feature = "gcp") {
        build_gcp(&manifest_dir, &out_dir);
    }

    if env::var("CARGO_CFG_TARGET_OS").unwrap() == "macos" {
        println!("cargo:rustc-link-lib=static=c++");
        println!("cargo:rustc-link-arg=-undefined");
        println!("cargo:rustc-link-arg=dynamic_lookup");
    } else {
        println!("cargo:rustc-link-lib=static=stdc++");
    }
}

fn configure_python(cfg: &mut Config) {
    println!("cargo:rerun-if-env-changed=Python3_ROOT_DIR");
    let python_root = env::var_os("Python3_ROOT_DIR")
        .expect("Python3_ROOT_DIR must be set so native profilers use the target Python");
    cfg.define("Python3_ROOT_DIR", &python_root);
    println!("cargo:rerun-if-env-changed=Python3_EXECUTABLE");
    let python_executable = env::var_os("Python3_EXECUTABLE")
        .expect("Python3_EXECUTABLE must be set so native profilers use the target Python");
    cfg.define("Python3_EXECUTABLE", &python_executable);
    cfg.define("Python3_FIND_STRATEGY", "LOCATION");
}

fn build_memalloc(manifest_dir: &Path, out_dir: &Path) {
    let cpp_dir = manifest_dir.join("../cpp").canonicalize().unwrap();
    rerun_if_sources_changed(&cpp_dir, MEMALLOC_SOURCES);

    let mut cfg = Config::new(&cpp_dir);
    configure_python(&mut cfg);
    cfg.out_dir(out_dir.join("memalloc"));

    let dst = cfg.build();

    println!("cargo:rustc-link-search=native={}", dst.display());
    println!("cargo:rustc-link-lib=static=datadog_mem_profiler_bundled");

    let ffi_header = manifest_dir.join("include/pyroscope_ffi.h");
    println!("cargo:rerun-if-changed={}", ffi_header.display());
}

fn build_gcp(manifest_dir: &Path, out_dir: &Path) {
    let gcp_dir = manifest_dir.join("../gcp").canonicalize().unwrap();
    rerun_if_sources_changed(&gcp_dir, GCP_SOURCES);
    let ffi_header = manifest_dir.join("include/pyroscope_ffi.h");
    println!("cargo:rerun-if-changed={}", ffi_header.display());

    let mut cfg = Config::new(&gcp_dir);
    configure_python(&mut cfg);
    cfg.out_dir(out_dir.join("gcp"));

    let dst = cfg.build();

    println!("cargo:rustc-link-search=native={}", dst.display());
    println!("cargo:rustc-link-lib=static=gcp_cpu_profiler");
}

fn rerun_if_sources_changed(source_dir: &Path, sources: &[&str]) {
    for source in sources {
        let path = source_dir.join(source);
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
