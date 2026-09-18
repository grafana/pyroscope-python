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
    let stack_config = agent.config.stack_config.clone();
    let start_agent = || -> Result<PyroscopeAgent> {
        // Create the client only after the Idle check, so an already-running or
        // busy agent doesn't build (and, on macOS, spawn a thread for) a client
        // that would just be thrown away.
        let http_client = create_http_client()?;
        agent.build(http_client)?.start()
    };

    memory::start(py, &mem_config)
        .map_err(|err| PyroscopeError::new(&format!("failed to start memory profiler: {err}")))?;

    // TODO(Pyroscope): must move after Sampler::start() once that exists.
    if let Err(err) = crate::stack::install_thread_hooks(py, &stack_config) {
        stop_profilers(py);
        return Err(PyroscopeError::new(&format!(
            "failed to install stack sampler thread hooks: {err}"
        )));
    }

    let agent = start_agent();
    match agent {
        Ok(agent) => {
            *guard = State::Running(Box::new(agent));
            Ok(())
        }
        Err(err) => {
            stop_profilers(py);
            Err(err)
        }
    }
}

/// Stop every native profiler and release process-wide profiling state.
///
/// Whole-agent teardown, and the only legitimate caller of
/// `encode::interner::clear()`: a per-profiler stop cannot know whether some
/// other profiler still holds interned string indices. Every profiler must be
/// stopped, and every out-of-Rust cache of indices dropped, before the table
/// they interned into goes away. See the invariant on `interner::clear`.
///
/// TODO(Pyroscope): when the vendored CPU stack sampler is wired in, this is
/// not sufficient for the fork-child path. `stack_atfork_child`
/// (cpp/stack/src/sampler.cpp) runs inside `os.fork()`, i.e. *before* Python's
/// `at_fork_after_in_child` reaches here, and it calls `restart_after_fork()`
/// after clearing the renderer caches -- so the sampling thread is live again
/// and re-warming `StackRenderer::string_id_cache` by the time we clear the
/// table underneath it. The sampler must be stopped (and kept stopped; the
/// agent is dead in the child) rather than resurrected. Do not rely on
/// `renderer_.postfork_child()` for this.
pub fn stop_profilers(py: Python<'_>) {
    crate::memory::stop(py);
    crate::stack::clear_samples();
    crate::encode::interner::clear();
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
    stop_profilers(py);
    *STATE.mutex().lock()? = State::Idle;
    res
}

pub fn at_fork_after_in_child(_py: Python<'_>) {
    // Here we intentionally leak the whole running agent.
    // This runs post-fork in the child, the old agent must never be dropped there (its
    // stop() joins threads that don't survive fork)
    #[cfg(not(miri))]
    STATE.leak_and_reset();
    // The miri variant returns the abandoned allocation so tests can reclaim
    // it. This hook is never exercised under Miri, and leaking is the whole
    // point here, so discard it.
    #[cfg(miri)]
    let _ = STATE.leak_and_reset();
}
