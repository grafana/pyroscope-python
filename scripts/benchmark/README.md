# Profiling overhead benchmark

Measures what the CPU profiler costs, in CPU and in memory, against no profiling
at all, across workload shapes chosen to expose different parts of that cost.

Measured at the shipping `sample_rate=100` and at **`gil_only=False`**, so a tick
unwinds every live Python thread
rather than only the GIL holder. That is the configuration whose cost scales with
both thread count and stack depth, and therefore the one where the shapes
separate. The shipping default is available as the `cpu_gil_only` variant. Stack
depths are 128 frames for the baseline shapes and 256 for `deep`.

Built so that adding the next profiler is one entry in `variants.py`.

```sh
scripts/benchmark/run_linux.sh                     # full matrix (~1h)
scripts/benchmark/run_linux.sh --quick             # 3 repeats, 8s regions (~12m)
scripts/benchmark/run_linux.sh --sweep             # thread-count sweep
scripts/benchmark/run_linux.sh --skip-build        # reuse the VM build
scripts/benchmark/run_linux.sh --self-check        # validate the harness itself
```

Run `--self-check` after changing the harness, and before believing a surprising
result. It feeds every guard data it must reject *and* data it must accept, then
measures the noise floor by comparing two byte-identical unprofiled variants --
whatever "overhead" that reports is the threshold every real number has to clear.

The complementary question, whether the harness can resolve a signal known to
exist, is answered by `--sweep`: at `gil_only=False` every live thread is
unwound, so overhead must rise with thread count. A null result from a harness
that cannot resolve a known signal says nothing about the profiler.

Results land in `scripts/benchmark/results/<timestamp>/` as `report.md`,
`report.html` and `results.json`.

Ports, all overridable by environment variable, because `4040` is Pyroscope's
own default and is often already taken on a host that runs Pyroscope:

| variable | default | what |
|---|---|---|
| `BENCH_SINK_PORT` | 4040 | local ingest sink |
| `BENCH_HTTP_PORT` | 8080 | gunicorn under test |
| `BENCH_HTTP_CONTROL_PORT` | 8099 | supervisor control endpoint |

The harness refuses to start if the sink port is already held, rather than
attaching to a listener it did not create. That guard matters: pointed at a real
Pyroscope server the benchmark would upload hundreds of profiles into it and
measure network and server cost instead of terminating the data path locally.

Remote target, for running against a machine other than the default:

| variable | default |
|---|---|
| `BENCH_REMOTE` | `orb` |
| `BENCH_SRC` | `~/bench-src` |
| `BENCH_VENV` | `~/bench-venv` |

### If your ssh key prompts on every use

With a Secure Enclave agent such as Secretive, every separate `ssh`, `rsync` or
`scp` invocation needs its own signature, so a script that polls the benchmark
host produces one authentication prompt per poll. Reuse a single connection
instead, in `~/.ssh/config`:

```
Host <benchmark-host>
	ControlMaster auto
	ControlPath ~/.ssh/cm-%C
	ControlPersist 4h
```

Then one prompt covers everything for the next four hours. `ssh -O check <host>`
shows whether a master is live, `ssh -O exit <host>` closes it.

Prefer one blocking call over repeated polling regardless: `ssh host 'while
pgrep -f suite.sh >/dev/null; do sleep 30; done'` is a single connection that
returns when the run ends, rather than N connections that each need auth and
each cost a round trip.

**Leave the machine alone while a run is in progress.** Anything else competing
for CPU lands in the numbers. This is not hypothetical: a run was lost because a
diagnostic command killed the sink out from under it. The harness now refuses to
start if something already holds the sink port rather than silently attaching to
a listener it does not control, but it cannot defend against arbitrary load.

## Adding a profiler variant

```python
# variants.py
VARIANTS["experimental"] = Variant(
    name="experimental",
    label="experimental cpu profiler",
    start=lambda c: pyroscope.configure(..., server_address=c.sink_url,
                                        tags={"run_id": c.run_id}),
    stop=pyroscope.shutdown,
    per_tick="describe what one tick of work actually is",
)
```

Then `--variants none cpu experimental`. Everything else -- the shapes, the
metrics, the guards, the report -- is variant-agnostic.

Two constraints on a new variant:

* It must reach the sink with `run_id` in its tags. The sink partitions by that
  tag, and several guards depend on attributing uploads to one run.
* It must not get a fast path. The comparison is only meaningful while the
  profiler is the sole variable, so every variant shares the same imports, the
  same warmup, the same subprocess launch and the same sink. It is easy to add a
  shortcut for one variant and quietly start measuring something else.

## Why it is built this way

**Linux only.** The measurement is cgroup v2 (`cpu.stat`, `memory.peak`) plus
cpuset pinning through transient systemd scopes. Runs on the OrbStack VM via
`ssh orb`.

**Fixed work, not fixed duration.** Every shape runs an exact iteration count.
Under a fixed duration the profiler steals time and the workload simply does
less, so the cost disappears into a metric nobody is reading.

**cgroup `cpu.stat` is the headline metric, not wall clock.** The cost lands in
the process's own CPU consumption. Wall clock also picks up scheduler jitter and
frequency scaling, which have nothing to do with the profiler, and it compresses
the ranking into the noise. Both are computed from the same runs and both are
reported; only one is the headline. Reading the cgroup rather than
`time.process_time()` also covers the Rust sampler and uploader threads *and*
the multi-process gunicorn tree with one mechanism.

**Isolation by pinning, not by quota.** A CPU quota would also cap the run, but
throttling adds jitter to the quantity being measured. Disjoint cpusets --
subject `2-5`, load generator `8-11`, sink `14-15` -- keep the measurement
tooling off the cores being measured.

**Repeats round-robin across the whole matrix**, not all repeats of one cell in a
row. Batching makes the comparison hostage to drift in machine load: whichever
cell lands in a busy window looks slower, and that bias can exceed the effect.

**A noise figure beside every number.** `spread` is half the min-max range over
the median. Differences smaller than the combined spread render as `~` and are
not ranked.

**Local sink.** `sink.py` accepts and discards the upload, so network and server
cost stay out of the numbers while the full encode, compress and POST path is
still exercised. It stores bodies and decodes only after the run: decoding inline
would put protobuf work on the critical path and could slow a POST enough to
back-pressure the agent through its `sync_channel(10)` upload queue, moving cost
out of the column being measured.

**Warmup outside the measured region.** py-spy scans `/proc/<pid>/maps` and
builds a per-thread frame cache on its first samples. That one-off cost belongs
before the window, not inside it.

## The shapes, and what each is for

| shape | what it is | what it exposes |
|---|---|---|
| `cpu_single` | 1 thread, CPU-bound, depth 128 | the floor: one thread's stack to unwind |
| `cpu_multi` | N threads, CPU-bound | thread-count sensitivity: at `gil_only=False` a tick unwinds all N stacks |
| `io_bound` | N threads mostly blocked | relative cost against a workload that barely uses the CPU |
| `mixed` | half CPU, half blocked | attribution: the profile should credit the CPU threads only |
| `deep` | 256-frame stacks | per-frame unwind cost, against the 128-frame baseline |
| `churn` | threads created and destroyed | per-tick task enumeration and per-thread cached state |
| `http` | gunicorn, prefork + threads | a real server: multi-process, one agent per worker |

Plus `--sweep`, running `cpu_multi` at 1/2/4/8/16 threads. That is as much a
harness self-check as a result. It runs three variants: `none`, `cpu` at the
shipping default, and `cpu_nogil` with `gil_only=False`. The expected shapes are
opposite, and both are asserted -- `cpu` close to flat, because `gil_only` skips
non-GIL threads before unwinding, and `cpu_nogil` rising, because every thread is
unwound. The rising curve is what proves the harness can resolve a thread-count
signal at all; without it a flat curve at the default would be indistinguishable
from a blind harness.

The sweep holds *total* work constant and spreads it over more threads, so
thread count is the only thing that varies. Holding per-thread work fixed instead
would scale total work with N, making the 1-thread region too short to measure
and the 16-thread one sixteen times longer, and the overhead percentage would
stop isolating thread-count sensitivity.

## What a unit of configuration buys

`gil_only` and `oncpu` are applied at *different* points, and the difference decides how cost scales.

`gil_only` is applied by py-spy inside its per-thread loop, **before** the unwind (`python_spy.rs`):

```rust
if self.config.gil_only && !owns_gil {
    continue;                        // skipped before get_stack_trace
}
let mut trace = get_stack_trace(&thread, ...);
```

`oncpu` (`include_idle`) is applied by the consumer in `rust/src/pyspy_backend.rs`, **after** the unwind, on traces py-spy has already filtered. So at the shipping default (`gil_only=True`):

* a tick unwinds exactly **one** stack -- the GIL holder -- and costs one pointer
  read for every other thread;
* per-tick cost is therefore close to flat in thread count, measured at roughly
  1% of a core across 1 to 16 threads;
* `seen/used` well below 1 on a multi-threaded shape is the pre-walk filter
  working, not the profiler failing.

The benchmark measures `gil_only=False` instead, where every live thread is
unwound. Two consequences to read alongside those numbers:

* cost rises with thread count -- measured 2.5% of a core at 1 thread to 11.3% at
  16, on 128-frame stacks;
* `seen/used` exceeds 1. Every recorded stack is credited a full sample period,
  but on a GIL-serialised workload only one of those threads was actually
  on-CPU, so the profile necessarily claims more CPU than was spent. That is
  structural, not a harness fault, and it is reported rather than failing a run.

With `gil_only=False` every live thread is unwound and cost does scale with thread count. The sweep runs that configuration as a control, because a flat curve at the default is otherwise indistinguishable from a harness that resolves nothing.

## Reading a result

- **cpu overhead** -- the headline. `~` means below the noise floor, not ranked.
- **wall overhead** -- reference only. `n/a*` marks a shape whose region ends
  after a fixed amount of *work* rather than a fixed span (`http`), so its
  duration moves with achieved throughput. A warmer machine finishes sooner,
  which would read as a faster profiler; CPU per unit of work is the comparable
  figure there.
- **mem static delta** -- steady-state footprint the profiler adds, sampled after
  warmup and before the workload's own peak.
- **mem peak delta** -- secondary. The profiler's peak (the gzip encode buffer
  during an upload) and the workload's peak happen at different moments, so
  subtracting peaks measures neither cleanly. `upload_interval` is held constant
  across variants because that buffer scales with it.
- **seen/used** -- CPU the profile claims over CPU the process consumed while
  profiling. A correctness figure. It sits beside the cost figure because a
  profiler that drops samples is cheap, and the trade cannot be read one without
  the other.
- **seen/wall** -- the same numerator over wall time: how many cores' worth the
  profile claims. At `gil_only=True` one stack is recorded per tick, so the
  ceiling is 1.0 per agent; at `gil_only=False` it is the thread count.
- `invalid` -- the cell's runs failed a guard. Shown without a number so it
  cannot be ranked against valid cells.

### A sampler over-attributes on low-duty-cycle work

Measured, with four threads over 14 s:

| workload | cgroup CPU | profile claims | ratio |
|---|--:|--:|--:|
| CPU slice + sleep | 0.86 s | 0.95 s | 1.11 |
| CPU slice + socket + sleep | 1.34 s | 1.78 s | 1.33 |
| same, `gil_only=False` | 2.05 s | 5.11 s | 2.49 |

A tick that catches a thread holding the GIL credits it a full sample period
(10 ms at the default rate) even if it only ran for microseconds. On a busy
workload the slices are much longer than the period and the estimate converges;
on a low-duty-cycle workload it does not, and the profile reports more CPU than
the process spent.

So `seen <= cpu used` is enforced as a hard failure only above 50% duty cycle,
where an excess really is an accounting bug. Below that it is recorded as a
finding. The physical ceiling -- `seen <= wall x agents`, which no correct
implementation can exceed at `gil_only=True` -- is enforced unconditionally.

Sample *record* counts are deliberately not reported as a volume: the encoder
aggregates identical stacks, so a record count measures cardinality rather than
how much was collected. `seen/used` sums sample values instead.

## Finding: the profiler loses most of its samples on deep stacks

Measured on CPython 3.12.3 / aarch64, `gil_only=False`, one worker thread, 8 s
per depth. `probe_depth.py` reproduces it.

| nesting depth | 32 | 64 | 96 | 128 | 160 | 192 | 256 |
|---|--:|--:|--:|--:|--:|--:|--:|
| CPU spent (s) | 8.11 | 8.18 | 8.23 | 8.28 | 8.22 | 8.14 | 8.11 |
| profile claims (s) | 7.93 | 8.00 | 7.88 | 7.39 | 4.19 | 1.09 | 0.73 |
| **ratio** | 0.98 | 0.98 | 0.96 | **0.89** | **0.51** | **0.13** | **0.09** |

Between depth 128 and 192 the profile stops accounting for the process's CPU,
and by 256 it has lost about 91% of it. Two things say this is a failure rather
than the sampler working harder: CPU spent is flat across every depth, and the
count of distinct stack records is flat too. The sampler is aborting early and
cheaply.

The failures are invisible from the outside. py-spy reports a failed unwind on
`Sample.sampling_errors`, and `rust/src/pyspy_backend.rs` iterates only
`sample.traces`, so a failed sample contributes nothing and logs nothing. Nothing
in the agent counts or surfaces it.

Consequence for this benchmark: at `deep` = 256 frames the *cost* figure is not a
usable measurement, because it is the cost of a profiler that has stopped
sampling. The `profile not collapsed` guard marks that cell invalid, which is the
correct outcome -- a cheap result whose subject is broken is exactly what that
bound exists to catch. The correctness figure is the finding.

## Guards

Every reported cell has passed these. A failure marks the cell and it is not
ranked; a failed physical invariant invalidates the table, not just the row.

Run validity: subprocess exited 0, a result line was emitted, completed
iterations equal the configured count, CPU was consumed, the region lasted at
least 5 s, profiled variants uploaded samples, and the unprofiled variant
uploaded nothing.

Physical, checked in both directions:

- `seen <= cpu used` -- a profile cannot contain more CPU than the process spent.
- `seen <= wall x agents x stacks-per-tick` -- a tick can record at most one
  sample period per stack it walks. `stacks-per-tick` is 1 at `gil_only=True`
  and the thread count at `gil_only=False`.
- `profile not collapsed` -- the profile must contain at least 40% of the
  *accountable* CPU, meaning the smaller of the CPU actually spent and the
  physical ceiling. That single quantity replaces what was a table of per-shape
  floors. Under-reporting is the bound that gets missed, because unlike
  over-reporting it still looks physically possible.
- `sink kept up` -- no upload slower than `upload_interval / 2`, or the agent's
  bounded upload queue starts absorbing cost that should have been measured.

Monotonic relationships that must hold by construction:

- deeper stacks cannot be cheaper than shallow ones;
- overhead must grow with thread count across the sweep;
- overhead cannot be negative beyond the noise floor. A real negative means the
  whole table is unusable.

HTTP-specific: all requests completed, all responses 200, every worker forked
and answered (a worker that never forked never ran `post_fork` and so was never
profiled), gunicorn exited cleanly, no queueing tail, and the load generator's
measured ceiling is at least 3x the observed rate -- a saturated generator turns
server slowdown into queueing and stops measuring the server.

## Layout

| file | role |
|---|---|
| `run_bench.py` | orchestrator: matrix, round-robin repeats, aggregation, guards, output |
| `variants.py` | profiler registry -- the extension point |
| `workloads.py` | in-process shapes |
| `worker.py` | one measured run of an in-process shape |
| `runner.py` | launching a run inside a transient cgroup scope |
| `calibrate.py` | picks iteration counts so a region lasts seconds |
| `cgroup.py` | reads `cpu.stat` / `memory.current` / `memory.peak` |
| `sink.py` | local ingest sink; decodes after the run |
| `stats.py` | median and the noise figure |
| `invariants.py` | the guards |
| `report.py` | markdown and self-contained HTML |
| `run_http.py` | orchestrator for the gunicorn shape |
| `httpbench/` | WSGI app, gunicorn config with the `post_fork` hook, supervisor, load generator |
| `pprof/` | vendored pprof + push protobuf definitions and generated bindings |
| `self_check.py` | harness validation: guards, noise floor, known signal |
| `probe_depth.py` | how much of a profile survives as stack depth grows |
| `verify_cgroup.py` | checks the measurement instrument against known work |
| `bootstrap.sh`, `sync.sh`, `run_linux.sh` | build and run on the VM |

Every design choice in "Why it is built this way" above exists because the
alternative produced a confident, plausible, wrong number at some point. None of
them are hypothetical.
