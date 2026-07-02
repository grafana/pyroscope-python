use crate::backend::Tag;
use crate::error::{PyroscopeError, Result};
use crate::pyroscope::{PyroscopeAgentBuilder};
use crate::{PyroscopeAgent, ThreadId};
use std::sync::Mutex;

#[derive(Debug, PartialEq, Clone)]
pub enum Signal {
    Kill,
    AddThreadTag(ThreadId, Tag),
    RemoveThreadTag(ThreadId, Tag),
}

static RUNNING_AGENT: Mutex<Option<PyroscopeAgent>> = Mutex::new(None);

pub fn run(agent: PyroscopeAgentBuilder) -> Result<()> {
    let mut guard = RUNNING_AGENT.lock()?;
    if (*guard).is_some() {
        return Err(PyroscopeError::AgentAlreadyRunning);
    }

    let agent = agent.build()?;

    *guard = Some(agent);

    Ok(())
}

pub fn add_thread_tag(tid: ThreadId, tag: Tag) -> Result<()> {
    if let Some(agent) = &*RUNNING_AGENT.lock()? {
        agent.add_thread_tag(tid, tag)
    } else {
        Err(PyroscopeError::AgentNotRunning)
    }
}

pub fn remove_thread_tag(tid: ThreadId, tag: Tag) -> Result<()> {
    if let Some(agent) = &*RUNNING_AGENT.lock()? {
        agent.remove_thread_tag(tid, tag)
    } else {
        Err(PyroscopeError::AgentNotRunning)
    }
}

pub fn stop() -> Result<()> {
    if let Some(agent) = RUNNING_AGENT.lock()?.take() {
        agent.stop()
    } else {
        Err(PyroscopeError::AgentNotRunning)
    }
}
