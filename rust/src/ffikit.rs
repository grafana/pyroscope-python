use crate::backend::Tag;
use crate::error::{PyroscopeError, Result};
use crate::pyroscope::{PyroscopeAgentBuilder, PyroscopeAgentRunning};
use crate::{PyroscopeAgent, ThreadId};
use pyo3::Python;
use std::ops::DerefMut;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

static STATE: AtomicPtr<Mutex<State>> = AtomicPtr::new(std::ptr::null_mut());

struct State {
    agent: Option<PyroscopeAgent<PyroscopeAgentRunning>>,
}
impl State {
    fn new_static() -> *mut Mutex<State> {
        Box::into_raw(Box::new(Mutex::new(State { agent: None })))
    }
}

fn state_lock() -> &'static Mutex<State> {
    unsafe {
        let cur = STATE.load(Ordering::SeqCst);
        if !cur.is_null() {
            return &*cur;
        }

        let new = State::new_static();
        let res = STATE.compare_exchange(
            std::ptr::null_mut(),
            new,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        match res {
            Ok(_) => &*new,
            Err(old) => {
                drop(Box::from_raw(new));
                &*old
            }
        }
    }
}

fn leak_state() {
    // this runs post-fork in the child, the old agent must never be dropped there (its
    // stop() joins threads that don't survive fork)
    STATE.store(State::new_static(), Ordering::SeqCst)
}

pub fn run(agent: PyroscopeAgentBuilder) -> Result<()> {
    let mut guard = state_lock().lock()?;
    if guard.agent.is_some() {
        return Err(PyroscopeError::AgentAlreadyRunning);
    }

    let agent = agent.build()?.start()?;

    guard.agent = Some(agent);

    Ok(())
}

pub fn add_thread_tag(tid: ThreadId, tag: Tag) -> Result<()> {
    if let Some(agent) = &state_lock().lock()?.deref_mut().agent {
        agent.add_thread_tag(tid, tag)
    } else {
        Err(PyroscopeError::AgentNotRunning)
    }
}

pub fn remove_thread_tag(tid: ThreadId, tag: Tag) -> Result<()> {
    if let Some(agent) = &state_lock().lock()?.deref_mut().agent {
        agent.remove_thread_tag(tid, tag)
    } else {
        Err(PyroscopeError::AgentNotRunning)
    }
}

pub fn stop() -> Result<()> {
    if let Some(agent) = state_lock().lock()?.agent.take() {
        agent.stop()
    } else {
        Err(PyroscopeError::AgentNotRunning)
    }
}

pub fn at_fork_after_in_child(_py: Python<'_>) {
    leak_state()
}
