"""Run the python-bench Django workload with each profiler and a baseline."""
import argparse
import csv
import http.server
import importlib.metadata
import json
import os
from pathlib import Path
import platform
import random
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.request

import psutil

ARGS = argparse.ArgumentParser()
ARGS.add_argument('--app', required=True, type=Path)
ARGS.add_argument('--output', required=True, type=Path)
ARGS.add_argument('--duration', type=float, default=30)
ARGS.add_argument('--warmup', type=float, default=5)
ARGS.add_argument('--repeats', type=int, default=3)
ARGS.add_argument('--smoke', action='store_true')
args = ARGS.parse_args()
out = args.output.resolve()
out.mkdir(parents=True, exist_ok=True)
uploads = []
upload_lock = threading.Lock()
current_case = 'startup'


def power_state():
    return {str(p): p.read_text().strip()
            for pattern in ('*/online', '*/status')
            for p in Path('/sys/class/power_supply').glob(pattern)}


class Sink(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        data = self.rfile.read(int(self.headers['Content-Length']))
        with upload_lock:
            index = len(uploads)
            path = out / 'profiles' / current_case / f'{index:05d}.bin'
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            uploads.append({'case': current_case, 'path': str(path.relative_to(out)),
                            'url': self.path, 'headers': dict(self.headers),
                            'bytes': len(data), 'time': time.time()})
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'{}')

    def log_message(self, *a):
        pass


def get(url):
    with urllib.request.urlopen(url, timeout=3) as response:
        return response.read()


def wait_until(check, process, timeout=60):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f'Process exited early: {process.returncode}')
        try:
            if check():
                return
        except (OSError, ValueError):
            pass
        time.sleep(.1)
    raise TimeoutError('Service did not become ready')


def stop(process):
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=30)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()
        raise RuntimeError('Process required SIGKILL')


def snapshot(processes):
    times = [p.cpu_times() for p in processes]
    return {'user': sum(t.user for t in times), 'system': sum(t.system for t in times),
            'rss_mib': sum(p.memory_info().rss for p in processes) / 1024**2}


def run(implementation, loaded, repeat):
    global current_case
    current_case = f'{repeat}-{implementation}-' + ('load' if loaded else 'idle')
    print(f'START {current_case}', flush=True)
    env = os.environ.copy()
    env.update(DEBUG='0', SECRET_KEY='benchmark-only',
               DJANGO_ALLOWED_HOSTS='localhost 127.0.0.1',
               PYROSCOPE_BENCHMARK='false' if implementation == 'baseline' else 'true',
               PYROSCOPE_BENCHMARK_IMPL=implementation,
               PYROSCOPE_BENCHMARK_SAMPLERATE='100',
               PYROSCOPE_BENCHMARK_SERVER='http://127.0.0.1:18081',
               PYTHONUNBUFFERED='1', RUST_BACKTRACE='1')
    command = [str(Path(sys.executable).parent / 'gunicorn'), '--workers=12',
               '--bind=127.0.0.1:18080', 'hello_django.wsgi']
    server = load = None
    log_path = out / f'{current_case}.log'
    before_uploads = len(uploads)
    with log_path.open('w') as log, (out / f'{current_case}-loadgen.log').open('w') as loadlog:
        try:
            server = subprocess.Popen(command, cwd=args.app, env=env, stdout=log,
                                      stderr=subprocess.STDOUT, start_new_session=True)
            parent = psutil.Process(server.pid)
            wait_until(lambda: len(parent.children()) == 12 and get('http://127.0.0.1:18080/'), server)
            # Every worker must finish initialization, including profiler startup.
            wait_until(lambda: log_path.read_text().count('BENCH_WORKER_READY') == 12, server)
            children = parent.children()
            processes = [parent] + children
            child_pids = {p.pid for p in children}
            if loaded:
                load = subprocess.Popen([str(out.parent / 'loadgen'), '-nworkers=12'],
                                        stdout=loadlog, stderr=subprocess.STDOUT,
                                        start_new_session=True)
                wait_until(lambda: get('http://127.0.0.1:18082/stats'), load)
            time.sleep(args.warmup)
            counts1 = json.loads(get('http://127.0.0.1:18082/stats')) if loaded else {'success': 0, 'errors': 0}
            start = snapshot(processes)
            t0 = time.monotonic()
            wall_start = time.time()
            power_start = power_state()
            loadavg = os.getloadavg()
            time.sleep(args.duration)
            end = snapshot(processes)
            elapsed = time.monotonic() - t0
            counts2 = json.loads(get('http://127.0.0.1:18082/stats')) if loaded else counts1
            if {p.pid for p in parent.children()} != child_pids:
                raise RuntimeError('Worker set changed during measurement')
            if load is not None and load.poll() is not None:
                raise RuntimeError('Load generator exited during measurement')
            requests = counts2['success'] - counts1['success']
            errors = counts2['errors'] - counts1['errors']
            user = end['user'] - start['user']
            system = end['system'] - start['system']
            result = dict(case=current_case, implementation=implementation, loaded=loaded,
                          measurement_start_unix=wall_start,
                          measurement_end_unix=time.time(),
                          power_start=power_start, power_end=power_state(),
                          repeat=repeat, elapsed_s=elapsed, user_s=user, system_s=system,
                          cpu_s=user+system, cpu_cores=(user+system)/elapsed,
                          requests=requests, errors=errors, requests_per_s=requests/elapsed,
                          cpu_us_per_request=(user+system)*1e6/requests if requests else None,
                          rss_start_mib=start['rss_mib'], rss_end_mib=end['rss_mib'],
                          loadavg_start=list(loadavg), worker_pids=sorted(child_pids))
        finally:
            try:
                stop(load)
            finally:
                stop(server)
    log_text = log_path.read_text()
    result.update(profile_uploads=len(uploads)-before_uploads,
                  profile_bytes=sum(u['bytes'] for u in uploads[before_uploads:]),
                  panics=log_text.count('panicked at'),
                  server_exit=server.returncode,
                  loadgen_exit=load.returncode if load else None)
    if result['panics'] or errors or result['server_exit'] != 0:
        raise RuntimeError(f'Unhealthy run: {result}')
    if implementation != 'baseline' and not result['profile_uploads']:
        raise RuntimeError(f'No profiles received: {current_case}')
    print(json.dumps(result), flush=True)
    with (out / 'runs.jsonl').open('a') as f:
        f.write(json.dumps(result) + '\n')
    (out / 'uploads.json').write_text(json.dumps(uploads, indent=2))
    return result


for port in (18080, 18081, 18082):
    with socket.socket() as s:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(('127.0.0.1', port))
sink = http.server.ThreadingHTTPServer(('127.0.0.1', 18081), Sink)
threading.Thread(target=sink.serve_forever, daemon=True).start()
metadata = dict(python=sys.version, executable=sys.executable, platform=platform.platform(),
                power=power_state(),
                governors={str(p): p.read_text().strip() for p in Path('/sys/devices/system/cpu').glob('cpu*/cpufreq/scaling_governor')},
                libc=platform.libc_ver(), affinity=sorted(os.sched_getaffinity(0)),
                cpu_count=os.cpu_count(), workers=12, clients=12, sample_rate=100,
                duration_s=args.duration, warmup_s=args.warmup, repeats=args.repeats,
                packages={d.metadata['Name']: d.version for d in importlib.metadata.distributions()},
                profiler_commit=subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
                bench_commit=subprocess.check_output(['git', '-C', str(args.app), 'rev-parse', 'HEAD'], text=True).strip(),
                started_at=time.strftime('%Y-%m-%dT%H:%M:%S%z'))
(out / 'metadata.json').write_text(json.dumps(metadata, indent=2))
rng = random.Random(20261109)
results = []
try:
    for repeat in range(1, args.repeats + 1):
        cases = [(impl, loaded) for impl in ('baseline', 'py-spy', 'stack', 'signal')
                 for loaded in (False, True) if not args.smoke or loaded]
        rng.shuffle(cases)
        for impl, loaded in cases:
            results.append(run(impl, loaded, repeat))
finally:
    sink.shutdown()
    (out / 'uploads.json').write_text(json.dumps(uploads, indent=2))
if results:
    with (out / 'runs.csv').open('w') as f:
        writer = csv.DictWriter(f, fieldnames=results[0].keys())
        writer.writeheader()
        writer.writerows(results)
print('COMPLETE', flush=True)
