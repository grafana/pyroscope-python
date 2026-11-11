"""Combine the fixed SDK results from the VM and laptop without pooling hosts."""
import csv
import json
from pathlib import Path
import statistics

root = Path(__file__).resolve().parent
rows = []
native_hashes = set()
text = [
    'CTimer overrun fix: VM and laptop benchmark comparison',
    '',
    'Applied e60886ce78175c54732ac19e2d7283ae018c9f5b from vibe/task-0740',
    'to baseline f6a0e3e25d91a4d7439f4027d11ee5aaf7f84956.',
    'Tested commit: 3638f627472fd4f326c435b1c493a3d1227d81b7.',
    'Both hosts use the same CPython 3.13.7 SDK wheel and pinned Python dependencies.',
    'Every case: fresh Docker container, six-CPU quota, six workers, six HTTP clients.',
    'gil_only=False, Signal CTimer, sample_rate=100, memory profiler disabled.',
    'Three repetitions per idle/loaded case; 15s warmup plus 30s measurement.',
    'RSS sampled every 100ms; arithmetic means across repetitions are shown.',
    '',
]
for machine in ('vm', 'laptop'):
    p = root/machine/'measured'
    validation = json.loads((p/'validation.json').read_text())
    native_hashes.add(validation['sdk_native_sha256'])
    host = json.loads((p/'host-metadata.json').read_text())
    summary = json.loads((p/'summary.json').read_text())
    lookup = {(r['implementation'], r['loaded']): r for r in summary}
    loaded_baseline = lookup['baseline', True]
    coverage = json.loads((p/'coverage.json').read_text())
    text += [f"Machine: {machine}; benchmark CPUs {host['cpuset']}; client/receiver CPUs {host['aux_cpus']}",
             'Implementation   Req/s (SD)       CPU us/req (SD)   CPU cost delta    Average RSS MiB (SD)   Highest peak RSS MiB']
    for impl in ('baseline', 'py-spy', 'stack', 'signal'):
        r = lookup[impl, True]
        idle = lookup[impl, False]
        cs = [c for c in coverage if c['implementation'] == impl and c['loaded']]
        cpu_profile = statistics.mean(c['uploaded_cpu_s'] for c in cs) if cs else None
        cpu_lower = statistics.mean(c['worker_lifecycle_cpu_lower_s'] for c in cs) if cs else None
        cpu_upper = statistics.mean(c['worker_lifecycle_cpu_upper_s'] for c in cs) if cs else None
        rows.append(dict(machine=machine, implementation=impl,
                         requests_per_s=r['requests_per_s_mean'], requests_per_s_sd=r['requests_per_s_sd'],
                         requests_per_s_median=r['requests_per_s_median'],
                         throughput_delta_percent=r['throughput_delta_percent'],
                         cpu_us_per_request=r['cpu_us_per_request_mean'], cpu_us_per_request_sd=r['cpu_us_per_request_sd'],
                         cpu_cost_delta_percent=r['cpu_cost_delta_percent'],
                         cpu_us_per_request_median=r['cpu_us_per_request_median'],
                         median_cpu_cost_delta_percent=(r['cpu_us_per_request_median']/loaded_baseline['cpu_us_per_request_median']-1)*100,
                         loaded_rss_mean_mib=r['rss_mean_mib_mean'], loaded_rss_sd_mib=r['rss_mean_mib_sd'],
                         loaded_rss_extra_mib=r['rss_extra_mib'], loaded_rss_peak_mib=r['rss_peak_mib_max'],
                         idle_cpu_seconds=idle['cpu_s_mean'], idle_cpu_seconds_sd=idle['cpu_s_sd'],
                         idle_rss_mib=idle['rss_mean_mib_mean'],
                         uploaded_cpu_seconds=cpu_profile, worker_cpu_lower_seconds=cpu_lower,
                         worker_cpu_upper_seconds=cpu_upper,
                         uploaded_cpu_percent_lower=100*cpu_profile/cpu_upper if cs else None,
                         uploaded_cpu_percent_upper=100*cpu_profile/cpu_lower if cs else None))
        text.append(f"{impl:14} {r['requests_per_s_mean']:8.1f} ({r['requests_per_s_sd']:6.1f})"
                    f" {r['cpu_us_per_request_mean']:10.2f} ({r['cpu_us_per_request_sd']:6.2f})"
                    f" {r['cpu_cost_delta_percent']:+12.2f}%"
                    f" {r['rss_mean_mib_mean']:14.2f} ({r['rss_mean_mib_sd']:5.2f})"
                    f" {r['rss_peak_mib_max']:18.2f}")
    text += ['', 'Idle: CPU seconds per 30s / average RSS MiB']
    for impl in ('baseline','py-spy','stack','signal'):
        r=lookup[impl,False]
        text.append(f"{impl:14} {r['cpu_s_mean']:.3f} +/- {r['cpu_s_sd']:.3f} CPU s; {r['rss_mean_mib_mean']:.2f} MiB")
    text += ['', 'Loaded profile weight versus observed worker lifecycle CPU (includes startup/warmup/shutdown):']
    for r in [r for r in rows if r['machine']==machine and r['implementation']!='baseline']:
        text.append(f"{r['implementation']:14} uploaded {r['uploaded_cpu_seconds']:.3f}s; "
                    f"worker CPU {r['worker_cpu_lower_seconds']:.3f}-{r['worker_cpu_upper_seconds']:.3f}s; "
                    f"ratio {r['uploaded_cpu_percent_lower']:.2f}-{r['uploaded_cpu_percent_upper']:.2f}%")
    text += [f"Validation: {validation['completed_runs']} completed runs; zero HTTP errors, panics or worker replacements.",
             f"Measured successful requests: {validation['successful_measured_requests']}; "
             f"maximum per-case quota throttling: {validation['maximum_cgroup_throttled_seconds']:.6f}s.", '']
    noisy = [r for r in rows if r['machine']==machine and r['requests_per_s_sd']/r['requests_per_s'] > .10]
    if noisy:
        individual = [json.loads(s) for s in (p/'runs.jsonl').read_text().splitlines()]
        text += ['Large run-to-run variation (all runs retained):']
        for row in noisy:
            values = [r['requests_per_s'] for r in individual if r['loaded'] and r['implementation']==row['implementation']]
            text.append(f"{row['implementation']}: measured rates " + ', '.join(f'{v:.1f}' for v in values) + ' requests/s.')
            text.append(f"Mean CPU-cost delta {row['cpu_cost_delta_percent']:+.2f}%; median CPU-cost delta {row['median_cpu_cost_delta_percent']:+.2f}%.")
        text += ['An unexplained slow run affects the means; these data do not establish a stable overhead estimate.', '']
assert len(native_hashes)==1, native_hashes
text += [f'Identical native SDK SHA256 on both machines: {next(iter(native_hashes))}', '',
         'Interpretation and limitations:',
         '- CPU cost and memory deltas are calculated against each machine own baseline.',
         '  Do not pool machines or attribute absolute performance differences solely to virtualization.',
         '- The VM used six guest CPUs. The laptop used six distinct physical cores (two P cores',
         '  and four E cores), with the load generator on other physical cores. Neither host is',
         '  globally isolated from unrelated activity. The laptop remained on AC power.',
         '- The fixed native SDK binary is identical, and runtime library/package versions are',
         '  recorded per case. Host kernels, processors and timer event behavior differ.',
         '- Total weight coverage compares against all worker-thread CPU. Timers or sampling',
         '  filters may exclude some native threads. Correct totals do not establish correct',
         '  stack attribution; the previously demonstrated phase bias is not fixed by this patch.',
         '- RSS sums the master and workers and therefore counts shared pages per process.',
         '  PSS, per-process RSS samples, lifecycle counters and all raw profiles are retained.',
         '- The applied patch preserves overruns but leaves the existing shutdown ordering.',
         '- Only this fixed-SDK suite is included above; smoke and older runs are excluded.',
         '']
(root/'comparison.txt').write_text('\n'.join(text))
(root/'comparison.json').write_text(json.dumps(rows,indent=2)+'\n')
with (root/'comparison.csv').open('w') as f:
    w=csv.DictWriter(f,fieldnames=rows[0].keys());w.writeheader();w.writerows(rows)
print('\n'.join(text))
