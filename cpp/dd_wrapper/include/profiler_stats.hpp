#pragma once

/* Pyroscope patch: Datadog::ProfilerStats is Pyroscope::ProfilerStats.
 *
 * Upstream this header declares the real counter block that rides alongside
 * the profile in Datadog's internal-metadata channel. Ours drops every
 * argument -- see cpp/pyroscope/Pyroscope.h, and stack_scope.md for why there
 * is no sink to report these numbers to. */

#include "Pyroscope.h"

namespace Datadog {

using ProfilerStats = Pyroscope::ProfilerStats;

} // namespace Datadog
