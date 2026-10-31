use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    pyo3_build_config::use_pyo3_cfgs();
    println!("cargo:rerun-if-env-changed=Python3_EXECUTABLE");
    let version = pyo3_build_config::get().version();
    if version.major != 3 || !(11..=15).contains(&version.minor) {
        return; // No layouts on 3.10; lib.rs rejects other unsupported versions.
    }
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let python = env::var_os("Python3_EXECUTABLE").expect(
        "Python3_EXECUTABLE must identify the target Python interpreter (passed from setup.py)",
    );
    check_python_layouts(&manifest_dir, Path::new(&python));
}

const UPDATE_PYTHON_LAYOUTS: &str = "PYROSCOPE_UPDATE_PYTHON_LAYOUTS";

/// Checks the layouts in `src/{unwind,resolve}/`, which are selected by
/// pyo3's `Py_3_*` cfgs, against the offsets `python_offsets.c` reads from the target Python's
/// headers. With `PYROSCOPE_UPDATE_PYTHON_LAYOUTS=1` it writes them instead. The program runs on
/// the build machine, so the target must be able to run there.
fn check_python_layouts(manifest_dir: &Path, python_executable: &Path) {
    let version = pyo3_build_config::get().version();
    if version.major != 3 || !(11..=15).contains(&version.minor) {
        return;
    }
    let source = manifest_dir.join("python_offsets.c");
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-env-changed={UPDATE_PYTHON_LAYOUTS}");
    let update = env::var_os(UPDATE_PYTHON_LAYOUTS).is_some_and(|v| v == "1");

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

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap()).join("python_layout");
    std::fs::create_dir_all(&out_dir).unwrap();
    let program = out_dir.join("python_offsets");
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

    let status = Command::new(&program)
        .arg(&out_dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", program.display()));
    assert!(status.success(), "{} failed", program.display());

    for module in ["unwind", "resolve"] {
        let generated_path = out_dir.join(format!("{module}.rs"));
        let checked_in_path = manifest_dir.join(format!(
            "src/{module}/v{}_{}.rs",
            version.major, version.minor
        ));
        println!("cargo:rerun-if-changed={}", checked_in_path.display());
        let generated = std::fs::read_to_string(&generated_path).unwrap();
        if update {
            std::fs::write(&checked_in_path, &generated).unwrap();
            continue;
        }
        let checked_in = std::fs::read_to_string(&checked_in_path).unwrap_or_default();
        assert!(
            checked_in == generated,
            "{} does not match the headers of {} in {include_dir}:\n{generated}\n\
             Rerun the build with {UPDATE_PYTHON_LAYOUTS}=1 to update it.",
            checked_in_path.display(),
            python_executable.display(),
        );
    }
}
