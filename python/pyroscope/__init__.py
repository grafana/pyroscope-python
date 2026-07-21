import warnings
import logging
import sys

from . import _native as lib

from contextlib import contextmanager

LOGGER = logging.getLogger(__name__)

LineNo = lib.LineNo

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
        mem_enabled=False,
        mem_max_nframe=128,
        mem_heap_sample_size=512 * 1024,
        mem_enable_mem_domain=True,
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
        mem_enabled,
        mem_max_nframe,
        mem_heap_sample_size,
        mem_enable_mem_domain,
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
