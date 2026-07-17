use crate::backend::Tag;
use crate::error::{PyroscopeError, Result};
use crate::forksafety::LeakableMutex;
use crate::pyroscope::{PyroscopeAgentBuilder, PyroscopeAgentRunning};
use crate::{PyroscopeAgent, ThreadId};
use pyo3::Python;
use std::ops::DerefMut;

static STATE: LeakableMutex<State> = LeakableMutex::new();

#[derive(Default)]
struct State {
    agent: Option<PyroscopeAgent<PyroscopeAgentRunning>>,
}

pub fn run(agent: PyroscopeAgentBuilder) -> Result<()> {
    let mut guard = STATE.mutex().lock()?;
    if guard.agent.is_some() {
        return Err(PyroscopeError::AgentAlreadyRunning);
    }

    let agent = agent.build()?.start()?;

    guard.agent = Some(agent);

    Ok(())
}

pub fn add_thread_tag(tid: ThreadId, tag: Tag) -> Result<()> {
    if let Some(agent) = &STATE.mutex().lock()?.deref_mut().agent {
        agent.add_thread_tag(tid, tag)
    } else {
        Err(PyroscopeError::AgentNotRunning)
    }
}

pub fn remove_thread_tag(tid: ThreadId, tag: Tag) -> Result<()> {
    if let Some(agent) = &STATE.mutex().lock()?.deref_mut().agent {
        agent.remove_thread_tag(tid, tag)
    } else {
        Err(PyroscopeError::AgentNotRunning)
    }
}

pub fn stop() -> Result<()> {
    if let Some(agent) = STATE.mutex().lock()?.agent.take() {
        agent.stop()
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
