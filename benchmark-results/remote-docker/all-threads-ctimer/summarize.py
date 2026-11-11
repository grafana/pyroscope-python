from collections import defaultdict
import csv
import json
from pathlib import Path
import statistics as st
import sys

root = Path(sys.argv[1])
runs = [json.loads(s) for s in (root/'runs.jsonl').read_text().splitlines()]
groups = defaultdict(list)
for r in runs:
    groups[r['implementation'], r['loaded']].append(r)
assert len(runs) == 24 and all(len(g) == 3 for g in groups.values())
assert all(r['errors'] == r['panics'] == r['server_exit'] == 0 for r in runs)
metrics = ['requests_per_s', 'cpu_us_per_request', 'user_s', 'system_s', 'cpu_s', 'cpu_cores',
           'rss_start_mib', 'rss_end_mib', 'rss_mean_mib', 'rss_peak_mib', 'pss_end_mib']
summary = []
for loaded in (False, True):
    for impl in ('baseline', 'py-spy', 'stack', 'signal'):
        row = dict(implementation=impl, loaded=loaded, repeats=3)
        for metric in metrics:
            vals = [r[metric] for r in groups[impl, loaded] if r[metric] is not None]
            for name, fn in [('mean', st.mean), ('median', st.median), ('sd', st.stdev), ('min', min), ('max', max)]:
                row[f'{metric}_{name}'] = fn(vals) if vals else None
        summary.append(row)
lookup = {(r['implementation'], r['loaded']): r for r in summary}
for row in summary:
    base = lookup['baseline', row['loaded']]
    row['rss_extra_mib'] = row['rss_mean_mib_mean'] - base['rss_mean_mib_mean']
    row['rss_extra_percent'] = (row['rss_mean_mib_mean']/base['rss_mean_mib_mean']-1)*100
    row['cpu_cost_delta_percent'] = ((row['cpu_us_per_request_mean']/base['cpu_us_per_request_mean']-1)*100
                                     if row['loaded'] else None)
    row['throughput_delta_percent'] = ((row['requests_per_s_mean']/base['requests_per_s_mean']-1)*100
                                       if row['loaded'] else None)
for filename, data in [('runs.csv', runs), ('summary.csv', summary)]:
    with (root/filename).open('w') as f:
        w = csv.DictWriter(f, fieldnames=data[0].keys()); w.writeheader(); w.writerows(data)
(root/'summary.json').write_text(json.dumps(summary, indent=2))
host = json.loads((root/'host-metadata.json').read_text())
runtime = json.loads((root/runs[0]['case']/'metadata.json').read_text())
profiled = ('py-spy', 'stack', 'signal')
lowest_rss = min(profiled, key=lambda name: lookup[name, True]['rss_mean_mib_mean'])
lowest_idle_cpu = min(profiled, key=lambda name: lookup[name, False]['cpu_s_mean'])
cost_deltas = ', '.join(f"{name}: {lookup[name, True]['cpu_cost_delta_percent']:+.2f}%" for name in profiled)
rss_deltas = ', '.join(f"{name}: {lookup[name, True]['rss_extra_mib']:+.2f} MiB" for name in profiled)
max_throughput_delta = max(abs(lookup[name, True]['throughput_delta_percent']) for name in profiled)
text = ['Remote Docker Python profiler benchmark', '',
        'Machine: user@5950x-vm-vibe.tail437fc2.ts.net',
        f"Profiler main: {runtime['source']['profiler_commit']}",
        f"Benchmark python-bench: {runtime['source']['benchmark_commit']}",
        f"Python: {runtime['python'].splitlines()[0]}",
        f"libc: {runtime['libc']}; {runtime['openssl']}",
        f"Image: {host['image'][0]['Id']}",
        'Each measurement: fresh Docker container, --cpus=6 --cpuset-cpus=0-5, 6 workers.',
        'Three repeats of each case, 15 s warmup then 30 s measured; 100 Hz SDK setting.',
        '', 'Findings for this workload and configuration:',
        f'- Lowest loaded average RSS: {lowest_rss}. Lowest idle CPU use: {lowest_idle_cpu}.',
        f'- Loaded CPU cost per request versus baseline: {cost_deltas}.',
        '  Small differences should be read',
        '  alongside the between-run SDs below; three repetitions do not establish',
        '  statistical significance for the smaller deltas.',
        f'- Loaded average RSS versus baseline: {rss_deltas}.',
        '  These totals cover the master plus six workers.',
        f'- Largest absolute mean throughput delta versus baseline: {max_throughput_delta:.2f}%.',
        '  Comparing small throughput differences requires more repetitions.',
        '', 'Loaded comparison (mean across repetitions; SD is between-run standard deviation):',
        'Profiler       Requests/s (SD)      Throughput delta   CPU us/req (SD)   CPU cost delta']
for impl in ('baseline', 'py-spy', 'stack', 'signal'):
    r = lookup[impl, True]
    text.append(f"{impl:12} {r['requests_per_s_mean']:9.1f} ({r['requests_per_s_sd']:6.1f})"
                f" {r['throughput_delta_percent']:+15.2f}%"
                f"  {r['cpu_us_per_request_mean']:9.2f} ({r['cpu_us_per_request_sd']:5.2f})"
                f" {r['cpu_cost_delta_percent']:+12.2f}%")
for loaded in (False, True):
    text += ['', ('Loaded' if loaded else 'Idle') + ' memory comparison (MiB, master + six workers):',
             'Profiler       Average RSS (SD)    Extra RSS   Extra RSS %   Mean run peak   Highest peak   End PSS']
    for impl in ('baseline', 'py-spy', 'stack', 'signal'):
        r = lookup[impl, loaded]
        text.append(f"{impl:12} {r['rss_mean_mib_mean']:8.2f} ({r['rss_mean_mib_sd']:5.2f})"
                    f" {r['rss_extra_mib']:+10.2f} {r['rss_extra_percent']:+12.2f}%"
                    f" {r['rss_peak_mib_mean']:14.2f} {r['rss_peak_mib_max']:14.2f}"
                    f" {r['pss_end_mib_mean']:9.2f}")
text += ['', 'Idle CPU use (CPU seconds per 30-second window, master + six workers):',
         'Profiler        User s    System s    Total s (SD)     Average CPU cores']
for impl in ('baseline', 'py-spy', 'stack', 'signal'):
    r = lookup[impl, False]
    text.append(f"{impl:12} {r['user_s_mean']:8.3f} {r['system_s_mean']:11.3f}"
                f" {r['cpu_s_mean']:10.3f} ({r['cpu_s_sd']:.3f}) {r['cpu_cores_mean']:16.4f}")
text += ['', 'All loaded repetitions:', 'Repeat Profiler     Requests/s    CPU us/req    Average RSS MiB   Peak RSS MiB']
for r in sorted((r for r in runs if r['loaded']), key=lambda r: (r['repeat'],r['implementation'])):
    text.append(f"{r['repeat']:6} {r['implementation']:10} {r['requests_per_s']:11.1f}"
                f" {r['cpu_us_per_request']:13.2f} {r['rss_mean_mib']:18.2f} {r['rss_peak_mib']:14.2f}")
text += ['', 'Method and limitations:',
         '- All 24 measurements passed: zero HTTP errors, zero detected Rust panics,',
         '  no worker replacements, clean process exits. Every profiled case uploaded data.',
         '- Same Django home-page rendering workload as python-bench. Its old dependency',
         '  pins were replaced with CPython 3.13-compatible Django 5.2.8, REST Framework',
         '  3.16.1 and Gunicorn 23.0.0. All cases use the identical image and application.',
         '- Six concurrent Go HTTP clients run outside the container. The load generator',
         '  and profile receiver are pinned to CPUs 6-9; benchmark containers to CPUs 0-5.',
         '  Host networking avoids Docker port forwarding overhead. No extra capabilities',
         '  or privileged containers are used. CPU limits are saved in docker-inspect.json.',
         '- The container CPU quota covers the application, profiler threads and memory',
         '  monitor. Reported CPU/RSS cover only Gunicorn master and workers, excluding',
         '  the monitor, client and receiver. Cgroup throttling counters are in runs.jsonl.',
         '- RSS is summed process resident memory sampled every 100 ms during the measured',
         '  window. Average RSS is time-weighted; peak RSS is the highest sampled concurrent',
         '  sum (not the sum of each process lifetime high-water marks). Brief spikes between',
         '  samples can be missed. RSS counts shared pages per process; end-of-window PSS',
         '  apportions shared pages and is provided as a complementary measurement.',
         '- Profiler settings: sample_rate=100, oncpu=True, gil_only=False, no memory profiler,',
         '  10 s uploads. Signal explicitly uses CTimer; Stack uses default per-thread 10 ms',
         '  CPU timer and adaptive wall sampler. Sampling mechanisms are not identical.',
         '- Inspect uploaded CPU weights in profile-summary.json when comparing sampling',
         '  coverage. This overhead comparison does not establish equivalent coverage or',
         '  profile accuracy. Full profiles, frame sequences and weights are retained.',
         '- Profile uploads are acknowledged and saved by a local HTTP sink. Encoding and',
         '  transmission are enabled; backend ingestion costs are outside this benchmark.',
         '- Initialization happens in each worker after Gunicorn forks. Measurement starts',
         '  after all six workers initialize and the warmup finishes. Profiles also include',
         '  startup, warmup and shutdown, so their totals cover more than the timed window.',
         '- Case order is shuffled per repetition with seed 20261109. Runs are sequential.',
         '  Small differences within the observed spread are not a reliable ranking.',
         '- This is a VM, so six pinned guest CPUs do not guarantee exclusive physical cores.',
         '  These results are specific to this application, Python version and configuration.',
         '- Laptop measurements and remote smoke runs are excluded from these statistics.',
         '', 'Artifacts:',
         '  runs.jsonl / runs.csv: individual CPU, throughput and memory measurements',
         '  summary.json / summary.csv: means, medians, SDs, ranges and baseline deltas',
         '  <case>/rss-timeline.csv: every memory sample, including individual process RSS',
         '  <case>/metadata.json / docker-inspect.json: runtime and actual container limits',
         '  <case>/*.log: application, container and client output',
         '  profiles/: original compressed protobuf uploads',
         '  decoded-profiles.jsonl.gz / complete-stacks.json.gz: full frames and weights',
         '']
(root/'comparison.txt').write_text('\n'.join(text))
print('\n'.join(text[:48]))
