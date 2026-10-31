use cmake::Config;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let cpp_dir = manifest_dir.join("../dd-trace-py");
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

    generate_unwind_layout(&manifest_dir, Path::new(&python_executable));

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

/// Compiles and runs `unwind_offsets.c` against the target Python's headers. The program runs on
/// the build machine, so the target must be able to run there.
fn generate_unwind_layout(manifest_dir: &Path, python_executable: &Path) {
    let source = manifest_dir.join("unwind_offsets.c");
    println!("cargo:rerun-if-changed={}", source.display());

    let output = Command::new(python_executable)
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_paths()['include'])",
        ])
        .output()
        .expect("failed to run Python3_EXECUTABLE");
    assert!(
        output.status.success(),
        "failed to query the include dir of {}: {}",
        python_executable.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let include_dir = String::from_utf8(output.stdout).unwrap();
    let include_dir = include_dir.trim();

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let program = out_dir.join("unwind_offsets");
    let status = cc::Build::new()
        .get_compiler()
        .to_command()
        .arg("-I")
        .arg(include_dir)
        .arg("-o")
        .arg(&program)
        .arg(&source)
        .status()
        .expect("failed to run the C compiler");
    assert!(status.success(), "failed to compile {}", source.display());

    let output = Command::new(&program)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", program.display()));
    assert!(output.status.success(), "{} failed", program.display());
    std::fs::write(out_dir.join("unwind_layout.rs"), output.stdout)
        .expect("failed to write unwind_layout.rs");
}

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
