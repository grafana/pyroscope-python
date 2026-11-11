Sub-issue of #270.

Reduce Signal's loaded CPU overhead by **decoupling its resolver from py-spy, avoiding per-frame heap allocations, and building pprof incrementally**.

The current Signal path in `rust/src/signal_sampler/resolve.rs` calls `py_spy::stack_trace::resolve_frames` through a `remoteprocess::ProcessMemory` adapter. It returns owned `py_spy::Frame` values with `String` names and filenames. `signal_backend.rs` then constructs another frame vector and `StackTrace`, aggregates it in `StackBuffer`, and builds pprof strings/functions/locations later during encoding.

Target design:

- Give Signal its own resolution path using SDK-owned metadata types and the supported CPython layouts. Remove Signal's dependency on py-spy decoder/types and adapter glue; the separate py-spy profiler remains supported.
- Resolve names and filenames as **string views backed by reusable validated buffers or stable owned/interned storage**. Avoid constructing new owned strings or heap frame objects for every frame of every sample. Allocate/intern only when new metadata or a new stack requires storage, and reuse existing IDs and scratch capacity on the warmed path.
- **Build pprof incrementally on the normal consumer/resolver thread**: resolve frame metadata directly into string, function and location IDs, then aggregate the weighted sample. Eliminate the intermediate owned-frame/`StackTrace`/`StackBuffer` conversion for Signal. Reuse a scratch location-ID buffer and preserve duplicate-stack aggregation.
- Define ownership and reclamation across uploads and shutdown so string views remain valid and caches do not grow without bound. Code-object address reuse must not silently reuse stale cached metadata.

The goal is a Signal-to-pprof path without routine per-frame allocation or py-spy resolution glue, with a measured reduction in the additional Signal CPU cost identified in the parent issue.

[Benchmark results and reproduction instructions](https://github.com/grafana/pyroscope-squad/issues/221#issuecomment-6732569172).
