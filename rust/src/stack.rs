use crate::encode::pprof::ffi::{FFIFrame, FFISampleValues};
use crate::encode::pprof::{CpuWallProfile, PProfBuilder};
use crate::utils::TimeRange;
use crate::forksafety::LeakableMutex;
use prost::Message;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::ops::{Deref, DerefMut};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static PROFILE_BUILDER: LeakableMutex<PProfBuilder<CpuWallProfile>> = LeakableMutex::new();

#[cfg(not(miri))]
unsafe extern "C" {
    fn pyroscope_stack_bump_upload_seq();
    fn pyroscope_stack_configure(
        interval_s: f64,
        fast_copy: bool,
        fast_copy_warmup_s: f64,
        max_nframes: u32,
        max_threads: u32,
        max_tasks: u32,
        adaptive_sampling: bool,
        target_overhead: f64,
        max_sampling_period_us: u64,
        baseline_core_pct: f64,
        p_stable_window_s: u32,
        p_stable_percentile: f64,
    );
    fn pyroscope_stack_fast_copy_initialized() -> bool;
    fn pyroscope_stack_interval_us() -> u64;
    fn pyroscope_stack_is_safe_copy_failed() -> bool;
    fn pyroscope_stack_start() -> bool;
    fn pyroscope_stack_stop();
    fn pyroscope_stack_take_sampling_thread_error() -> bool;
}

#[cfg(all(test, not(miri)))]
unsafe extern "C" {
    fn pyroscope_stack_upload_seq() -> u64;
}

#[cfg(not(miri))]
fn bump_upload_seq() {
    unsafe { pyroscope_stack_bump_upload_seq() }
}

#[cfg(miri)]
fn bump_upload_seq() {}

#[derive(Clone)]
pub struct Config {
    pub enabled: bool,
}

#[derive(Debug)]
pub struct Options {
    pub fast_copy: bool,
    pub fast_copy_warmup_s: f64,
    pub max_nframe: u32,
    pub max_threads: u32,
    pub max_tasks: u32,
    pub adaptive_sampling: bool,
    pub adaptive_target_overhead: f64,
    pub adaptive_max_interval_us: u64,
    pub adaptive_baseline: f64,
    pub adaptive_p_stable_window_s: u32,
    pub adaptive_p_stable_percentile: f64,
    pub async_tracking: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            fast_copy: true,
            fast_copy_warmup_s: 15.0,
            max_nframe: 128,
            max_threads: 25,
            max_tasks: 50,
            adaptive_sampling: false,
            adaptive_target_overhead: 0.01,
            adaptive_max_interval_us: 1_000_000,
            adaptive_baseline: 0.0,
            adaptive_p_stable_window_s: 600,
            adaptive_p_stable_percentile: 95.0,
            async_tracking: false,
        }
    }
}

/// Tracks whether `pyroscope_stack_start` succeeded, so `stop` never calls
/// `Sampler::stop()` on a sampler that was never started. That call bumps
/// `thread_seq_num` unconditionally, and `Sampler::prefork` reads the
/// counter's *parity* to decide whether to restart after a fork.
static STARTED: AtomicBool = AtomicBool::new(false);

static OPTIONS: OnceLock<Options> = OnceLock::new();

pub fn set_options(options: Options) -> bool {
    set_options_in(&OPTIONS, options)
}

fn set_options_in(lock: &OnceLock<Options>, options: Options) -> bool {
    if lock.set(options).is_err() {
        log::warn!(
            target: "pyroscope-python",
            "ignoring configure_cpu_profiler: the CPU profiler options are already fixed for \
             this process, by an earlier call or by the session that started the sampler; \
             in effect: {:?}",
            lock.get()
        );
        return false;
    }
    true
}

fn options() -> &'static Options {
    OPTIONS.get_or_init(Options::default)
}

/// Start the vendored echion sampler, then register the live threads with it.
///
/// Mirrors the order of `ddtrace/profiling/collector/stack.py::_init`: setters,
/// `is_safe_copy_failed()`, `start()`, thread registration, then the asyncio
/// patch. Registration must come after `start()` -- `Sampler::start` runs
/// `one_time_setup`, which placement-news echion's thread info map and would
/// discard any earlier registration -- and before the asyncio patch, whose
/// `track_asyncio_loop` is dropped for an unregistered thread.
pub fn start(py: Python<'_>, config: &Config, sample_rate: u32) -> PyResult<()> {
    if !config.enabled {
        return Ok(());
    }

    let options = options();
    configure(1.0 / f64::from(sample_rate.max(1)), options);

    if is_safe_copy_failed() {
        log::error!(
            target: "pyroscope-python",
            "no safe memory copy method available (safe_memcpy and process_vm_readv both failed); \
             the CPU stack sampler stays off"
        );
        return Ok(());
    }

    if fast_copy_initialized() {
        faulthandler::install(py)?;
    }

    if !sampler_start() {
        return Err(PyRuntimeError::new_err(
            "failed to start the CPU stack sampler's sampling thread",
        ));
    }
    STARTED.store(true, Ordering::Release);

    threads::install(py)?;
    if options.async_tracking {
        asyncio::install(py);
    }
    Ok(())
}

pub fn stop(py: Python<'_>) {
    if STARTED.swap(false, Ordering::AcqRel) {
        // Not under the GIL: Sampler::stop waits for the sampling thread, which
        // interns strings, and `memory::dump_pprof` holds the interner lock
        // while attached to Python.
        py.detach(sampler_stop);
    }
    clear_samples();
}

pub fn postfork_child() {
    #[cfg(not(miri))]
    PROFILE_BUILDER.leak_and_reset();
    #[cfg(miri)]
    let _ = PROFILE_BUILDER.leak_and_reset();
}

#[cfg(not(miri))]
fn configure(interval_s: f64, options: &Options) {
    unsafe {
        pyroscope_stack_configure(
            interval_s,
            options.fast_copy,
            options.fast_copy_warmup_s,
            options.max_nframe,
            options.max_threads,
            options.max_tasks,
            options.adaptive_sampling,
            options.adaptive_target_overhead,
            options.adaptive_max_interval_us,
            options.adaptive_baseline,
            options.adaptive_p_stable_window_s,
            options.adaptive_p_stable_percentile,
        )
    }
}

#[cfg(not(miri))]
fn fast_copy_initialized() -> bool {
    unsafe { pyroscope_stack_fast_copy_initialized() }
}

#[cfg(not(miri))]
fn interval_us() -> u64 {
    unsafe { pyroscope_stack_interval_us() }
}

#[cfg(not(miri))]
fn is_safe_copy_failed() -> bool {
    unsafe { pyroscope_stack_is_safe_copy_failed() }
}

#[cfg(not(miri))]
fn sampler_start() -> bool {
    unsafe { pyroscope_stack_start() }
}

#[cfg(not(miri))]
fn sampler_stop() {
    unsafe { pyroscope_stack_stop() }
}

#[cfg(not(miri))]
fn sampling_thread_failed() -> bool {
    unsafe { pyroscope_stack_take_sampling_thread_error() }
}

#[cfg(miri)]
fn configure(_interval_s: f64, _options: &Options) {}

#[cfg(miri)]
fn fast_copy_initialized() -> bool {
    false
}

#[cfg(miri)]
fn interval_us() -> u64 {
    0
}

#[cfg(miri)]
fn is_safe_copy_failed() -> bool {
    false
}

#[cfg(miri)]
fn sampler_start() -> bool {
    true
}

#[cfg(miri)]
fn sampler_stop() {}

#[cfg(miri)]
fn sampling_thread_failed() -> bool {
    false
}

pub fn push_sample(frames: &[FFIFrame], values: &FFISampleValues) {
    if let Ok(mut pb) = PROFILE_BUILDER.mutex().lock() {
        pb.add_ffi_sample(frames, values);
    }
}

/// Discard the samples buffered for the next cpu/wall profile.
///
/// See `crate::memory::implementation::clear_samples` for the reasoning,
/// including why the shared string table is deliberately left alone.
pub fn clear_samples() {
    let mut pb = PROFILE_BUILDER.mutex().lock().unwrap_or_else(|e| e.into_inner());
    pb.reset();
}

/// Take the accumulated cpu/wall samples as an encoded pprof, if any.
///
/// Unlike `crate::memory::dump_pprof` this needs neither the GIL nor a
/// profiler-side flush, but it keeps the same lock order: the interner before
/// the profile builder, never the reverse.
pub fn dump_pprof(sample_rate: u32, time_range: &TimeRange) -> Option<Vec<u8>> {
    let st = crate::encode::interner::string_table().lock();
    let pb = PROFILE_BUILDER.mutex().lock();
    let profile = match (st, pb) {
        (Ok(mut st), Ok(mut pb)) => {
            pb.set_profile_type(st.deref_mut(), period_ns(sample_rate));
            pb.take_profile_and_reset(st.deref(), time_range)
        }
        _ => None,
    }?;
    bump_upload_seq();
    Some(profile.encode_to_vec())
}

pub fn report_sampling_thread_error() {
    if STARTED.load(Ordering::Acquire) && sampling_thread_failed() {
        log::error!(
            target: "pyroscope-python",
            "the CPU stack sampler's sampling thread died and stopped sampling; \
             its cpu/wall profiles are empty from here on"
        );
    }
}

/// Asks the sampler rather than restating `1 / sample_rate`, because adaptive
/// sampling moves the real interval. It reports 0 before the first
/// `configure()` and under miri.
fn period_ns(sample_rate: u32) -> i64 {
    match interval_us() {
        0 => 1_000_000_000 / i64::from(sample_rate.max(1)),
        us => (us as i64).saturating_mul(1_000),
    }
}

/// Ported from dd-trace-py `ddtrace/profiling/collector/threading.py::init_stack`.
mod threads {
    use pyo3::prelude::*;
    use pyo3::types::PyModule;
    use pyo3::wrap_pyfunction;
    use std::ffi::{CString, c_char};
    use std::sync::OnceLock;

    unsafe extern "C" {
        fn pyroscope_stack_register_thread(id: u64, native_id: u64, name: *const c_char);
        fn pyroscope_stack_unregister_thread(id: u64);
    }

    static INSTALLED: OnceLock<()> = OnceLock::new();

    const INSTALL_SRC: &std::ffi::CStr = cr#"
def install(threading, register, unregister):
    Thread = threading.Thread
    orig_set_native_id = Thread._set_native_id
    orig_bootstrap_inner = Thread._bootstrap_inner

    def _set_native_id(self):
        orig_set_native_id(self)
        if self.ident is not None and self.native_id is not None:
            register(self.ident, self.native_id, self.name)

    def _bootstrap_inner(self, *args, **kwargs):
        orig_bootstrap_inner(self, *args, **kwargs)
        if self.ident is not None:
            unregister(self.ident)

    Thread._set_native_id = _set_native_id
    Thread._bootstrap_inner = _bootstrap_inner

    for tid, thread in list(threading._active.items()):
        register(tid, getattr(thread, "native_id", None) or tid, thread.name)
"#;

    #[pyfunction]
    fn register_thread(py: Python<'_>, id: u64, native_id: u64, name: &str) {
        let Ok(name) = CString::new(name) else {
            log::warn!(
                target: "pyroscope-python",
                "not registering thread {id}: its name contains an interior NUL"
            );
            return;
        };
        log::debug!(
            target: "pyroscope-python",
            "registering thread id={id} native_id={native_id} name={name:?}"
        );
        py.detach(|| unsafe { pyroscope_stack_register_thread(id, native_id, name.as_ptr()) });
    }

    #[pyfunction]
    fn unregister_thread(py: Python<'_>, id: u64) {
        log::debug!(target: "pyroscope-python", "unregistering thread id={id}");
        py.detach(|| unsafe { pyroscope_stack_unregister_thread(id) });
    }

    /// The wrappers must be Python-level functions: a `#[pyfunction]` is a
    /// `builtin_function_or_method` and has no `__get__`, so assigning one onto
    /// `Thread._set_native_id` would call it without `self`.
    pub fn install(py: Python<'_>) -> PyResult<()> {
        if INSTALLED.get().is_some() {
            return Ok(());
        }

        let module = PyModule::from_code(
            py,
            INSTALL_SRC,
            c"pyroscope_stack_threads.py",
            c"_pyroscope_stack_threads",
        )?;
        module.getattr("install")?.call1((
            py.import("threading")?,
            wrap_pyfunction!(register_thread, py)?,
            wrap_pyfunction!(unregister_thread, py)?,
        ))?;

        let _ = INSTALLED.set(());
        Ok(())
    }
}

/// Ported from dd-trace-py `ddtrace/profiling/_asyncio.py`.
///
/// Upstream hooks the `asyncio` and `uvloop` imports with `ModuleWatchdog`; we
/// have no import-hook machinery, so `install` patches whichever of the two is
/// already in `sys.modules` and skips the rest. See `stack_known_bugs.md`.
mod asyncio {
    use pyo3::ffi::PyObject;
    use pyo3::prelude::*;
    use pyo3::types::PyModule;
    use pyo3::wrap_pyfunction;
    use std::sync::OnceLock;
    use std::sync::atomic::Ordering;

    unsafe extern "C" {
        fn pyroscope_stack_init_asyncio(scheduled_tasks: *mut PyObject, eager_tasks: *mut PyObject);
        fn pyroscope_stack_track_asyncio_loop(thread_id: u64, event_loop: *mut PyObject);
        fn pyroscope_stack_link_tasks(parent: *mut PyObject, child: *mut PyObject);
        fn pyroscope_stack_weak_link_tasks(parent: *mut PyObject, child: *mut PyObject);
        fn pyroscope_stack_set_uvloop_mode(thread_id: u64, value: bool);
    }

    static INSTALLED: OnceLock<()> = OnceLock::new();

    const INSTALL_SRC: &std::ffi::CStr = cr#"
import inspect
import sys


def install(asyncio, threading, uvloop, track_loop, init_asyncio, link_tasks, weak_link_tasks, set_uvloop_mode):
    from asyncio import events, tasks

    def arg(args, kwargs, index, name):
        if len(args) > index:
            return args[index]
        return kwargs.get(name)

    def ident():
        return threading.current_thread().ident

    def running_loop():
        try:
            return asyncio.get_running_loop()
        except RuntimeError:
            return None

    def current_task():
        try:
            return asyncio.current_task()
        except RuntimeError:
            return None

    # Pyroscope patch: upstream indexes the task sets unguarded.
    def init():
        if sys.hexversion >= 0x030C0000:
            scheduled = getattr(tasks, "_scheduled_tasks", None)
            eager = getattr(tasks, "_eager_tasks", None)
        else:
            scheduled = getattr(tasks, "_all_tasks", None)
            eager = None
        data = getattr(scheduled, "data", None)
        if data is not None:
            init_asyncio(data, eager)

    # Pyroscope patch: upstream's wrapping rewrites the code object, so every
    # alias of a patched function follows; attribute assignment does not.
    def publish(name, original, wrapper):
        setattr(tasks, name, wrapper)
        if getattr(asyncio, name, None) is original:
            setattr(asyncio, name, wrapper)

    # Pyroscope patch: upstream hooks the policy only. 3.14 deprecated the
    # policy indirection, and asyncio.Runner goes through the module function.
    original_set_event_loop = events.set_event_loop

    def set_event_loop(*args, **kwargs):
        track_loop(ident(), arg(args, kwargs, 0, "loop"))
        return original_set_event_loop(*args, **kwargs)

    events.set_event_loop = set_event_loop
    if getattr(asyncio, "set_event_loop", None) is original_set_event_loop:
        asyncio.set_event_loop = set_event_loop

    policy = getattr(events, "_BaseDefaultEventLoopPolicy", None)
    if policy is None:
        policy = getattr(events, "BaseDefaultEventLoopPolicy", None)
    if policy is not None:
        original_policy_set_event_loop = policy.set_event_loop

        def policy_set_event_loop(*args, **kwargs):
            track_loop(ident(), arg(args, kwargs, 1, "loop"))
            return original_policy_set_event_loop(*args, **kwargs)

        policy.set_event_loop = policy_set_event_loop

    original_gathering_init = tasks._GatheringFuture.__init__

    def gathering_init(*args, **kwargs):
        try:
            return original_gathering_init(*args, **kwargs)
        finally:
            children = arg(args, kwargs, 1, "children")
            if children is not None and running_loop() is not None:
                parent = current_task()
                if parent is not None:
                    for child in children:
                        link_tasks(parent, child)

    tasks._GatheringFuture.__init__ = gathering_init

    original_wait = tasks._wait

    def _wait(*args, **kwargs):
        try:
            return original_wait(*args, **kwargs)
        finally:
            futures = arg(args, kwargs, 0, "fs")
            if futures is not None and running_loop() is not None:
                parent = current_task()
                if parent is not None:
                    for future in futures:
                        link_tasks(parent, future)

    publish("_wait", original_wait, _wait)

    original_as_completed = tasks.as_completed

    def _as_completed(*args, **kwargs):
        parent = current_task()
        fs = arg(args, kwargs, 0, "fs")
        if parent is not None and fs is not None:
            futures = {asyncio.ensure_future(f, loop=kwargs.get("loop")) for f in set(fs)}
            for future in futures:
                link_tasks(parent, future)
            if args:
                args = (futures,) + args[1:]
            else:
                kwargs = {**kwargs, "fs": futures}
        return original_as_completed(*args, **kwargs)

    # Pyroscope patch: upstream's code-object rewrite keeps the generator flag,
    # so its wrapper runs on first next(); attribute assignment needs an
    # explicit generator to keep as_completed lazy where the original is one.
    if inspect.isgeneratorfunction(original_as_completed):

        def as_completed(*args, **kwargs):
            return (yield from _as_completed(*args, **kwargs))

    else:
        as_completed = _as_completed

    publish("as_completed", original_as_completed, as_completed)

    original_shield = tasks.shield

    def shield(*args, **kwargs):
        awaitable = arg(args, kwargs, 0, "arg")
        future = asyncio.ensure_future(awaitable, loop=kwargs.get("loop"))
        parent = current_task()
        if parent is not None:
            link_tasks(parent, future)
        if args:
            args = (future,) + args[1:]
        else:
            kwargs = {**kwargs, "arg": future}
        return original_shield(*args, **kwargs)

    publish("shield", original_shield, shield)

    taskgroups = sys.modules.get("asyncio.taskgroups")
    taskgroup = getattr(taskgroups, "TaskGroup", None) if taskgroups is not None else None
    if taskgroup is not None and hasattr(taskgroup, "create_task"):
        original_taskgroup_create_task = taskgroup.create_task

        def taskgroup_create_task(*args, **kwargs):
            task = original_taskgroup_create_task(*args, **kwargs)
            parent = current_task()
            if parent is not None and task is not None:
                link_tasks(parent, task)
            return task

        taskgroup.create_task = taskgroup_create_task

    original_create_task = tasks.create_task

    def create_task(*args, **kwargs):
        task = original_create_task(*args, **kwargs)
        parent = current_task()
        if parent is not None and task is not None:
            weak_link_tasks(parent, task)
        return task

    publish("create_task", original_create_task, create_task)

    loop = running_loop()
    if loop is not None:
        track_loop(ident(), loop)
    init()

    if uvloop is None:
        return

    original_new_event_loop = getattr(uvloop, "new_event_loop", None)
    if original_new_event_loop is not None:

        def new_event_loop(*args, **kwargs):
            loop = original_new_event_loop(*args, **kwargs)
            thread_id = ident()
            set_uvloop_mode(thread_id, True)
            track_loop(thread_id, loop)
            init()
            return loop

        uvloop.new_event_loop = new_event_loop
        # Pyroscope patch: uvloop.run binds new_event_loop as a keyword default,
        # which upstream's code-object rewrite reaches and ours does not.
        defaults = getattr(getattr(uvloop, "run", None), "__kwdefaults__", None) or {}
        if defaults.get("loop_factory") is original_new_event_loop:
            defaults["loop_factory"] = new_event_loop

    uvloop_policy = getattr(uvloop, "EventLoopPolicy", None)
    if uvloop_policy is not None and hasattr(uvloop_policy, "set_event_loop"):
        original_uvloop_set_event_loop = uvloop_policy.set_event_loop

        def uvloop_set_event_loop(*args, **kwargs):
            thread_id = ident()
            set_uvloop_mode(thread_id, True)
            loop = arg(args, kwargs, 1, "loop")
            if loop is not None:
                track_loop(thread_id, loop)
                init()
            return original_uvloop_set_event_loop(*args, **kwargs)

        uvloop_policy.set_event_loop = uvloop_set_event_loop
"#;

    /// Not under the GIL: this takes echion's `thread_info_map_lock`, which the
    /// sampling thread holds while interning strings.
    #[pyfunction]
    fn track_asyncio_loop(py: Python<'_>, thread_id: u64, event_loop: &Bound<'_, PyAny>) {
        let event_loop = event_loop.as_ptr() as usize;
        py.detach(|| unsafe {
            pyroscope_stack_track_asyncio_loop(thread_id, event_loop as *mut PyObject)
        });
    }

    #[pyfunction]
    fn init_asyncio(scheduled_tasks: &Bound<'_, PyAny>, eager_tasks: &Bound<'_, PyAny>) {
        unsafe { pyroscope_stack_init_asyncio(scheduled_tasks.as_ptr(), eager_tasks.as_ptr()) }
    }

    /// Pyroscope patch: upstream keeps filling the link maps after `stop()`,
    /// where nothing prunes them and every `create_task` adds an entry.
    #[pyfunction]
    fn link_tasks(py: Python<'_>, parent: &Bound<'_, PyAny>, child: &Bound<'_, PyAny>) {
        if !super::STARTED.load(Ordering::Acquire) {
            return;
        }
        let (parent, child) = (parent.as_ptr() as usize, child.as_ptr() as usize);
        py.detach(|| unsafe {
            pyroscope_stack_link_tasks(parent as *mut PyObject, child as *mut PyObject)
        });
    }

    #[pyfunction]
    fn weak_link_tasks(py: Python<'_>, parent: &Bound<'_, PyAny>, child: &Bound<'_, PyAny>) {
        if !super::STARTED.load(Ordering::Acquire) {
            return;
        }
        let (parent, child) = (parent.as_ptr() as usize, child.as_ptr() as usize);
        py.detach(|| unsafe {
            pyroscope_stack_weak_link_tasks(parent as *mut PyObject, child as *mut PyObject)
        });
    }

    /// Pyroscope patch: upstream keeps the GIL here, unlike its own
    /// `track_asyncio_loop`, though both take `thread_info_map_lock`.
    #[pyfunction]
    fn set_uvloop_mode(py: Python<'_>, thread_id: u64, value: bool) {
        py.detach(|| unsafe { pyroscope_stack_set_uvloop_mode(thread_id, value) });
    }

    /// Must run after `threads::install`: `track_asyncio_loop` is a
    /// find-then-mutate on echion's thread map and is dropped for a thread that
    /// is not registered yet.
    pub fn install(py: Python<'_>) {
        if INSTALLED.set(()).is_err() {
            return;
        }

        let patched = (|| -> PyResult<()> {
            let modules = py.import("sys")?.getattr("modules")?;
            let Ok(asyncio) = modules.get_item("asyncio") else {
                log::warn!(
                    target: "pyroscope-python",
                    "not tracking asyncio tasks: asyncio is not imported yet, and cpu_async only \
                     patches what is imported when the agent starts"
                );
                return Ok(());
            };

            let uvloop = modules.get_item("uvloop").ok();
            if uvloop.is_none() {
                log::warn!(
                    target: "pyroscope-python",
                    "not tracking uvloop event loops: uvloop is not imported yet, and cpu_async \
                     only patches what is imported when the agent starts; harmless if this \
                     process does not use uvloop, otherwise import uvloop before configure()"
                );
            }

            let module = PyModule::from_code(
                py,
                INSTALL_SRC,
                c"pyroscope_stack_asyncio.py",
                c"_pyroscope_stack_asyncio",
            )?;
            module.getattr("install")?.call1((
                asyncio,
                py.import("threading")?,
                uvloop,
                wrap_pyfunction!(track_asyncio_loop, py)?,
                wrap_pyfunction!(init_asyncio, py)?,
                wrap_pyfunction!(link_tasks, py)?,
                wrap_pyfunction!(weak_link_tasks, py)?,
                wrap_pyfunction!(set_uvloop_mode, py)?,
            ))?;
            Ok(())
        })();

        if let Err(err) = patched {
            log::warn!(target: "pyroscope-python", "not tracking asyncio tasks: {err}");
        }
    }
}

/// Ported from dd-trace-py `ddtrace/profiling/_faulthandler.py`.
///
/// Upstream hooks the import with `ModuleWatchdog`; `faulthandler` is not in
/// `sys.modules` at startup, so importing and patching it here is equivalent.
mod faulthandler {
    use pyo3::exceptions::PyImportError;
    use pyo3::prelude::*;
    use pyo3::types::PyModule;
    use pyo3::wrap_pyfunction;
    use std::sync::OnceLock;

    #[allow(dead_code)]
    #[repr(C)]
    pub enum SamplerPauseResult {
        Paused,
        NotRunning,
        Timeout,
    }

    unsafe extern "C" {
        fn pyroscope_stack_pause_sampling() -> SamplerPauseResult;
        fn pyroscope_stack_resume_sampling();
        fn pyroscope_stack_uninstall_segv_handler();
        fn pyroscope_stack_reinstall_segv_handler();
    }

    static INSTALLED: OnceLock<()> = OnceLock::new();

    const INSTALL_SRC: &std::ffi::CStr = cr#"
def install(faulthandler, threading, pause_sampling, resume_sampling, uninstall_segv_handler, reinstall_segv_handler):
    _original_enable = faulthandler.enable
    _original_disable = faulthandler.disable
    _enable_lock = threading.Lock()

    def _patched_enable(*args, **kwargs):
        with _enable_lock:
            # None means the sampler is running but did not pause in time:
            # swapping handlers now would race with safe_memcpy.
            pause_result = pause_sampling()
            safe_to_swap = pause_result is not None
            try:
                if safe_to_swap:
                    try:
                        uninstall_segv_handler()
                    except Exception:
                        pass

                    # Without a fresh install, faulthandler can save itself as its
                    # own previous handler and loop forever on a fault.
                    try:
                        _original_disable()
                    except Exception:
                        pass

                try:
                    _original_enable(*args, **kwargs)
                except Exception:
                    if safe_to_swap:
                        try:
                            reinstall_segv_handler()
                        except Exception:
                            pass
                    raise

                try:
                    reinstall_segv_handler()
                except Exception:
                    pass
            finally:
                if pause_result is True:
                    resume_sampling()

    def _patched_disable():
        with _enable_lock:
            pause_result = pause_sampling()
            safe_to_swap = pause_result is not None
            try:
                if not safe_to_swap:
                    return False

                try:
                    disabled = _original_disable()
                except Exception:
                    disabled = False

                # disable() restores the handler faulthandler saved, which need not be ours.
                try:
                    reinstall_segv_handler()
                except Exception:
                    pass

                return disabled
            finally:
                if pause_result is True:
                    resume_sampling()

    faulthandler.enable = _patched_enable
    faulthandler.disable = _patched_disable

    if faulthandler.is_enabled():
        try:
            reinstall_segv_handler()
        except Exception:
            pass
"#;

    /// Not under the GIL: `Sampler::pause` waits up to 3 s for the sampling
    /// thread, which interns strings.
    #[pyfunction]
    fn pause_sampling(py: Python<'_>) -> Option<bool> {
        match py.detach(|| unsafe { pyroscope_stack_pause_sampling() }) {
            SamplerPauseResult::Paused => Some(true),
            SamplerPauseResult::NotRunning => Some(false),
            SamplerPauseResult::Timeout => None,
        }
    }

    #[pyfunction]
    fn resume_sampling() {
        unsafe { pyroscope_stack_resume_sampling() }
    }

    #[pyfunction]
    fn uninstall_segv_handler() {
        unsafe { pyroscope_stack_uninstall_segv_handler() }
    }

    #[pyfunction]
    fn reinstall_segv_handler() {
        unsafe { pyroscope_stack_reinstall_segv_handler() }
    }

    pub fn install(py: Python<'_>) -> PyResult<()> {
        if INSTALLED.get().is_some() {
            return Ok(());
        }

        let faulthandler = match py.import("faulthandler") {
            Ok(module) => module,
            Err(err) if err.is_instance_of::<PyImportError>(py) => {
                log::warn!(
                    target: "pyroscope-python",
                    "not patching faulthandler: {err}; a later faulthandler.enable() \
                     can displace the fast copy's SIGSEGV handler"
                );
                let _ = INSTALLED.set(());
                return Ok(());
            }
            Err(err) => return Err(err),
        };

        let module = PyModule::from_code(
            py,
            INSTALL_SRC,
            c"pyroscope_stack_faulthandler.py",
            c"_pyroscope_stack_faulthandler",
        )?;
        module.getattr("install")?.call1((
            faulthandler,
            py.import("threading")?,
            wrap_pyfunction!(pause_sampling, py)?,
            wrap_pyfunction!(resume_sampling, py)?,
            wrap_pyfunction!(uninstall_segv_handler, py)?,
            wrap_pyfunction!(reinstall_segv_handler, py)?,
        ))?;

        let _ = INSTALLED.set(());
        Ok(())
    }
}

/// The test binary links no libpython: pyo3's `extension-module` leaves every
/// Python symbol to the host process, and `--gc-sections` drops the vendored
/// C++ that needs them. `Sampler::track_asyncio_loop` is the exception.
#[cfg(test)]
#[unsafe(no_mangle)]
static _Py_NoneStruct: [usize; 4] = [0; 4];

#[cfg(all(test, not(miri)))]
mod thread_registration_tests {
    use pyo3::ffi::PyObject;
    use std::ffi::{CString, c_char};
    use std::sync::{Mutex, mpsc};

    unsafe extern "C" {
        fn pyroscope_stack_register_thread(id: u64, native_id: u64, name: *const c_char);
        fn pyroscope_stack_unregister_thread(id: u64);
        fn pyroscope_stack_thread_count() -> usize;
        fn pyroscope_stack_track_asyncio_loop(thread_id: u64, event_loop: *mut PyObject);
        fn pyroscope_stack_thread_asyncio_loop(thread_id: u64) -> usize;
    }

    /// Both tests mutate echion's process-wide thread map, and the count
    /// assertions below are deltas.
    static THREAD_MAP: Mutex<()> = Mutex::new(());

    /// The ids must be live pthread_t values: `ThreadInfo::create` calls
    /// `pthread_getcpuclockid` / `pthread_mach_thread_np` on them, which
    /// dereferences the pthread descriptor.
    #[test]
    fn threads_register_and_unregister_over_the_ffi() {
        let _guard = THREAD_MAP.lock().unwrap_or_else(|e| e.into_inner());
        let before = unsafe { pyroscope_stack_thread_count() };

        let (registered_tx, registered_rx) = mpsc::channel();
        let name = CString::new("stack::registration_test").unwrap();

        let mut handles = Vec::new();
        let mut releases = Vec::new();
        for _ in 0..2 {
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let registered_tx = registered_tx.clone();
            let name = name.clone();
            handles.push(std::thread::spawn(move || {
                let id = unsafe { libc::pthread_self() } as u64;
                unsafe { pyroscope_stack_register_thread(id, id, name.as_ptr()) };
                registered_tx.send(id).unwrap();
                release_rx.recv().unwrap();
                unsafe { pyroscope_stack_unregister_thread(id) };
            }));
            releases.push(release_tx);
        }

        let ids: Vec<u64> = (0..2).map(|_| registered_rx.recv().unwrap()).collect();
        assert_ne!(ids[0], ids[1], "two distinct threads");
        assert_eq!(
            unsafe { pyroscope_stack_thread_count() },
            before + 2,
            "both threads registered"
        );

        for release in releases {
            release.send(()).unwrap();
        }
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(
            unsafe { pyroscope_stack_thread_count() },
            before,
            "both threads unregistered"
        );
    }

    #[test]
    fn a_sampler_that_never_started_stashed_no_error() {
        assert!(!super::sampling_thread_failed());
    }

    /// `track_asyncio_loop` is a find-then-mutate on echion's thread map, so an
    /// unregistered thread takes the loop silently and never unwinds tasks.
    /// This is what pins `asyncio::install` after `threads::install`.
    #[test]
    fn asyncio_loops_reach_registered_threads_only() {
        let _guard = THREAD_MAP.lock().unwrap_or_else(|e| e.into_inner());

        let (registered_tx, registered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let name = CString::new("stack::asyncio_test").unwrap();
        let handle = std::thread::spawn(move || {
            let id = unsafe { libc::pthread_self() } as u64;
            unsafe { pyroscope_stack_register_thread(id, id, name.as_ptr()) };
            registered_tx.send(id).unwrap();
            release_rx.recv().unwrap();
            unsafe { pyroscope_stack_unregister_thread(id) };
        });

        let id = registered_rx.recv().unwrap();
        let unregistered = id ^ 0xffff_ffff;
        let event_loop = 0x1234_usize as *mut PyObject;

        unsafe { pyroscope_stack_track_asyncio_loop(id, event_loop) };
        unsafe { pyroscope_stack_track_asyncio_loop(unregistered, event_loop) };

        assert_eq!(
            unsafe { pyroscope_stack_thread_asyncio_loop(id) },
            0x1234,
            "the loop reached the registered thread"
        );
        assert_eq!(
            unsafe { pyroscope_stack_thread_asyncio_loop(unregistered) },
            0,
            "the loop was dropped for an unregistered thread"
        );

        release_tx.send(()).unwrap();
        handle.join().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::r#gen::google::Profile;
    use crate::encode::interner;
    use crate::encode::pprof::PprofBuilderType;
    use crate::encode::pprof::ffi::FFIInternedString;
    use std::time::{Duration, UNIX_EPOCH};

    fn intern(s: &str) -> FFIInternedString {
        (&interner::string_table().lock().unwrap().add(s)).into()
    }

    /// The memory slots are non-zero so that one leaking into a cpu or wall
    /// value cannot pass for a correct number.
    fn values(cpu_time: i64, wall_time: i64) -> FFISampleValues {
        FFISampleValues {
            cpu_time,
            wall_time,
            alloc_space: 100_000,
            alloc_count: 200_000,
            heap_space: 300_000,
            heap_count: 400_000,
        }
    }

    fn push(frames: &[FFIFrame], values: &FFISampleValues) {
        crate::ffi::pyroscope_push_sample(
            PprofBuilderType::CpuWall,
            frames.as_ptr(),
            frames.len(),
            values,
        );
    }

    fn resolve(profile: &Profile, index: i64) -> &str {
        profile.string_table[index as usize].as_str()
    }

    #[test]
    fn cpu_options_are_fixed_by_the_first_call() {
        let lock = OnceLock::new();

        assert!(set_options_in(
            &lock,
            Options {
                max_nframe: 7,
                ..Options::default()
            }
        ));
        assert!(!set_options_in(
            &lock,
            Options {
                max_nframe: 9,
                ..Options::default()
            }
        ));
        assert_eq!(lock.get().expect("the first call seals").max_nframe, 7);
    }

    #[cfg(not(miri))]
    fn upload_seq() -> Option<u64> {
        Some(unsafe { pyroscope_stack_upload_seq() })
    }

    #[cfg(miri)]
    fn upload_seq() -> Option<u64> {
        None
    }

    /// Deliberately a single test: it drains the process-wide accumulator and
    /// moves the sampler's interval, so a second test touching either in
    /// parallel would race.
    #[test]
    fn cpu_wall_samples_pushed_over_the_ffi_become_one_profile() {
        // 50 Hz on the sampler against the 100 Hz passed to dump_pprof, so the
        // period asserted below can only have come from the sampler. The
        // non-default knobs are here so every setter behind
        // pyroscope_stack_configure is crossed at least once.
        configure(
            1.0 / 50.0,
            &Options {
                fast_copy: false,
                max_tasks: 0,
                adaptive_baseline: 1.0,
                adaptive_p_stable_window_s: 5,
                adaptive_p_stable_percentile: 50.0,
                ..Options::default()
            },
        );

        let frames = [FFIFrame {
            function_name: intern("stack::tests::some_function"),
            file_name: intern("stack::tests/some_file.py"),
            line: 42,
        }];
        let time_range = TimeRange::new(UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(10)).unwrap();

        push(&frames, &values(3, 5));
        push(&frames, &values(7, 11));

        let seq_before = upload_seq();
        let bytes = dump_pprof(100, &time_range).expect("a profile with one sample");
        let profile = Profile::decode(bytes.as_slice()).expect("a decodable pprof");

        let sample_types: Vec<(&str, &str)> = profile
            .sample_type
            .iter()
            .map(|vt| (resolve(&profile, vt.r#type), resolve(&profile, vt.unit)))
            .collect();
        assert_eq!(
            sample_types,
            vec![("cpu", "nanoseconds"), ("wall", "nanoseconds")]
        );
        let period_type = profile.period_type.as_ref().expect("a period type");
        assert_eq!(
            (
                resolve(&profile, period_type.r#type),
                resolve(&profile, period_type.unit)
            ),
            ("cpu", "nanoseconds")
        );
        // configure and interval_us are no-ops under miri, so period_ns falls
        // back to 1 / sample_rate there.
        let expected_period = if cfg!(miri) { 10_000_000 } else { 20_000_000 };
        assert_eq!(profile.period, expected_period);
        assert_eq!(profile.duration_nanos, 10_000_000_000);

        assert_eq!(profile.sample.len(), 1, "one row per distinct stack");
        assert_eq!(profile.sample[0].value, vec![10, 16]);
        assert_eq!(profile.function.len(), 1);
        assert_eq!(
            resolve(&profile, profile.function[0].name),
            "stack::tests::some_function"
        );
        assert_eq!(
            resolve(&profile, profile.function[0].filename),
            "stack::tests/some_file.py"
        );
        assert_eq!(profile.location.len(), 1);
        assert_eq!(profile.location[0].line[0].line, 42);

        let seq_after_dump = upload_seq();
        assert_eq!(
            seq_after_dump,
            seq_before.map(|s| s + 1),
            "one upload, one bump"
        );

        assert!(
            dump_pprof(100, &time_range).is_none(),
            "a drained accumulator must not produce a second profile"
        );
        assert_eq!(
            upload_seq(),
            seq_after_dump,
            "an empty window is not an upload"
        );

        push(&frames, &values(1, 2));
        clear_samples();
        assert!(dump_pprof(100, &time_range).is_none());
    }
}
