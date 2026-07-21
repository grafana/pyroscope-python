use crate::backend::Tag;
use crate::error::{PyroscopeError, Result};
use crate::pyroscope::{PyroscopeAgentBuilder, PyroscopeAgentRunning};
use crate::{PyroscopeAgent, ThreadId};
use crate::{forksafety, memory};
use pyo3::Python;

static STATE: forksafety::LeakableMutex<State> = forksafety::LeakableMutex::new();

#[derive(Default)]
enum State {
    #[default]
    Idle,
    Busy,
    Running(Box<PyroscopeAgent<PyroscopeAgentRunning>>),
}

pub fn run(py: Python<'_>, agent: PyroscopeAgentBuilder) -> Result<()> {
    let mut guard = STATE.mutex().lock()?;
    match *guard {
        State::Idle => {}
        State::Busy => return Err(PyroscopeError::ConcurrentOperation),
        State::Running(_) => return Err(PyroscopeError::AgentAlreadyRunning),
    }
    let mem_config = agent.config.mem_config.clone();
    let start_agent =
        || -> Result<PyroscopeAgent<PyroscopeAgentRunning>> { agent.build()?.start() };

    memory::start(py, &mem_config)
        .map_err(|err| PyroscopeError::new(&format!("failed to start memory profiler: {err}")))?;

    let agent = start_agent();
    match agent {
        Ok(agent) => {
            *guard = State::Running(Box::new(agent));
            Ok(())
        }
        Err(err) => {
            memory::stop(py);
            Err(err)
        }
    }
}

pub fn add_thread_tag(tid: ThreadId, tag: Tag) -> Result<()> {
    if let State::Running(agent) = &*STATE.mutex().lock()? {
        agent.add_thread_tag(tid, tag)
    } else {
        Err(PyroscopeError::AgentNotRunning)
    }
}

pub fn remove_thread_tag(tid: ThreadId, tag: Tag) -> Result<()> {
    if let State::Running(agent) = &*STATE.mutex().lock()? {
        agent.remove_thread_tag(tid, tag)
    } else {
        Err(PyroscopeError::AgentNotRunning)
    }
}

pub fn stop(py: Python<'_>) -> Result<()> {
    // Claim the agent and leave a Busy marker so concurrent run/stop calls
    // fail fast instead of racing with the teardown below.
    let agent = {
        let mut guard = STATE.mutex().lock()?;
        match std::mem::replace(&mut *guard, State::Busy) {
            State::Running(agent) => agent,
            State::Busy => return Err(PyroscopeError::ConcurrentOperation),
            State::Idle => {
                *guard = State::Idle;
                return Err(PyroscopeError::AgentNotRunning);
            }
        }
    };

    // The lock must not be held while joining the agent threads: the snapshot
    // thread attaches to Python for the memory flush, and a third thread
    // already attached to Python could block on the lock, which would
    // deadlock the three of them (stopper -> snapshot thread -> GIL holder ->
    // lock). The GIL is detached for the same reason.
    let res = py.detach(|| agent.stop());
    crate::memory::stop(py);
    *STATE.mutex().lock()? = State::Idle;
    res
}

pub fn at_fork_after_in_child(_py: Python<'_>) {
    // Here we intentionally leak the whole running agent.
    // This runs post-fork in the child, the old agent must never be dropped there (its
    // stop() joins threads that don't survive fork)
    STATE.leak_and_reset();
}
