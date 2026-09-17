#pragma once

/* Pyroscope patch: deliberately empty.
 *
 * Upstream this is dd_wrapper's C-ABI-ish facade -- the ddup_config_* setters,
 * ddup_start/ddup_cleanup, ddup_upload and the endpoint-count calls, plus a
 * ddup_push_* proxy for every Datadog::Sample method. It exists so the Cython
 * profiling modules can drive the profiler without seeing C++ types.
 *
 * Pyroscope vendors no Cython layer and no libdatadog uploader, so none of it
 * is reimplemented. cpp/stack/src/stack_renderer.cpp includes this header but
 * references no ddup_ symbol -- the include is dead upstream too, left behind
 * when the renderer moved to calling Sample directly. This file exists only so
 * that include needs no patching; drop both if the renderer is ever edited. */
