use cmake::Config;
use std::env;
use std::path::{Path, PathBuf};

const MEMORY_NATIVE_SOURCES: &[&str] = &[
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

/// The vendored CPU profiler: its CMake project directory under `cpp/cpu/` and
/// the static library that project produces.
struct CpuProject {
    dir: &'static str,
    lib: &'static str,
    /// `--cfg` emitted when this project is actually built.
    /// `rust/src/cpu/native.rs` keys its `extern "C"` declarations off it, so
    /// build.rs is the single source of truth for what is available and the two
    /// cannot drift.
    cfg: &'static str,
    /// Whether this implementation supports the target we are building for.
    supported: fn(&str) -> bool,
}

const CPU_PROJECTS: &[CpuProject] = &[CpuProject {
    dir: "ddtrace_stack",
    lib: "pyroscope_cpu_ddtrace",
    cfg: "pyroscope_cpu_ddtrace",
    // Linux only: thread auto-registration needs the kernel TID and a clock
    // derived from it. See the TODO(macos) in
    // cpp/cpu/ddtrace_stack/src/echion/threads.cc.
    supported: |os| os == "linux",
}];

/// Every cfg build.rs can emit. Kept separate from CPU_PROJECTS because the
/// check-cfg declarations must cover projects skipped on this target too,
/// otherwise `#[cfg(...)]` on them would warn.
const ALL_CPU_CFGS: &[&str] = &["pyroscope_cpu_ddtrace"];

fn main() {
    // Declare every cfg we may emit, so cargo does not warn about unexpected cfgs.
    for name in ALL_CPU_CFGS {
        println!("cargo::rustc-check-cfg=cfg({name})");
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let cpp_dir = manifest_dir.join("../cpp").canonicalize().unwrap();

    let ffi_header = manifest_dir.join("include/pyroscope_ffi.h");
    println!("cargo:rerun-if-changed={}", ffi_header.display());

    let build_memory = cfg!(feature = "memory");
    let build_cpu = cfg!(feature = "cpu_native");
    if !build_memory && !build_cpu {
        return;
    }

    // Both the memalloc profiler and the vendored CPU sampler read
    // version-specific CPython internals, so they must be compiled against the
    // exact interpreter the wheel targets.
    println!("cargo:rerun-if-env-changed=Python3_ROOT_DIR");
    println!("cargo:rerun-if-env-changed=Python3_EXECUTABLE");
    let python_root = env::var_os("Python3_ROOT_DIR")
        .expect("Python3_ROOT_DIR must be set (passed from setup.py) so the native profilers are compiled against the target Python version");
    let python_executable = env::var_os("Python3_EXECUTABLE")
        .expect("Python3_EXECUTABLE must be set (passed from setup.py) so the native profilers are compiled against the exact target Python interpreter");

    let configure = |cfg: &mut Config| {
        cfg.define("Python3_ROOT_DIR", &python_root);
        cfg.define("Python3_EXECUTABLE", &python_executable);
        cfg.define("Python3_FIND_STRATEGY", "LOCATION");
    };

    if build_memory {
        rerun_if_changed(&cpp_dir, MEMORY_NATIVE_SOURCES);

        let mut cfg = Config::new(&cpp_dir);
        configure(&mut cfg);
        let dst = cfg.build();

        println!("cargo:rustc-link-search=native={}", dst.display());
        println!("cargo:rustc-link-lib=static=datadog_mem_profiler_bundled");
    }

    if build_cpu {
        let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
        let cpu_dir = cpp_dir.join("cpu");
        // The vendored tree is large and changes as a unit; watch the whole
        // directory rather than listing every file.
        rerun_if_dir_changed(&cpu_dir);
        // The CPU project also compiles against the shared profiling helpers.
        rerun_if_changed(
            &cpp_dir,
            &[
                "profiling_helpers/frame_accessors.h",
                "profiling_helpers/linetable_parser.h",
                "profiling_helpers/version_compat.h",
            ],
        );

        for project in CPU_PROJECTS {
            if !(project.supported)(&target_os) {
                continue;
            }
            let project_dir = cpu_dir.join(project.dir);
            if !project_dir.join("CMakeLists.txt").exists() {
                panic!(
                    "cpu profiler project {} is enabled for {target_os} but {} is missing",
                    project.dir,
                    project_dir.display()
                );
            }

            let mut cfg = Config::new(&project_dir);
            configure(&mut cfg);
            // Install into its own OUT_DIR subtree so the archives cannot
            // overwrite the memory profiler's.
            cfg.out_dir(out_dir().join(project.dir));
            let dst = cfg.build();

            println!("cargo:rustc-link-search=native={}", dst.display());
            println!("cargo:rustc-link-lib=static={}", project.lib);
            println!("cargo:rustc-cfg={}", project.cfg);
        }
    }

    // C++ runtime, shared by the memory and CPU profilers.
    if env::var("CARGO_CFG_TARGET_OS").unwrap() == "macos" {
        println!("cargo:rustc-link-lib=static=c++");
        println!("cargo:rustc-link-arg=-undefined");
        println!("cargo:rustc-link-arg=dynamic_lookup");
    } else {
        println!("cargo:rustc-link-lib=static=stdc++");
    }
}

fn out_dir() -> PathBuf {
    PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo"))
}

fn rerun_if_changed(base: &Path, sources: &[&str]) {
    for source in sources {
        println!("cargo:rerun-if-changed={}", base.join(source).display());
    }
}

/// Emit a rerun-if-changed for every file under `dir`.
///
/// Cargo only reruns on a directory's own mtime, which does not change when a
/// file inside it is edited, so the tree is walked explicitly.
fn rerun_if_dir_changed(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rerun_if_dir_changed(&path);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
