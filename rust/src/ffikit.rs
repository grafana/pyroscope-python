use crate::backend::Tag;
use crate::error::{PyroscopeError, Result};
use crate::forksafety;
use crate::pyroscope::{PyroscopeAgentBuilder, PyroscopeAgentRunning};
use crate::{PyroscopeAgent, ThreadId};
use pyo3::Python;

static STATE: forksafety::LeakableMutex<State> = forksafety::LeakableMutex::new();

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

pub fn stop() -> Result<()> {
    if let Some(agent) = STATE.mutex().lock()?.agent.take() {
        // stop() sends a Kill over the bounded session channel; when that
        // channel is full SyncSender::send waits via Thread::park. Captured
        // crash when this ran on the fork-surviving thread:
        //   EXC_BREAKPOINT (SIGTRAP)
        //   libdispatch: BUG IN CLIENT OF LIBDISPATCH:
        //                Use-after-free of dispatch_semaphore_t or dispatch_group_t
        //   libsystem_c: crashed on child side of fork pre-exec
        //
        //   _dispatch_semaphore_wait_slow
        //   std::thread::Thread::park
        //   std::sync::mpmc::zero::Channel::send
        //   std::sync::mpmc::Sender::send
        //   _native::pyroscope::PyroscopeAgent::stop
        //   _native::ffikit::stop
        //   _native::__pyfunction_drop_agent
        forksafety::execute_no_libdispatch_park(|| agent.stop())
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
