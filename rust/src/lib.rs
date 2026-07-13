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

use crate::backend::{BackendConfig, BackendImpl, Tag, ThreadTagsSet};
use crate::pyroscope::{PyroscopeAgentBuilder, PyroscopeAgentRunning};
use crate::pyspy_backend::Pyspy;
use libc::getpid;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::wrap_pyfunction;
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Mutex, MutexGuard};

const LOG_TAG: &str = "Pyroscope::FFI";

const PYSPY_NAME: &str = "pyspy";
const PYSPY_VERSION: &str = env!("CARGO_PKG_VERSION");

const LAST_INSTRUCTION: u32 = LineNo::LastInstruction as u32;
const FIRST: u32 = LineNo::First as u32;
const NO_LINE: u32 = LineNo::NoLine as u32;

struct AgentConfig {
    pyspy_config: py_spy::Config,
    backend_config: BackendConfig,
    pyroscope_config: pyroscope::PyroscopeConfig,
    restart_on_fork_in_child: bool,
}

struct AgentState {
    agent: Option<PyroscopeAgent<PyroscopeAgentRunning>>,
    restart_config: Option<AgentConfig>,
}

impl AgentState {
    const fn new() -> AgentState {
        AgentState {
            agent: None,
            restart_config: None,
        }
    }
}

type AgentMutex = Mutex<AgentState>;
type AgentGuard = MutexGuard<'static, AgentState>;

static RUNNING_AGENT: AtomicPtr<AgentMutex> = AtomicPtr::new(std::ptr::null_mut());

fn agent_mutex() -> &'static AgentMutex {
    let ptr = RUNNING_AGENT.load(Ordering::Acquire);
    if !ptr.is_null() {
        return unsafe { &*ptr };
    }
    let fresh = Box::into_raw(Box::new(Mutex::new(AgentState::new())));
    match RUNNING_AGENT.compare_exchange(
        std::ptr::null_mut(),
        fresh,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => unsafe { &*fresh },
        Err(published) => unsafe {
            drop(Box::from_raw(fresh));
            &*published
        },
    }
}

thread_local! {
    static FORK_GUARD: Cell<Option<AgentGuard>> = const { Cell::new(None) };
}

fn build_and_start_agent(config: &AgentConfig) -> Result<PyroscopeAgent<PyroscopeAgentRunning>> {
    let dynamic_tags = ThreadTagsSet::new();
    let pyspy = BackendImpl::new(Box::new(Pyspy::new(
        config.pyspy_config.clone(),
        config.backend_config,
        dynamic_tags.clone(),
    )));
    PyroscopeAgentBuilder::new(config.pyroscope_config.clone(), pyspy, dynamic_tags)
        .build()?
        .start()
}

struct RawStderrLogger;

// A single unlocked write(2) per line: no user-space lock, safe in forked children.
impl log::Log for RawStderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {} > {}\n",
            humantime::format_rfc3339_millis(std::time::SystemTime::now()),
            record.level(),
            record.target(),
            record.args()
        );
        let _ = unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
    }

    fn flush(&self) {}
}

#[pyfunction]
fn initialize_logging(logging_level: u32) -> bool {
    static LOGGER: RawStderrLogger = RawStderrLogger;

    let level = match logging_level {
        50 => log::LevelFilter::Off,
        40 => log::LevelFilter::Error,
        30 => log::LevelFilter::Warn,
        20 => log::LevelFilter::Info,
        _ => log::LevelFilter::Debug,
    };
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level);
    true
}

#[pyfunction]
fn fork_handler_before(py: Python<'_>) {
    py.detach(|| {
        // Hold the agent mutex across the fork; the agent keeps running.
        let guard = agent_mutex().lock().unwrap_or_else(|p| p.into_inner());
        FORK_GUARD.set(Some(guard));
    });
}

#[pyfunction]
fn fork_handler_after_in_parent() {
    drop(FORK_GUARD.take());
}

fn emit_fork_leak_warning(py: Python<'_>, restarting: bool) {
    let msg = if restarting {
        "pyroscope: the process was forked while the profiling agent was running. The agent \
         instance inherited from the parent is unusable in the child and has been leaked (its \
         memory, sample buffers and thread bookkeeping cannot be reclaimed). Forking while \
         agent threads are running also carries a small risk of deadlocks in the child if the \
         fork lands while one of those threads holds a process-global lock. A new agent is \
         started automatically because restart_on_fork_in_child=True. To avoid the leak and \
         the deadlock risk, configure pyroscope in worker processes after forking, or call \
         pyroscope.shutdown() before forking."
    } else {
        "pyroscope: the process was forked while the profiling agent was running. The agent \
         instance inherited from the parent is unusable in the child and has been leaked (its \
         memory, sample buffers and thread bookkeeping cannot be reclaimed), and profiling is \
         disabled in this process. Forking while agent threads are running also carries a \
         small risk of deadlocks in the child if the fork lands while one of those threads \
         holds a process-global lock. To avoid the leak and the deadlock risk, configure \
         pyroscope in worker processes after forking, or call pyroscope.shutdown() before \
         forking. Pass restart_on_fork_in_child=True to configure() to restart profiling in \
         forked children automatically (each fork still leaks the parent agent)."
    };
    log::warn!(target: LOG_TAG, "[{}] {}", unsafe { getpid() }, msg);
    let res = py.import("warnings").and_then(|warnings| {
        warnings.call_method1(
            "warn",
            (msg, py.get_type::<pyo3::exceptions::PyRuntimeWarning>()),
        )
    });
    if let Err(e) = res {
        log::error!(target: LOG_TAG, "[{}] failed to emit fork warning: {}", unsafe { getpid() }, e);
    }
}

#[pyfunction]
fn fork_handler_after_in_child(py: Python<'_>) {
    let Some(mut guard) = FORK_GUARD.take() else {
        log::error!(target: LOG_TAG, "[{}] fork guard missing in child fork handler", unsafe { getpid() });
        return;
    };
    let was_running = guard.agent.is_some();
    let restart_config = guard.restart_config.take();
    // Leak the guard, the mutex and the inherited agent; the child must never touch them.
    std::mem::forget(guard);

    let fresh = Box::into_raw(Box::new(Mutex::new(AgentState::new())));
    RUNNING_AGENT.store(fresh, Ordering::Release);

    let restarting = restart_config
        .as_ref()
        .is_some_and(|c| c.restart_on_fork_in_child);
    if was_running {
        emit_fork_leak_warning(py, restarting);
    }

    if !restarting {
        return;
    }
    let mut config = restart_config.unwrap();
    config.pyspy_config.pid = Some(std::process::id().try_into().unwrap());

    py.detach(|| match build_and_start_agent(&config) {
        Ok(agent) => {
            log::debug!(target: LOG_TAG, "[{}] agent restarted in child after fork", unsafe { getpid() });
            let mut state = agent_mutex().lock().unwrap();
            state.agent = Some(agent);
            state.restart_config = Some(config);
        }
        Err(e) => {
            log::error!(target: LOG_TAG, "[{}] failed to restart agent in child after fork: {}", unsafe { getpid() }, e)
        }
    });
}

fn initialize_os_python_fork_handlers(py: Python<'_>) -> PyResult<()> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }

    let register = || -> PyResult<()> {
        let args = PyDict::new(py);
        args.set_item("before", wrap_pyfunction!(fork_handler_before, py)?)?;
        args.set_item(
            "after_in_parent",
            wrap_pyfunction!(fork_handler_after_in_parent, py)?,
        )?;
        args.set_item(
            "after_in_child",
            wrap_pyfunction!(fork_handler_after_in_child, py)?,
        )?;
        py.import("os")?
            .getattr("register_at_fork")?
            .call((), Some(&args))?;
        Ok(())
    };
    register().inspect_err(|_| {
        REGISTERED.store(false, Ordering::Release);
    })
}

#[allow(clippy::too_many_arguments)]
#[pyfunction]
fn initialize_agent(
    py: Python<'_>,
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
    restart_on_fork_in_child: bool,
) -> bool {
    if initialize_os_python_fork_handlers(py).is_err() {
        return false;
    }

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

    let agent_config = AgentConfig {
        pyspy_config: config,
        backend_config,
        pyroscope_config: agent_builder,
        restart_on_fork_in_child,
    };

    py.detach(|| {
        let mut guard = agent_mutex().lock()?;
        match guard.agent {
            None => {
                guard.agent = Some(build_and_start_agent(&agent_config)?);
                guard.restart_config = Some(agent_config);
                log::debug!(target: LOG_TAG, "[{}] agent started", unsafe { getpid() });
                Ok(())
            }
            Some(_) => {
                log::debug!(target: LOG_TAG, "[{}] agent already running", unsafe { getpid() });
                Err(PyroscopeError::AgentAlreadyRunning)
            }
        }
    })
    .is_ok()
}

#[pyfunction]
fn drop_agent(py: Python<'_>) -> bool {
    log::debug!(target: LOG_TAG, "[{}] drop_agent", unsafe { getpid() });

    let res = py.detach(|| {
        let mut guard = agent_mutex().lock()?;
        guard.restart_config = None;
        match guard.agent.take() {
            None => {
                log::debug!(target: LOG_TAG, "[{}] agent not running", unsafe { getpid() });
                Err(PyroscopeError::AgentNotRunning)
            }
            Some(agent) => {
                let res = agent.stop();
                log::debug!(target: LOG_TAG, "[{}] agent stopped, ok={}", unsafe { getpid() }, res.is_ok());
                res
            }
        }
    });
    res.is_ok()
}

#[pyfunction]
fn add_thread_tag(py: Python<'_>, key: String, value: String) -> bool {
    py.detach(|| {
        if let Some(agent) = &agent_mutex().lock()?.agent {
            agent.add_thread_tag(self_thread_id(), Tag { key, value })
        } else {
            Err(PyroscopeError::AgentNotRunning)
        }
    })
    .is_ok()
}

#[pyfunction]
fn remove_thread_tag(py: Python<'_>, key: String, value: String) -> bool {
    py.detach(|| {
        if let Some(agent) = &agent_mutex().lock()?.agent {
            agent.remove_thread_tag(self_thread_id(), Tag { key, value })
        } else {
            Err(PyroscopeError::AgentNotRunning)
        }
    })
    .is_ok()
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
