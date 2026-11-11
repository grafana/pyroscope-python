//! Rust -> Python stack shape -> synchronous Rust Criterion loop.
//! Pass --profile-time <seconds> to collect native CPU profiles instead of timings.

use std::ffi::CString;
use std::hint::black_box;

use criterion::{Criterion, Throughput};

use pyo3::prelude::*;
use pyroscope_python_unwind::{self as unwind, Frame, MAX_FRAMES};

use pyroscope_pprof::{StringTable, ffi::FFIFrame};
use pyroscope_python_resolve::{Lines, PYO3Types, Resolver};

#[path = "support/profiling.rs"]
mod profiling;

#[pyclass(unsendable)]
struct Benchmarks {
    criterion: Criterion,
    completed: bool,
}

#[pymethods]
impl Benchmarks {
    fn run(&mut self, py: Python<'_>, expected: Vec<String>) -> PyResult<()> {
        assert!(!self.completed, "Python must call Rust exactly once");
        // Capture CPython's own complete root-to-leaf stack, including current line numbers,
        // outside the timed region. No Python bytecode runs again until all loops finish.
        let name_attribute = if py.version_info().minor >= 11 {
            "co_qualname"
        } else {
            "co_name"
        };
        let mut frame = py.import("sys")?.getattr("_getframe")?.call0()?;
        let mut python_stack = Vec::new();
        while !frame.is_none() {
            let code = frame.getattr("f_code")?;
            python_stack.push((
                code.getattr(name_attribute)?.extract::<String>()?,
                code.getattr("co_filename")?.extract::<String>()?,
                frame.getattr("f_lineno")?.extract::<i32>()?,
            ));
            frame = frame.getattr("f_back")?;
        }
        python_stack.reverse();

        let mut resolver = Resolver::<PYO3Types>::new(Lines::LastInstruction);
        let mut strings = StringTable::new();
        let mut buffer = [Frame::default(); MAX_FRAMES];
        let captured = unwind::unwind(&mut buffer).expect("preflight unwind failed");
        let resolved = resolver
            .resolve_stack(captured, 1, &mut strings)
            .expect("preflight resolution failed");
        validate_stack(&expected, &python_stack, resolved, &strings);
        eprintln!("Validated mixed stack: {} Python frames", captured.len());

        // Resolve-only reuses one real stack. The active Python calls retain every code
        // object until this callback returns. Resolver scratch storage is warmed by preflight.
        let captured = captured.to_vec();
        let mut group = self.criterion.benchmark_group("mixed");
        group.throughput(Throughput::Elements(1)); // One complete stack per iteration.
        group.bench_function("unwind", |b| {
            b.iter(|| {
                let frames = unwind::unwind(black_box(&mut buffer)).expect("unwind failed");
                black_box(frames);
            });
        });
        group.bench_function("resolve", |b| {
            b.iter(|| {
                black_box(
                    resolver
                        .resolve_stack(black_box(&captured), 1, &mut strings)
                        .expect("resolution failed"),
                );
            });
        });
        group.bench_function("unwind_resolve", |b| {
            b.iter(|| {
                let frames = unwind::unwind(black_box(&mut buffer)).expect("unwind failed");
                black_box(
                    resolver
                        .resolve_stack(black_box(frames), 1, &mut strings)
                        .expect("resolution failed"),
                );
            });
        });
        group.finish();
        self.completed = true;
        Ok(())
    }
}

fn validate_stack(
    expected: &[String],
    python_stack: &[(String, String, i32)],
    resolved: &[FFIFrame],
    strings: &StringTable,
) {
    let mut table = Vec::new();
    strings.clone_pprof_table(&mut table);
    let observed: Vec<_> = resolved
        .iter()
        .rev()
        .map(|f| {
            (
                table[f.function_name.index as usize].clone(),
                table[f.file_name.index as usize].clone(),
                f.line,
            )
        })
        .collect();
    let names: Vec<_> = observed.iter().map(|(name, _, _)| name).collect();
    assert_eq!(
        names,
        expected.iter().collect::<Vec<_>>(),
        "wrong complete root-to-leaf stack; full profile: {observed:#?}"
    );
    assert_eq!(
        observed, python_stack,
        "frames/lines differ from CPython; full profile: {observed:#?}"
    );
}

fn main() -> PyResult<()> {
    let criterion = Criterion::default()
        .with_profiler(profiling::Pprof::default())
        .configure_from_args();
    initialize_python();
    Python::attach(|py| {
        let version = py.version_info();
        assert!(
            version.major == 3 && (10..=14).contains(&version.minor),
            "signal stack benchmarks require CPython 3.10–3.14"
        );
        assert_eq!((version.major, version.minor), unwind::PY_VERSION);
        eprintln!("Benchmarking CPython {}", Python::version_str());

        // SAFETY: launch this standalone process with recovery signals unblocked. It
        // installs no competing recovery handlers. Python remains attached on this
        // thread and retains the source objects throughout each loop.
        unsafe { kindasafe_init::init() }.expect("failed to initialize fault recovery");

        let source = CString::new(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../scripts/benchmarks/stack_shape.py"
        )))
        .unwrap();
        let module = PyModule::from_code(py, &source, c"stack_shape.py", c"stack_shape")?;
        let bench = Py::new(
            py,
            Benchmarks {
                criterion,
                completed: false,
            },
        )?;
        module.getattr("mixed")?.call1((&bench,))?;
        let bench = bench.borrow(py);
        assert!(bench.completed, "Python must call Rust exactly once");
        bench.criterion.final_summary();
        Ok(())
    })
}

fn initialize_python() {
    // Use the interpreter selected at build time, including its standard library. Searching
    // PATH here can pick a different Python and fails with relocatable uv/pyenv installs.
    let executable = CString::new(env!("SIGNAL_BENCH_PYTHON")).unwrap();
    // SAFETY: main calls this exactly once before attaching or using any other Python API.
    // CPython initializes/owns the config strings; clear them only after initialization.
    unsafe {
        let mut config = std::mem::MaybeUninit::uninit();
        pyo3::ffi::PyConfig_InitIsolatedConfig(config.as_mut_ptr());
        let mut config = config.assume_init();
        config.install_signal_handlers = 0;
        let status = pyo3::ffi::PyConfig_SetBytesString(
            &raw mut config,
            &raw mut config.program_name,
            executable.as_ptr(),
        );
        if pyo3::ffi::PyStatus_Exception(status) != 0 {
            pyo3::ffi::Py_ExitStatusException(status);
        }
        let status = pyo3::ffi::Py_InitializeFromConfig(&config);
        if pyo3::ffi::PyStatus_Exception(status) != 0 {
            pyo3::ffi::Py_ExitStatusException(status);
        }
        pyo3::ffi::PyConfig_Clear(&raw mut config);
        // Match PyO3's initialization protocol: release the GIL before Python::attach.
        pyo3::ffi::PyEval_SaveThread();
    }
}
