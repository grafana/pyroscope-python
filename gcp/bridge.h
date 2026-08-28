#ifndef PYROSCOPE_GCP_BRIDGE_H_
#define PYROSCOPE_GCP_BRIDGE_H_

#include <Python.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// The caller must hold the GIL. Returns a new reference, or nullptr with a
// Python exception set.
PyObject *gcp_cpu_profiler_collect(int64_t duration_nanos,
                                   int64_t period_nanos);

#ifdef __cplusplus
}  // extern "C"
#endif

#endif  // PYROSCOPE_GCP_BRIDGE_H_
