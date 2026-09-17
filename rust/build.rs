/// CPython minor versions we have a transcribed `_Py_DebugOffsets` mirror for.
///
/// Emitting a cfg only for these makes the mirror selection fail closed: a
/// newer CPython gets no cfg, so the profiler reports the version as
/// unsupported instead of reading one layout out of another.
const SUPPORTED_MINORS: &[u8] = &[13, 14];

fn main() {
    // Declares the `Py_3_*` cfgs. Referencing an undeclared cfg is an
    // `unexpected_cfgs` warning, which `--deny warnings` turns into an error,
    // so this has to run in every feature configuration.
    pyo3_build_config::use_pyo3_cfgs();
    emit_target_python_cfgs();
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
