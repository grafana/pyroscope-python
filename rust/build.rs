use cmake::Config;
use std::env;
use std::path::{Path, PathBuf};

fn main() {
    if cfg!(not(feature = "cpp-profilers")) {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let cpp_dir = manifest_dir.join("../cpp");
    let cpp_dir = cpp_dir.canonicalize().unwrap();

    rerun_if_native_sources_changed(&manifest_dir, &cpp_dir);

    let mut cfg = Config::new(&cpp_dir);

    println!("cargo:rerun-if-env-changed=Python3_ROOT_DIR");
    let python_root = env::var_os("Python3_ROOT_DIR")
        .expect("Python3_ROOT_DIR must be set (passed from setup.py) so the C++ profilers are compiled against the target Python version");
    cfg.define("Python3_ROOT_DIR", &python_root);
    println!("cargo:rerun-if-env-changed=Python3_EXECUTABLE");
    let python_executable = env::var_os("Python3_EXECUTABLE")
        .expect("Python3_EXECUTABLE must be set (passed from setup.py) so the C++ profilers are compiled against the exact target Python interpreter");
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

/// Emit a `rerun-if-changed` for every C++ source under `cpp/`, plus the
/// generated FFI header.
///
/// This walks the tree rather than naming files. A hand-maintained list was
/// tried first and silently rotted: every entry still pointed at a flat
/// `cpp/_memalloc.cpp`-style path after the sources moved into `cpp/memalloc/`
/// and `cpp/pyroscope/`, so cargo watched nothing that existed and C++ edits
/// never triggered a rebuild. Since then `cpp/stack/` and `cpp/dd_wrapper/`
/// arrived too, which a list would also have missed.
///
/// CMake build trees are skipped. In-source `cmake-build-*` directories (what
/// CLion creates) hold generated copies of these same headers plus the fetched
/// abseil checkout; watching them would make every configure look like a source
/// change.
fn rerun_if_native_sources_changed(manifest_dir: &Path, cpp_dir: &Path) {
    let ffi_header = manifest_dir.join("include/pyroscope_ffi.h");
    println!("cargo:rerun-if-changed={}", ffi_header.display());

    emit_rerun_for_dir(cpp_dir);
}

fn emit_rerun_for_dir(dir: &Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => panic!("failed to read {}: {e}", dir.display()),
    };

    for entry in entries {
        let entry = entry.expect("failed to read directory entry");
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if path.is_dir() {
            if name.starts_with("cmake-build") || name.starts_with('.') {
                continue;
            }
            emit_rerun_for_dir(&path);
        } else if name == "CMakeLists.txt"
            || matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("c" | "cc" | "cpp" | "h" | "hpp" | "cmake")
            )
        {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
