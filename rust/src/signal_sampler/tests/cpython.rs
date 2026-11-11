use pyo3::{exceptions::PyAssertionError, prelude::*, types::PyModule, wrap_pyfunction};
use pyroscope_pprof::{StringTable, ffi::FFIFrame};
use pyroscope_python_resolve::{Lines, PYO3Types, Resolver};
use pyroscope_python_unwind::{self as unwind, Frame, MAX_FRAMES};
use std::{
    ffi::CString,
    sync::atomic::{AtomicUsize, Ordering},
};

const SOURCE: &str = include_str!("../../../../scripts/tests/resolve_stacks.py");
const FOREIGN_SOURCE: &str = "def leaf():\n    return assert_stack(expected)\n";
const FOREIGN_FILENAME: &str = "foreign_🐍.py";
static STACKS_CHECKED: AtomicUsize = AtomicUsize::new(0);

// Root-to-leaf name, filename, first line, and current line, supplied by the test.
type ExpectedFrame = (String, String, i32, i32);

#[pyfunction(signature = (expected, count = 1))]
fn assert_stack(expected: Vec<ExpectedFrame>, count: usize) -> PyResult<usize> {
    let mut buffer = [Frame::default(); MAX_FRAMES];
    let raw = unwind::unwind(&mut buffer)
        .map_err(|error| PyAssertionError::new_err(format!("unwind failed: {error:?}")))?;
    for mode in [Lines::LastInstruction, Lines::First, Lines::NoLine] {
        let mut strings = StringTable::new();
        let mut resolver = Resolver::<PYO3Types>::new(mode);
        let resolved = resolver
            .resolve_stack(raw, count, &mut strings)
            .map_err(|error| {
                PyAssertionError::new_err(format!("{mode:?}: {error:?}; raw stack: {raw:?}"))
            })?;
        let mut table = Vec::new();
        strings.clone_pprof_table(&mut table);
        let actual: Vec<_> = resolved
            .iter()
            .rev()
            .map(|frame| {
                (
                    table[frame.function_name.index as usize].clone(),
                    table[frame.file_name.index as usize].clone(),
                    frame.line,
                )
            })
            .collect();
        let wanted: Vec<_> = expected
            .iter()
            .map(|(name, filename, first, current)| {
                (
                    name.clone(),
                    filename.clone(),
                    match mode {
                        Lines::LastInstruction => *current,
                        Lines::First => *first,
                        Lines::NoLine => 0,
                    },
                )
            })
            .collect();
        if actual != wanted {
            return Err(PyAssertionError::new_err(format!(
                "{mode:?}:\nexpected root-to-leaf stack: {wanted:#?}\n\
                 actual root-to-leaf stack: {actual:#?}\nraw stack: {raw:#?}"
            )));
        }
        if count > 1 {
            assert_profile_weight(resolved, strings, &wanted, count)?;
        }
    }
    STACKS_CHECKED.fetch_add(1, Ordering::Relaxed);
    Ok(1)
}

fn assert_profile_weight(
    frames: &[FFIFrame],
    strings: StringTable,
    expected: &[(String, String, i32)],
    count: usize,
) -> PyResult<()> {
    let profile =
        crate::signal_backend::tests::profile_for_resolved_stack(frames, strings, &[2, count - 2]);
    let actual: Vec<_> = profile
        .sample
        .iter()
        .map(|sample| {
            let stack: Vec<_> = sample
                .location_id
                .iter()
                .rev()
                .flat_map(|id| {
                    let location = profile
                        .location
                        .iter()
                        .find(|location| location.id == *id)
                        .unwrap();
                    location.line.iter().map(|line| {
                        let function = profile
                            .function
                            .iter()
                            .find(|function| function.id == line.function_id)
                            .unwrap();
                        (
                            profile.string_table[function.name as usize].as_str(),
                            profile.string_table[function.filename as usize].as_str(),
                            line.line,
                        )
                    })
                })
                .collect();
            (stack, sample.value.clone())
        })
        .collect();
    let stack: Vec<_> = expected
        .iter()
        .map(|(name, filename, line)| (name.as_str(), filename.as_str(), i64::from((*line).max(0))))
        .collect();
    // Two identical weighted samples must aggregate to the full count at 100 Hz.
    let wanted = vec![(stack, vec![count as i64 * 10_000_000])];
    if actual != wanted {
        return Err(PyAssertionError::new_err(format!(
            "expected profile stacks and weights: {wanted:#?}\nactual: {actual:#?}\nfull profile: {profile:#?}"
        )));
    }
    Ok(())
}

fn source_line(marker: &str) -> i32 {
    let lines: Vec<_> = SOURCE
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            line.split_once("# ")
                .is_some_and(|(_, label)| label == marker)
        })
        .map(|(index, _)| index as i32 + 1)
        .collect();
    assert_eq!(lines.len(), 1, "expected one source marker {marker:?}");
    lines[0]
}

fn expected_frame(name: &str, filename: &str, first: &str, current: &str) -> ExpectedFrame {
    // CPython 3.10 has co_name; qualified code names start with 3.11.
    #[cfg(not(Py_3_11))]
    let name = name.rsplit('.').next().unwrap();
    (
        name.to_owned(),
        filename.to_owned(),
        source_line(first),
        source_line(current),
    )
}

fn module<'py>(py: Python<'py>, source: &str, filename: &str, name: &str) -> Bound<'py, PyModule> {
    let module = PyModule::from_code(
        py,
        &CString::new(source).unwrap(),
        &CString::new(filename).unwrap(),
        &CString::new(name).unwrap(),
    )
    .unwrap();
    module
        .add_function(wrap_pyfunction!(assert_stack, &module).unwrap())
        .unwrap();
    module
}

#[test]
fn resolves_real_cpython_stack_shapes() {
    let _guard = crate::signal_sampler::METRICS_LOCK.lock().unwrap();
    Python::initialize();
    // SAFETY: these tests preserve recovery handlers throughout all walks.
    unsafe { kindasafe_init::init() }.unwrap();
    Python::attach(|py| {
        let foreign = module(py, FOREIGN_SOURCE, FOREIGN_FILENAME, "resolver_foreign");
        for (index, filename) in ["frames.py", "frames_é.py", "frames_中.py", "frames_🐍.py"]
            .into_iter()
            .enumerate()
        {
            let module = module(py, SOURCE, filename, &format!("resolver_shapes_{index}"));
            module.setattr("foreign", &foreign).unwrap();
            let frame = |name: &str, marker: &str| {
                expected_frame(
                    name,
                    filename,
                    &format!("first: {marker}"),
                    &format!("call: {marker}"),
                )
            };
            let mut comprehension = vec![frame("comprehension", "comprehension")];
            // PEP 709 inlines list comprehensions from CPython 3.12 onward.
            if !cfg!(Py_3_12) {
                comprehension.push(expected_frame(
                    "comprehension.<locals>.<listcomp>",
                    filename,
                    "call: comprehension",
                    "call: comprehension",
                ));
            }
            let cases = [
                ("direct", vec![frame("direct", "direct")]),
                (
                    "nested",
                    vec![
                        frame("nested", "nested"),
                        frame("nested.<locals>.outer", "outer"),
                        frame("nested.<locals>.outer.<locals>.inner", "inner"),
                    ],
                ),
                (
                    "methods",
                    vec![
                        frame("methods", "methods"),
                        frame("Café.café", "café"),
                        frame("Café.中文", "中文"),
                        frame("Café.𐐀", "𐐀"),
                    ],
                ),
                (
                    "generators",
                    vec![
                        frame("generators", "generators"),
                        frame("generator_outer", "generator_outer"),
                        frame("generator_leaf", "generator_leaf"),
                    ],
                ),
                (
                    "coroutines",
                    vec![
                        frame("coroutines", "coroutines"),
                        frame("coroutine_outer", "coroutine_outer"),
                        frame("coroutine_leaf", "coroutine_leaf"),
                    ],
                ),
                (
                    "empty_lines",
                    vec![
                        frame("empty_lines", "empty_lines"),
                        (
                            "empty_lines_leaf".into(),
                            filename.into(),
                            source_line("first: empty_lines_leaf"),
                            -1,
                        ),
                    ],
                ),
                (
                    "unmapped_lines",
                    vec![
                        frame("unmapped_lines", "unmapped_lines"),
                        (
                            "unmapped_lines_middle".into(),
                            filename.into(),
                            source_line("first: unmapped_lines_middle"),
                            -1,
                        ),
                        frame("mapped_lines_leaf", "mapped_lines_leaf"),
                    ],
                ),
                ("comprehension", comprehension),
                (
                    "c_reentry",
                    vec![
                        frame("c_reentry", "c_reentry"),
                        frame("map_leaf", "map_leaf"),
                    ],
                ),
                (
                    "exceptions",
                    vec![frame("exceptions", "exceptions"), frame("direct", "direct")],
                ),
                (
                    "cross_module",
                    vec![
                        frame("cross_module", "cross_module"),
                        ("leaf".into(), FOREIGN_FILENAME.into(), 1, 2),
                    ],
                ),
            ];
            for (entry, expected) in cases {
                module.setattr("expected", expected.clone()).unwrap();
                foreign.setattr("expected", expected).unwrap();
                let before = STACKS_CHECKED.load(Ordering::Relaxed);
                let result = module
                    .getattr(entry)
                    .unwrap()
                    .call0()
                    .unwrap_or_else(|error| panic!("{filename}: {entry}: {error}"));
                assert_eq!(result.extract::<usize>().unwrap(), 1, "{filename}: {entry}");
                assert_eq!(
                    STACKS_CHECKED.load(Ordering::Relaxed),
                    before + 1,
                    "{filename}: {entry}"
                );
            }
            for depth in [0, 4, 32] {
                let mut expected = vec![frame("recursive", "recursive"); depth];
                expected.push(expected_frame(
                    "recursive",
                    filename,
                    "first: recursive",
                    "leaf: recursive",
                ));
                module.setattr("expected", expected).unwrap();
                let before = STACKS_CHECKED.load(Ordering::Relaxed);
                let result = module
                    .getattr("recursive")
                    .unwrap()
                    .call1((depth,))
                    .unwrap_or_else(|error| panic!("{filename}: recursive({depth}): {error}"));
                assert_eq!(
                    result.extract::<usize>().unwrap(),
                    1,
                    "{filename}: recursive({depth})"
                );
                assert_eq!(
                    STACKS_CHECKED.load(Ordering::Relaxed),
                    before + 1,
                    "{filename}: recursive({depth})"
                );
            }
        }
    });
}
