#pragma once

/* Pyroscope patch: Datadog::Sample is Pyroscope::Sample.
 *
 * Upstream this header declares the libdatadog-backed Datadog::Sample -- a
 * vector of ddog_prof_Location2 plus a vector of ddog_prof_Label2 and a
 * values array, exported via ddog_prof_Profile_add2 -- alongside
 * intern_string, intern_function, their ddog_prof_*Id2 handle typedefs, and
 * the internal::StringArena that backs label-value copies.
 *
 * Pyroscope has one Sample shim shared by every profiler in this extension.
 * It carries interned string ids from the Rust-backed string table rather than
 * Profiles Dictionary handles, and has no label storage at all, so the arena
 * has nothing to hold. See cpp/pyroscope/Pyroscope.h, which documents the
 * differences method by method, and note that cpp/stack/include/stack_renderer.hpp
 * already re-aliases Datadog::string_id to Pyroscope::string_id.
 *
 * intern_string and intern_function are not re-exported here: the vendored
 * stack sampler calls Pyroscope::intern_string directly, and there is no
 * function interning (commits "replace Datadog::intern_string with a shared
 * string table" and "replace Datadog::intern_function by reusing the encoder's
 * dedup"). */

#include "Pyroscope.h"

namespace Datadog {

using Sample = Pyroscope::Sample;

} // namespace Datadog
