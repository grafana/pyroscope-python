import warnings
import logging
import sys

from . import _native as lib

from contextlib import contextmanager

LOGGER = logging.getLogger(__name__)

LineNo = lib.LineNo
ProfilerImplementation = lib.ProfilerImplementation

def configure(
        app_name=None,
        application_name=None,
        server_address="http://localhost:4040",
        basic_auth_username="",
        basic_auth_password="",
        enable_logging=False,
        sample_rate=100,
        oncpu=True,
        native=None,
        gil_only=True,
        report_pid=False,
        report_thread_id=False,
        report_thread_name=False,
        tags=None,
        tenant_id="",
        http_headers=None,
        line_no=LineNo.LastInstruction,
        upload_interval=10,
        mem_enabled=False,
        mem_max_nframe=128,
        mem_heap_sample_size=512 * 1024,
        mem_enable_mem_domain=True,
        cpu_enabled=True,
        cpu_implementation=ProfilerImplementation.PySpy,
):
    if app_name is not None:
        warnings.warn("app_name is deprecated, use application_name", DeprecationWarning)
        application_name = app_name

    if native is not None:
        warnings.warn("native is deprecated and not supported", DeprecationWarning)

    LOGGER.disabled = not enable_logging
    if enable_logging:
        log_level = LOGGER.getEffectiveLevel()
        lib.initialize_logging(log_level)

    return lib.initialize_agent(
        application_name,
        server_address,
        basic_auth_username,
        basic_auth_password,
        sample_rate,
        oncpu,
        gil_only,
        report_pid,
        report_thread_id,
        report_thread_name,
        runtime_name(),
        runtime_version(),
        tags or {},
        tenant_id or "",
        http_headers or {},
        line_no,
        upload_interval,
        mem_enabled,
        mem_max_nframe,
        mem_heap_sample_size,
        mem_enable_mem_domain,
        cpu_enabled,
        cpu_implementation,
    )

def configure_experimental_stack_profiler(
        fast_copy=True,
        fast_copy_warmup=15.0,
        max_nframe=128,
        max_threads=25,
        max_tasks=50,
        adaptive_sampling=True,
        adaptive_target_overhead=0.01,
        adaptive_max_interval_us=100000,
        adaptive_baseline=0.0,
        adaptive_p_stable_window_s=600,
        adaptive_p_stable_percentile=95.0,
        async_tracking=False,
):
    """Set the options of the stack CPU profiler, for the whole process.

    Only the first call takes effect. A later call is refused and returns
    False, including after shutdown(), and so is any call made once a
    configure() with cpu_implementation=ProfilerImplementation.Stack has
    started the profiler.

    fast_copy reads the sampled memory with memcpy under SIGSEGV/SIGBUS
    handlers instead of a syscall, after fast_copy_warmup seconds on the
    syscall copy.

    max_threads and max_tasks cap how many threads, and how many leaf asyncio
    tasks, one sampling cycle covers; past the cap the sampler picks a uniform
    random subset. 0 means no cap.

    adaptive_sampling moves the sampling interval between 100us and
    adaptive_max_interval_us to keep the sampler near adaptive_target_overhead,
    a fraction of the process CPU time. adaptive_baseline is an overhead floor
    in core-percent units (1 = 0.01 core, 0 disables the floor), and
    adaptive_p_stable_percentile is a percentage between 0 and 100 of the app
    CPU seen over the last adaptive_p_stable_window_s seconds.

    async_tracking unwinds asyncio tasks, uvloop included. It patches asyncio
    and uvloop only if they are imported before configure().
    """
    return lib.configure_experimental_stack_profiler(
        fast_copy,
        fast_copy_warmup,
        max_nframe,
        max_threads,
        max_tasks,
        adaptive_sampling,
        adaptive_target_overhead,
        adaptive_max_interval_us,
        adaptive_baseline,
        adaptive_p_stable_window_s,
        adaptive_p_stable_percentile,
        async_tracking,
    )

def shutdown():
    drop = lib.drop_agent()

    if drop:
        LOGGER.info("Pyroscope Agent successfully shutdown")
    else:
        LOGGER.warning("Pyroscope Agent shutdown failed")
    return drop

def add_thread_tag(key, value):
    return lib.add_thread_tag(key, value)

def remove_thread_tag(key, value):
    return lib.remove_thread_tag(key, value)

def runtime_name():
    return sys.implementation.name

def runtime_version():
    vinfo = sys.implementation.version
    if vinfo.releaselevel == "final" and not vinfo.serial:
        vinfo = vinfo[:3]
    return ".".join(map(str, vinfo))

@contextmanager
def tag_wrapper(tags):
    for key, value in tags.items():
        lib.add_thread_tag(key, value)
    try:
        yield
    finally:
        for key, value in tags.items():
            lib.remove_thread_tag(key, value)

def stop():
    warnings.warn("deprecated, no longer applicable", DeprecationWarning)
def change_name(name):
    warnings.warn("deprecated, no longer applicable", DeprecationWarning)
def tag(tags):
    warnings.warn("deprecated, use tag_wrapper function", DeprecationWarning)
def remove_tags(*keys):
    warnings.warn("deprecated, no longer applicable", DeprecationWarning)
def build_summary():
    warnings.warn("deprecated, no longer applicable", DeprecationWarning)
def test_logger():
    warnings.warn("deprecated, no longer applicable", DeprecationWarning)

def _install_stack_threads(threading, register, unregister):
    Thread = threading.Thread
    orig_set_native_id = Thread._set_native_id
    orig_bootstrap_inner = Thread._bootstrap_inner

    def _set_native_id(self):
        orig_set_native_id(self)
        if self.ident is not None and self.native_id is not None:
            register(self.ident, self.native_id)

    def _bootstrap_inner(self, *args, **kwargs):
        orig_bootstrap_inner(self, *args, **kwargs)
        if self.ident is not None:
            unregister(self.ident)

    Thread._set_native_id = _set_native_id
    Thread._bootstrap_inner = _bootstrap_inner

    for tid, thread in list(threading._active.items()):
        register(tid, getattr(thread, "native_id", None) or tid)

def _install_stack_faulthandler(faulthandler, threading, pause_sampling, resume_sampling, uninstall_segv_handler, reinstall_segv_handler):
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

def _install_stack_asyncio(asyncio, threading, uvloop, track_loop, init_asyncio, link_tasks, weak_link_tasks, set_uvloop_mode):
    import inspect
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

    def publish(name, original, wrapper):
        setattr(tasks, name, wrapper)
        if getattr(asyncio, name, None) is original:
            setattr(asyncio, name, wrapper)

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
        # uvloop.run binds new_event_loop as its loop_factory keyword default.
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
