use crate::backend::Tag;
use crate::error::{PyroscopeError, Result};
use crate::pyroscope::{PyroscopeAgentBuilder, PyroscopeAgentRunning};
use crate::{PyroscopeAgent, ThreadId};
use crate::{forksafety, memory};
use pyo3::Python;

static STATE: forksafety::LeakableMutex<State> = forksafety::LeakableMutex::new();

#[derive(Default)]
struct State {
    agent: Option<PyroscopeAgent<PyroscopeAgentRunning>>,
}

pub fn run(py: Python<'_>, agent: PyroscopeAgentBuilder) -> Result<()> {
    let mut guard = STATE.mutex().lock()?;
    if guard.agent.is_some() {
        return Err(PyroscopeError::AgentAlreadyRunning);
    }
    let mem_config = agent.config.mem_config.clone();
    let start_agent =
        || -> Result<PyroscopeAgent<PyroscopeAgentRunning>> { agent.build()?.start() };

    memory::start(py, &mem_config)
        .map_err(|err| PyroscopeError::new(&format!("failed to start memory profiler: {err}")))?;

    let agent = start_agent();
    match agent {
        Ok(agent) => {
            guard.agent = Some(agent);
            Ok(())
        }
        Err(err) => {
            memory::stop(py);
            Err(err)
        }
    }
}

pub fn add_thread_tag(tid: ThreadId, tag: Tag) -> Result<()> {
    if let Some(agent) = &STATE.mutex().lock()?.agent {
        agent.add_thread_tag(tid, tag)
    } else {
        Err(PyroscopeError::AgentNotRunning)
    }
}

pub fn remove_thread_tag(tid: ThreadId, tag: Tag) -> Result<()> {
    if let Some(agent) = &STATE.mutex().lock()?.agent {
        agent.remove_thread_tag(tid, tag)
    } else {
        Err(PyroscopeError::AgentNotRunning)
    }
}

pub fn stop(py: Python<'_>) -> Result<()> {
    let agent = STATE.mutex().lock()?.agent.take();
    if let Some(agent) = agent {
        let res = py.detach(|| agent.stop());
        crate::memory::stop(py);
        res
    } else {
        Err(PyroscopeError::AgentNotRunning)
    }
}

pub fn at_fork_after_in_child(_py: Python<'_>) {
    // Here we intentionally leak the whole running agent.
    // This runs post-fork in the child, the old agent must never be dropped there (its
    // stop() joins threads that don't survive fork)
    STATE.leak_and_reset();
}
