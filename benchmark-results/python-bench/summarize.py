"""Summarize the AC-powered runs; earlier runs are intentionally excluded."""
from collections import defaultdict
import csv
import json
from pathlib import Path
import statistics as st

root = Path(__file__).resolve().parent
measured = root / 'measured'
runs = [json.loads(line) for line in (measured / 'runs.jsonl').read_text().splitlines()]
meta = json.loads((measured / 'metadata.json').read_text())
groups = defaultdict(list)
for run in runs:
    groups[run['implementation'], run['loaded']].append(run)
assert len(runs) == 24 and all(len(v) == 3 for v in groups.values()), 'Incomplete measurement suite'
assert all(r['errors'] == r['panics'] == r['server_exit'] == 0 for r in runs)
assert all(r['power_start']['/sys/class/power_supply/AC/online'] == '1'
           and r['power_end']['/sys/class/power_supply/AC/online'] == '1' for r in runs)

metrics = ['user_s', 'system_s', 'cpu_s', 'cpu_cores', 'requests_per_s',
           'cpu_us_per_request', 'rss_end_mib', 'profile_uploads', 'profile_bytes']
summary = []
for loaded in (False, True):
    for impl in ('baseline', 'py-spy', 'stack', 'signal'):
        group = groups[impl, loaded]
        row = {'implementation': impl, 'workload': 'loaded' if loaded else 'idle', 'n': len(group)}
        for metric in metrics:
            values = [r[metric] for r in group if r[metric] is not None]
            for name, value in [('mean', st.mean(values) if values else None),
                                ('median', st.median(values) if values else None),
                                ('stdev', st.stdev(values) if len(values) > 1 else None),
                                ('min', min(values) if values else None),
                                ('max', max(values) if values else None)]:
                row[metric + '_' + name] = value
        summary.append(row)
with (root / 'summary.csv').open('w') as f:
    w = csv.DictWriter(f, fieldnames=summary[0].keys())
    w.writeheader()
    w.writerows(summary)
(root / 'summary.json').write_text(json.dumps(summary, indent=2))

lookup = {(r['implementation'], r['workload']): r for r in summary}
baseline = lookup['baseline', 'loaded']
text = [
    'Python profiler benchmark comparison (AC-powered runs only)',
    '',
    f"Profiler: grafana/pyroscope-python main {meta['profiler_commit']}",
    f"Benchmark: grafana/pyroscope python-bench {meta['bench_commit']}",
    f"Started: {meta['started_at']}",
    f"Runtime: {meta['python'].splitlines()[0]}",
    f"Platform: {meta['platform']}; libc {meta['libc']}",
    'CPU: Intel Core i7-1365U; 10 physical cores, 12 logical CPUs.',
    'AC was online at both measurement boundaries for every included run.',
    '',
    'Loaded results: arithmetic mean of three 30-second measurements.',
    'CPU includes the Gunicorn master and all 12 workers, including profiler threads.',
    'CPU cost is normalized by successful requests; lower is better.',
    '',
    'Implementation   Requests/s (SD)     vs baseline   CPU us/request (SD)   overhead    CPU cores   RSS MiB*',
]
for impl in ('baseline', 'py-spy', 'stack', 'signal'):
    row = lookup[impl, 'loaded']
    rps = row['requests_per_s_mean']
    cost = row['cpu_us_per_request_mean']
    throughput_delta = (rps / baseline['requests_per_s_mean'] - 1) * 100
    overhead = (cost / baseline['cpu_us_per_request_mean'] - 1) * 100
    text.append(f"{impl:14} {rps:9.1f} ({row['requests_per_s_stdev']:6.1f})  {throughput_delta:+9.2f}%"
                f"    {cost:8.1f} ({row['cpu_us_per_request_stdev']:5.1f})    {overhead:+7.2f}%"
                f"    {row['cpu_cores_mean']:7.3f}    {row['rss_end_mib_mean']:7.1f}")
text += ['', 'Idle results: mean process CPU time over 30 seconds across 12 workers plus master.',
         'Implementation    User s     System s   Total CPU s (SD)   Average cores   Extra cores vs baseline']
idle_baseline = lookup['baseline', 'idle']['cpu_cores_mean']
for impl in ('baseline', 'py-spy', 'stack', 'signal'):
    row = lookup[impl, 'idle']
    text.append(f"{impl:14} {row['user_s_mean']:8.3f}   {row['system_s_mean']:8.3f}"
                f"    {row['cpu_s_mean']:8.3f} ({row['cpu_s_stdev']:.3f})"
                f"      {row['cpu_cores_mean']:8.4f}         {row['cpu_cores_mean']-idle_baseline:+8.4f}")
text += ['', 'Per-repeat loaded results (allows checking run-to-run variation):',
         'Repeat  Implementation   Requests/s   CPU us/request   CPU seconds']
for r in sorted((r for r in runs if r['loaded']), key=lambda r: (r['repeat'], r['implementation'])):
    text.append(f"{r['repeat']:6}  {r['implementation']:14} {r['requests_per_s']:10.1f}"
                f" {r['cpu_us_per_request']:16.1f} {r['cpu_s']:13.2f}")
text += ['', 'Loaded medians (all three runs retained; no outlier exclusion):',
         'Implementation   Median req/s   Median CPU us/req   CPU cost vs baseline']
for impl in ('baseline', 'py-spy', 'stack', 'signal'):
    row = lookup[impl, 'loaded']
    delta = (row['cpu_us_per_request_median'] / baseline['cpu_us_per_request_median'] - 1)*100
    text.append(f"{impl:14} {row['requests_per_s_median']:12.1f}"
                f" {row['cpu_us_per_request_median']:19.1f} {delta:+21.2f}%")
text += [
    '',
    'Method and scope:',
    '- The branch Django home-page rendering workload is unchanged. Each run starts',
    '  a fresh Gunicorn master with 12 synchronous workers, waits for every worker to',
    '  finish application/profiler initialization, then warms up for 5 seconds.',
    '- Loaded runs use 12 concurrent closed-loop Go clients requesting GET /. The',
    '  branch load generator was adapted to expose interval counters and count only',
    '  successful 2xx responses. Idle runs perform only the startup readiness request.',
    '- Cases are shuffled within each repeat with seed 20261109; execution is sequential.',
    '- All profilers use sample_rate=100, oncpu=True, gil_only=True, memory profiling',
    '  disabled, upload_interval=10, and otherwise the SDK defaults. Signal uses ITimer.',
    '  Stack keeps its default per-thread 10 ms CPU timer and adaptive wall sampler.',
    '  These are default implementation comparisons, not identical sampling mechanisms.',
    '- A local HTTP sink acknowledges and saves gzip protobuf uploads. Upload encoding',
    '  and sending remain enabled. Backend ingestion/query costs are not measured.',
    '  The load generator and sink share the machine but are outside measured CPU/RSS.',
    '- Profiles include startup, warmup and shutdown; their aggregate weights should',
    '  not be equated with CPU time in just the 30-second measurement window.',
    '- The old benchmark dependency pins predate CPython 3.13. This run uses Django',
    '  5.2.8, Django REST Framework 3.16.1 and Gunicorn 23.0.0 for every implementation',
    '  and baseline. The unused PostgreSQL driver was omitted (SQLite is configured).',
    '  Full resolved versions are in requirements.lock.txt and measured/metadata.json.',
    '- SDK was built from the unmodified main source in release mode with Rust 1.99.1,',
    '  Clang 22.1.8 and system OpenSSL 3.5.9. Explicit GCC 16 search paths were needed',
    '  for this host Clang installation; see build-final.log and reproduce.sh.',
    '- All 24 included runs had zero HTTP errors, zero detected Rust panics, stable',
    '  worker PIDs, and clean Gunicorn/load-generator exits. Every profiled run uploaded',
    '  profiles. Earlier smoke and interrupted measurements are excluded and retained',
    '  separately in excluded-before-ac-restart/.',
    '- This is a shared laptop with frequency scaling, heterogeneous cores and no CPU',
    '  isolation. Three short repetitions do not establish statistically significant',
    '  differences of a few percent. SD reports variation across runs, not a confidence',
    '  interval. Results apply to this Django workload and runtime configuration.',
    '- *RSS is the sum of process RSS, so shared pages may be counted more than once.',
    '',
    'Files:',
    '  measured/runs.csv, measured/runs.jsonl: individual measurements',
    '  summary.csv, summary.json: group means, standard deviations and ranges',
    '  measured/profiles/: raw uploaded protobuf bodies',
    '  measured/decoded-profiles.jsonl.gz: complete decoded profiles with frame sequences',
    '  measured/complete-stacks.json.gz: aggregated full root-to-leaf stacks and weights',
    '  measured/profile-summary.json: sample types and total profile weights per case',
    '  measured/*.log: complete Gunicorn and load generator logs',
    '  benchmark.patch, run_benchmark.py, loadgen.go, reproduce.sh: reproducible harness',
    '',
]
(root / 'comparison.txt').write_text('\n'.join(text))
print('\n'.join(text[:32]))
