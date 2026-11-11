import argparse
import csv
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import sys
import time
import urllib.request

import psutil

p = argparse.ArgumentParser()
p.add_argument('--implementation', required=True)
p.add_argument('--loaded', type=int, required=True)
p.add_argument('--duration', type=float, default=30)
p.add_argument('--warmup', type=float, default=15)
p.add_argument('--repeat', type=int, required=True)
a = p.parse_args()
out = Path('/results')


def get_stats():
    with urllib.request.urlopen('http://127.0.0.1:18082/stats', timeout=3) as r:
        return json.load(r)


def cpu(procs):
    values = [p.cpu_times() for p in procs]
    return sum(v.user for v in values), sum(v.system for v in values)


def memory(procs):
    return [p.memory_info().rss / 1048576 for p in procs]


def proportional_memory(procs):
    return sum(p.memory_full_info().pss for p in procs) / 1048576


def cgroup_cpu():
    return {k: int(v) for k, v in (line.split() for line in Path('/sys/fs/cgroup/cpu.stat').read_text().splitlines())}


def interrupted(signum, frame):
    raise KeyboardInterrupt


signal.signal(signal.SIGTERM, interrupted)
env = os.environ.copy()
env.update(DEBUG='0', SECRET_KEY='benchmark-only', DJANGO_ALLOWED_HOSTS='localhost 127.0.0.1',
           PYROSCOPE_BENCHMARK='false' if a.implementation == 'baseline' else 'true',
           PYROSCOPE_BENCHMARK_IMPL=a.implementation, PYROSCOPE_BENCHMARK_SAMPLERATE='100',
           PYROSCOPE_BENCHMARK_SERVER='http://127.0.0.1:18081', RUST_BACKTRACE='1')
native_path = next(Path('/usr/local/lib/python3.13/site-packages/pyroscope').glob('_native*.so'))
metadata = dict(sdk_native_sha256=hashlib.sha256(native_path.read_bytes()).hexdigest(), python=sys.version, platform=platform.platform(), libc=platform.libc_ver(),
                source=json.loads(Path('/harness/source.json').read_text()),
                affinity=sorted(os.sched_getaffinity(0)), cpu_max=Path('/sys/fs/cgroup/cpu.max').read_text().strip(),
                cpuset=Path('/sys/fs/cgroup/cpuset.cpus.effective').read_text().strip(),
                workers=6, clients=6, sample_rate=100, gil_only=False, signal_timer='CTimer',
                wsgi_sha256=hashlib.sha256(Path('/app/hello_django/wsgi.py').read_bytes()).hexdigest(), warmup_s=a.warmup, duration_s=a.duration,
                packages={d.metadata['Name']: d.version for d in importlib.metadata.distributions()},
                openssl=subprocess.check_output(['openssl', 'version'], text=True).strip())
(out / 'metadata.json').write_text(json.dumps(metadata, indent=2))
log_path = out / 'gunicorn.log'
server = None
with log_path.open('w') as log:
    try:
        server = subprocess.Popen(['gunicorn', '--workers=6', '--bind=127.0.0.1:18080',
                                   'hello_django.wsgi'], env=env, stdout=log,
                                  stderr=subprocess.STDOUT, start_new_session=True)
        parent = psutil.Process(server.pid)
        deadline = time.monotonic() + 60
        while True:
            if server.poll() is not None:
                raise RuntimeError(f'Gunicorn exited {server.returncode}')
            if len(parent.children()) == 6 and log_path.read_text().count('BENCH_WORKER_READY') == 6:
                try:
                    with urllib.request.urlopen('http://127.0.0.1:18080/', timeout=2) as r:
                        if r.status == 200:
                            break
                except OSError:
                    pass
            if time.monotonic() > deadline:
                raise TimeoutError('Workers did not initialize')
            time.sleep(.1)
        settings = [json.loads(line.removeprefix('BENCH_PROFILER_SETTINGS ')) for line in log_path.read_text().splitlines() if line.startswith('BENCH_PROFILER_SETTINGS ')]
        if a.implementation != 'baseline':
            assert len(settings) == 6 and all(s['gil_only'] is False and s['signal_timer'] == 'CTimer' and s['implementation'] == a.implementation for s in settings), settings
        procs = [parent] + parent.children()
        pids = [p.pid for p in procs]
        time.sleep(a.warmup)
        pss_start = proportional_memory(procs)
        counts_start = get_stats() if a.loaded else dict(success=0, errors=0)
        cg_start = cgroup_cpu()
        user_start, sys_start = cpu(procs)
        start = time.monotonic()
        wall_start = time.time()
        timeline = []
        while True:
            rss = memory(procs)
            elapsed = time.monotonic() - start
            timeline.append(dict(elapsed_s=elapsed, rss_mib=sum(rss),
                                 master_rss_mib=rss[0], workers_rss_mib=sum(rss[1:]),
                                 process_rss_mib=rss))
            if elapsed >= a.duration:
                break
            time.sleep(min(.1, a.duration - elapsed))
        user_end, sys_end = cpu(procs)
        elapsed = time.monotonic() - start
        cg_end = cgroup_cpu()
        counts_end = get_stats() if a.loaded else counts_start
        pss_end = proportional_memory(procs)
        assert set(p.pid for p in parent.children()) == set(pids[1:]), 'Worker replacement'
        requests = counts_end['success'] - counts_start['success']
        errors = counts_end['errors'] - counts_start['errors']
        user_s, system_s = user_end-user_start, sys_end-sys_start
        rss_integral = sum((x['rss_mib']+y['rss_mib'])/2 * (y['elapsed_s']-x['elapsed_s'])
                           for x, y in zip(timeline, timeline[1:]))
        result = dict(implementation=a.implementation, loaded=bool(a.loaded), repeat=a.repeat,
                      elapsed_s=elapsed, measurement_start_unix=wall_start, measurement_end_unix=time.time(),
                      user_s=user_s, system_s=system_s, cpu_s=user_s+system_s,
                      cpu_cores=(user_s+system_s)/elapsed, requests=requests, errors=errors,
                      requests_per_s=requests/elapsed,
                      cpu_us_per_request=(user_s+system_s)*1e6/requests if requests else None,
                      rss_start_mib=timeline[0]['rss_mib'], rss_end_mib=timeline[-1]['rss_mib'],
                      rss_mean_mib=rss_integral/(timeline[-1]['elapsed_s']-timeline[0]['elapsed_s']),
                      rss_peak_mib=max(r['rss_mib'] for r in timeline), rss_samples=len(timeline),
                      pss_start_mib=pss_start, pss_end_mib=pss_end, worker_pids=pids[1:],
                      cgroup_cpu_delta={k: cg_end[k]-v for k,v in cg_start.items()})
        (out / 'rss-timeline.json').write_text(json.dumps(timeline))
        with (out / 'rss-timeline.csv').open('w') as f:
            w = csv.DictWriter(f, fieldnames=timeline[0].keys()); w.writeheader(); w.writerows(timeline)
    finally:
        if server is not None and server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=30)
            except subprocess.TimeoutExpired:
                os.killpg(server.pid, signal.SIGKILL)
                server.wait()
                raise RuntimeError('Gunicorn needed SIGKILL')
lifecycle = [json.loads(line.removeprefix('BENCH_PROFILE_CPU ')) for line in log_path.read_text().splitlines() if line.startswith('BENCH_PROFILE_CPU ')]
if a.implementation != 'baseline':
    assert len(lifecycle) == 6 and all(r["shutdown_succeeded"] for r in lifecycle), lifecycle
result.update(server_exit=server.returncode, panics=log_path.read_text().count('panicked at'),
              gil_only=False, signal_timer='CTimer', lifecycle_cpu=lifecycle,
              profile_cpu_lower_bound_s=sum(r['cpu_before_shutdown']-r['cpu_after_configure'] for r in lifecycle) if lifecycle else None,
              profile_cpu_upper_bound_s=sum(r['cpu_after_shutdown']-r['cpu_before_configure'] for r in lifecycle) if lifecycle else None)
(out / 'result.json').write_text(json.dumps(result, indent=2))
assert result['server_exit'] == result['panics'] == result['errors'] == 0, result
print(json.dumps(result), flush=True)
