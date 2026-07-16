mod pyspy_backend;
// mod mem;

// Re-exports structs
pub use crate::pyroscope::PyroscopeAgent;
pub use error::{PyroscopeError, Result};

pub mod backend;
pub mod encode;
pub mod error;
pub mod pyroscope;
pub mod session;

mod utils;
pub use utils::ThreadId;
pub mod ffikit;

use std::{
    collections::HashMap,
    ffi::CString,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::backend::{BackendConfig, BackendImpl, Tag, ThreadTagsSet};
use crate::pyroscope::PyroscopeAgentBuilder;
use crate::pyspy_backend::Pyspy;
use pyo3::exceptions::PyDeprecationWarning;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::wrap_pyfunction;

const PYSPY_NAME: &str = "pyspy";
const PYSPY_VERSION: &str = env!("CARGO_PKG_VERSION");

static AGENT_RUNNING: AtomicBool = AtomicBool::new(false);

#[pyfunction]
fn warn_about_fork(py: Python<'_>) -> PyResult<()> {
    if !AGENT_RUNNING.load(Ordering::Acquire) {
        return Ok(());
    }

    // The native agent starts threads and is not safe to inherit across a fork.
    // See https://github.com/grafana/pyroscope-python/issues/122.
    let message = CString::new(format!(
        "This process (pid={}) is running Pyroscope, use of fork() may lead to \
         deadlocks in the child. Forking after Pyroscope starts is unsupported; \
         configure Pyroscope after forking or call pyroscope.shutdown() before forking.",
        std::process::id()
    ))
    .expect("fork warning does not contain null bytes");

    PyErr::warn(py, &py.get_type::<PyDeprecationWarning>(), &message, 2)
}

fn register_fork_warning(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let register_at_fork = py.import("os")?.getattr("register_at_fork")?;

    let kwargs = PyDict::new(py);
    kwargs.set_item("after_in_parent", wrap_pyfunction!(warn_about_fork, m)?)?;
    register_at_fork.call((), Some(&kwargs))?;
    Ok(())
}

#[pyfunction]
fn initialize_logging(logging_level: u32) -> bool {
    // Force rustc to display the log messages in the console.
    match logging_level {
        50 => {
            unsafe { std::env::set_var("RUST_LOG", "off") };
        }
        40 => {
            unsafe { std::env::set_var("RUST_LOG", "error") };
        }
        30 => {
            unsafe { std::env::set_var("RUST_LOG", "warn") };
        }
        20 => {
            unsafe { std::env::set_var("RUST_LOG", "info") };
        }
        10 => {
            unsafe { std::env::set_var("RUST_LOG", "debug") };
        }
        _ => {
            unsafe { std::env::set_var("RUST_LOG", "debug") };
        }
    }

    // Initialize the logger.
    pretty_env_logger::init_timed();
    true
}

#[allow(clippy::too_many_arguments)]
#[pyfunction]
fn initialize_agent(
    application_name: String,
    server_address: String,
    basic_auth_username: String,
    basic_auth_password: String,
    sample_rate: u32,
    oncpu: bool,
    gil_only: bool,
    report_pid: bool,
    report_thread_id: bool,
    report_thread_name: bool,
    runtime_name: String,
    runtime_version: String,
    tags: HashMap<String, String>,
    tenant_id: String,
    http_headers: HashMap<String, String>,
    line_no: LineNo,
) -> bool {
    let pid = std::process::id();

    let backend_config = BackendConfig {
        report_thread_id,
        report_thread_name,
        report_pid,
    };

    let pid = pid.try_into().unwrap();

    let config = py_spy::Config {
        blocking: py_spy::config::LockingStrategy::NonBlocking,
        native: false,
        pid: Some(pid),
        sampling_rate: sample_rate.into(),
        include_idle: !oncpu,
        include_thread_ids: true,
        subprocesses: false,
        gil_only,
        lineno: line_no.into(),
        duration: py_spy::config::RecordDuration::Unlimited,
        ..py_spy::Config::default()
    };

    let dynamic_tags = ThreadTagsSet::new();

    let pyspy = BackendImpl::new(Box::new(Pyspy::new(
        config,
        backend_config,
        dynamic_tags.clone(),
    )));

    let mut agent_builder = pyroscope::PyroscopeConfig::new(
        server_address,
        application_name,
        sample_rate,
        PYSPY_NAME,
        PYSPY_VERSION,
        // mem::Config {
        //     enabled: mem_enabled,
        //     enable_mem_domain: mem_enable_mem_domain,
        //     max_nframe: mem_max_nframe,
        //     heap_sample_size: mem_heap_sample_size,
        // },
    )
    .tags(tags)
    .runtime(runtime_name, runtime_version);

    if !basic_auth_username.is_empty() && !basic_auth_password.is_empty() {
        agent_builder = agent_builder.basic_auth(basic_auth_username, basic_auth_password);
    }
    if !tenant_id.is_empty() {
        agent_builder = agent_builder.tenant_id(tenant_id);
    }
    agent_builder = agent_builder.http_headers(http_headers);

    // mem::start(&pyroscope_config.mem_config);
    let started = ffikit::run(PyroscopeAgentBuilder::new(
        agent_builder,
        pyspy,
        dynamic_tags,
    ))
    .is_ok();
    if started {
        AGENT_RUNNING.store(true, Ordering::Release);
    }
    started
}

#[pyfunction]
fn drop_agent() -> bool {
    let dropped = ffikit::stop().is_ok();
    if dropped {
        AGENT_RUNNING.store(false, Ordering::Release);
    }
    dropped
}

#[pyfunction]
fn add_thread_tag(key: String, value: String) -> bool {
    ffikit::add_thread_tag(self_thread_id(), Tag { key, value }).is_ok()
}

#[pyfunction]
fn remove_thread_tag(key: String, value: String) -> bool {
    ffikit::remove_thread_tag(self_thread_id(), Tag { key, value }).is_ok()
}

#[pyclass(eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineNo {
    LastInstruction = 0,
    First = 1,
    NoLine = 2,
}

impl From<LineNo> for py_spy::config::LineNo {
    fn from(val: LineNo) -> Self {
        match val {
            LineNo::LastInstruction => py_spy::config::LineNo::LastInstruction,
            LineNo::First => py_spy::config::LineNo::First,
            LineNo::NoLine => py_spy::config::LineNo::NoLine,
        }
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<LineNo>()?;
    m.add_function(wrap_pyfunction!(initialize_logging, m)?)?;
    m.add_function(wrap_pyfunction!(initialize_agent, m)?)?;
    m.add_function(wrap_pyfunction!(drop_agent, m)?)?;
    m.add_function(wrap_pyfunction!(add_thread_tag, m)?)?;
    m.add_function(wrap_pyfunction!(remove_thread_tag, m)?)?;
    register_fork_warning(m)?;
    Ok(())
}

pub fn self_thread_id() -> ThreadId {
    // https://github.com/python/cpython/blob/main/Python/thread_pthread.h#L304
    ThreadId::pthread_self()
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_cargo_version_matches_python_version() {
        let cargo_version = env!("CARGO_PKG_VERSION");
        let pyproject = include_str!("../../pyproject.toml");
        let python_version = pyproject
            .lines()
            .find_map(|line| {
                let line = line.trim();
                if line.starts_with("version") && line.contains('=') {
                    let start = line.find('"')?;
                    let end = line.rfind('"')?;
                    if start < end {
                        Some(&line[start + 1..end])
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .expect("could not find version in pyproject.toml");

        assert_eq!(
            cargo_version, python_version,
            "Cargo.toml version ({cargo_version}) does not match pyproject.toml ({python_version})"
        );
    }
}
