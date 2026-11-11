import argparse
import http.server
import json
import os
from pathlib import Path
import random
import signal
import socket
import subprocess
import threading
import time

p = argparse.ArgumentParser()
p.add_argument('--output', required=True, type=Path)
p.add_argument('--smoke', action='store_true')
p.add_argument('--machine', choices=('vm','laptop'), required=True)
p.add_argument('--image', default='pyroscope-python-bench:fixed-3638f62')
p.add_argument('--cpuset', default='0-5')
p.add_argument('--aux-cpus', default='6-9')
a = p.parse_args()
root = Path(__file__).resolve().parent
out = a.output.resolve()
out.mkdir(parents=True, exist_ok=False)
(out / 'controller.pid').write_text(str(os.getpid()))
def cpu_list(value):
    result = set()
    for part in value.split(','):
        bounds = part.split('-')
        result.update(range(int(bounds[0]), int(bounds[-1])+1))
    return result

assert len(cpu_list(a.cpuset)) == 6
assert not cpu_list(a.cpuset) & cpu_list(a.aux_cpus)
os.sched_setaffinity(0, cpu_list(a.aux_cpus))

def host_state():
    files = list(Path('/sys/class/power_supply').glob('*/online')) + list(Path('/sys/class/power_supply').glob('*/status')) + list(Path('/sys/class/thermal').glob('thermal_zone*/temp'))
    result = {}
    for path in files:
        try:
            result[str(path)] = path.read_text().strip()
        except OSError as error:
            result[str(path)] = str(error)
    return result

if a.machine == 'laptop':
    assert Path('/sys/class/power_supply/AC/online').read_text().strip() == '1', 'AC power required'
power_history = []
power_done = threading.Event()
def monitor_power():
    while not power_done.is_set():
        power_history.append(dict(time=time.time(), state=host_state()))
        power_done.wait(1)
threading.Thread(target=monitor_power, daemon=True).start()
uploads = []
upload_lock = threading.Lock()
current_case = None


class Sink(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers['Content-Length']))
        with upload_lock:
            path = out / 'profiles' / current_case / f'{len(uploads):05d}.bin'
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(body)
            uploads.append(dict(case=current_case, path=str(path.relative_to(out)), url=self.path,
                                headers=dict(self.headers), bytes=len(body), time=time.time()))
        self.send_response(200)
        self.end_headers()

    def log_message(self, *a):
        pass


def command(*args):
    return subprocess.check_output(args, text=True)


def interrupted(signum, frame):
    raise KeyboardInterrupt


signal.signal(signal.SIGTERM, interrupted)
for port in (18080, 18081, 18082):
    with socket.socket() as s:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(('127.0.0.1', port))
sink = http.server.ThreadingHTTPServer(('127.0.0.1', 18081), Sink)
threading.Thread(target=sink.serve_forever, daemon=True).start()
meta = dict(host=command('uname', '-a'), cpu=command('lscpu'), docker=command('docker', 'version'),
            image=json.loads(command('docker', 'image', 'inspect', a.image)),
            started=time.strftime('%Y-%m-%dT%H:%M:%S%z'), controller_affinity=sorted(os.sched_getaffinity(0)),
            machine=a.machine, power_start=host_state(), cpus=6, cpuset=a.cpuset, aux_cpus=a.aux_cpus, workers=6, clients=6, gil_only=False, signal_timer='CTimer', warmup=1 if a.smoke else 15,
            duration=3 if a.smoke else 30, repeats=1 if a.smoke else 3, shuffle_seed=20261109)
(out / 'host-metadata.json').write_text(json.dumps(meta, indent=2))
rng = random.Random(20261109)
results = []
try:
    for repeat in range(1, meta['repeats']+1):
        cases = [(impl, loaded) for impl in ('baseline', 'py-spy', 'stack', 'signal')
                 for loaded in (False, True) if not a.smoke or loaded]
        rng.shuffle(cases)
        for impl, loaded in cases:
            current_case = f'{repeat}-{impl}-' + ('load' if loaded else 'idle')
            case_dir = out / current_case
            case_dir.mkdir()
            container = 'ctimer-fixed-' + a.machine + '-' + current_case
            print('START', current_case, flush=True)
            load = None
            before_uploads = len(uploads)
            before_power = len(power_history)
            with (case_dir / 'loadgen.log').open('w') as loadlog, (case_dir / 'container.log').open('w') as log:
                try:
                    if loaded:
                        load = subprocess.Popen([str(root/'loadgen'), '-nworkers=6'],
                                                stdout=loadlog, stderr=subprocess.STDOUT)
                    args = ['docker', 'run', '--name', container, '--init', '--network=host',
                            '--cpus=6', '--cpuset-cpus='+a.cpuset,
                            '-v', str(case_dir)+':/results', '-v', str(root)+':/harness:ro',
                            '-v', str(root/'wsgi.py')+':/app/hello_django/wsgi.py:ro',
                            a.image, '--implementation', impl,
                            '--loaded', str(int(loaded)), '--repeat', str(repeat),
                            '--warmup', str(meta['warmup']), '--duration', str(meta['duration'])]
                    (case_dir/'command.json').write_text(json.dumps(args))
                    status = subprocess.call(args, stdout=log, stderr=subprocess.STDOUT)
                    (case_dir/'docker-inspect.json').write_text(command('docker', 'inspect', container))
                    if status:
                        raise RuntimeError(f'Container {current_case} exited {status}: see {case_dir}')
                finally:
                    subprocess.run(['docker', 'stop', '-t', '35', container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    subprocess.run(['docker', 'rm', container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    if load:
                        load.terminate()
                        try:
                            load.wait(timeout=15)
                        except subprocess.TimeoutExpired:
                            load.kill(); load.wait()
            result = json.loads((case_dir/'result.json').read_text())
            result.update(case=current_case, profile_uploads=len(uploads)-before_uploads,
                          profile_bytes=sum(u['bytes'] for u in uploads[before_uploads:]),
                          loadgen_exit=load.returncode if load else None)
            result['host_samples'] = power_history[before_power:]
            if a.machine == 'laptop':
                assert result['host_samples'] and all(sample['state'].get('/sys/class/power_supply/AC/online') == '1' for sample in result['host_samples']), 'AC disconnected during run'
            assert not load or load.returncode == 0
            assert impl == 'baseline' or result['profile_uploads'] > 0, 'No profiles received'
            results.append(result)
            with (out/'runs.jsonl').open('a') as f:
                f.write(json.dumps(result)+'\n')
            (out/'uploads.json').write_text(json.dumps(uploads, indent=2))
            print(json.dumps(result), flush=True)
finally:
    power_done.set()
    (out/'host-timeline.json').write_text(json.dumps(power_history, indent=2))
    sink.shutdown()
    (out/'uploads.json').write_text(json.dumps(uploads, indent=2))
print('COMPLETE', len(results), flush=True)
