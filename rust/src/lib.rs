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

use std::collections::HashMap;

use crate::backend::{BackendConfig, BackendImpl, Tag, ThreadTagsSet};
use crate::pyroscope::PyroscopeAgentBuilder;
use crate::pyspy_backend::Pyspy;
use pyo3::prelude::*;
use pyo3::wrap_pyfunction;

const PYSPY_NAME: &str = "pyspy";
const PYSPY_VERSION: &str = env!("CARGO_PKG_VERSION");

const LAST_INSTRUCTION: u32 = LineNo::LastInstruction as u32;
const FIRST: u32 = LineNo::First as u32;
const NO_LINE: u32 = LineNo::NoLine as u32;

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
    line_no: u32,
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
        lineno: LineNo::from(line_no).into(),
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
    ffikit::run(PyroscopeAgentBuilder::new(
        agent_builder,
        pyspy,
        dynamic_tags,
    ))
    .is_ok()
}

#[pyfunction]
fn drop_agent() -> bool {
    ffikit::stop().is_ok()
}

#[pyfunction]
fn add_thread_tag(key: String, value: String) -> bool {
    ffikit::add_thread_tag(self_thread_id(), Tag { key, value }).is_ok()
}

#[pyfunction]
fn remove_thread_tag(key: String, value: String) -> bool {
    ffikit::remove_thread_tag(self_thread_id(), Tag { key, value }).is_ok()
}

#[repr(C)]
#[derive(Debug)]
pub enum LineNo {
    LastInstruction = 0,
    First = 1,
    NoLine = 2,
}

impl From<u32> for LineNo {
    fn from(val: u32) -> Self {
        match val {
            FIRST => LineNo::First,
            NO_LINE => LineNo::NoLine,
            _ => LineNo::LastInstruction,
        }
    }
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
    m.add("LastInstruction", LAST_INSTRUCTION)?;
    m.add("First", FIRST)?;
    m.add("NoLine", NO_LINE)?;
    m.add_function(wrap_pyfunction!(initialize_logging, m)?)?;
    m.add_function(wrap_pyfunction!(initialize_agent, m)?)?;
    m.add_function(wrap_pyfunction!(drop_agent, m)?)?;
    m.add_function(wrap_pyfunction!(add_thread_tag, m)?)?;
    m.add_function(wrap_pyfunction!(remove_thread_tag, m)?)?;
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
