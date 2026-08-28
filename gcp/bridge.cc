#include "bridge.h"

#include "profiler.h"

extern "C" PyObject *gcp_cpu_profiler_collect(int64_t duration_nanos,
                                              int64_t period_nanos) {
  CPUProfiler profiler(duration_nanos, period_nanos);
  return profiler.Collect();
}
