use crate::backend::Tag;
use crate::error::{PyroscopeError, Result};
use crate::pyroscope::PyroscopeAgentBuilder;
use crate::{PyroscopeAgent, ThreadId};
use crate::{forksafety, memory};
use pyo3::Python;

static STATE: forksafety::LeakableMutex<State> = forksafety::LeakableMutex::new();

#[derive(Default)]
enum State {
    #[default]
    Idle,
    Busy,
    Running(Box<PyroscopeAgent>),
}

fn create_http_client() -> Result<reqwest::blocking::Client> {
    // reqwest's blocking client waits on its runtime thread via Thread::park.
    // Captured crash when this ran on the fork-surviving thread:
    //   EXC_BREAKPOINT (SIGTRAP)
    //   libdispatch: BUG IN CLIENT OF LIBDISPATCH:
    //                Use-after-free of dispatch_semaphore_t or dispatch_group_t
    //   libsystem_c: crashed on child side of fork pre-exec
    //
    //   _dispatch_semaphore_wait_slow
    //   std::thread::Thread::park
    //   reqwest::blocking::client::ClientBuilder::build
    //   _native::session::SessionManager::new
    //   _native::pyroscope::PyroscopeAgentBuilder::build
    //   _native::ffikit::run
    //   _native::initialize_agent
    Ok(forksafety::no_dispatch_semaphore(|| {
        reqwest::blocking::Client::builder().build()
    })?)
}

pub fn run(py: Python<'_>, agent: PyroscopeAgentBuilder) -> Result<()> {
    let mut guard = STATE.mutex().lock()?;
    match *guard {
        State::Idle => {}
        State::Busy => return Err(PyroscopeError::ConcurrentOperation),
        State::Running(_) => return Err(PyroscopeError::AgentAlreadyRunning),
    }
    let mem_config = agent.config.mem_config.clone();
    let start_agent = || -> Result<PyroscopeAgent> {
        // Create the client only after the Idle check, so an already-running or
        // busy agent doesn't build (and, on macOS, spawn a thread for) a client
        // that would just be thrown away.
        let http_client = create_http_client()?;
        agent.build(http_client)?.start()
    };

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
    //
    // agent.stop() sends a Kill over the bounded session channel; a full
    // channel makes SyncSender::send park. On macOS that parker is a
    // dispatch_semaphore_t inherited across fork and aborts in the child:
    //   _dispatch_semaphore_wait_slow
    //   std::thread::Thread::park
    //   std::sync::mpmc::zero::Channel::send
    //   std::sync::mpmc::Sender::send
    //   _native::pyroscope::PyroscopeAgent::stop
    //   _native::ffikit::stop
    //   _native::__pyfunction_drop_agent
    // so run it on a fresh thread via no_dispatch_semaphore.
    let res = py.detach(|| forksafety::no_dispatch_semaphore(|| agent.stop()));
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
