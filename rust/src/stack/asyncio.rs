//! Ported from dd-trace-py `ddtrace/profiling/_asyncio.py`.
//!
//! Upstream hooks the `asyncio` and `uvloop` imports with `ModuleWatchdog`; we
//! have no import-hook machinery, so `install` patches whichever of the two is
//! already in `sys.modules` and skips the rest. See `stack_known_bugs.md`.

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
    fn pyroscope_stack_set_uvloop_mode(thread_id: u64, value: bool) -> bool;
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
    let applied = py.detach(|| unsafe { pyroscope_stack_set_uvloop_mode(thread_id, value) });
    let state = if value { "enabled" } else { "disabled" };
    if applied {
        log::info!(
            target: "pyroscope-python",
            "uvloop task unwinding {state} for thread {thread_id}"
        );
    } else {
        log::warn!(
            target: "pyroscope-python",
            "uvloop task unwinding not {state} for thread {thread_id}: the thread is not \
             registered"
        );
    }
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
