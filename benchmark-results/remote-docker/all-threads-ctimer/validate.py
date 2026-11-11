import hashlib
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
runs = [json.loads(s) for s in (root/'runs.jsonl').read_text().splitlines()]
assert len(runs) == 24
expected_hash = hashlib.sha256((Path(__file__).parent/'wsgi.py').read_bytes()).hexdigest()
for r in runs:
    p = root/r['case']
    m = json.loads((p/'metadata.json').read_text())
    inspect = json.loads((p/'docker-inspect.json').read_text())[0]
    assert m['workers'] == 6 and m['affinity'] == list(range(6)) and m['cpu_max'] == '600000 100000'
    assert m['gil_only'] is False and r['gil_only'] is False
    assert m['signal_timer'] == r['signal_timer'] == 'CTimer'
    assert m['wsgi_sha256'] == expected_hash
    assert inspect['HostConfig']['NanoCpus'] == 6000000000 and inspect['HostConfig']['CpusetCpus'] == '0-5'
    assert len(r['worker_pids']) == 6 and r['elapsed_s'] >= 30 and r['rss_samples'] >= 290
    assert r['errors'] == r['panics'] == r['server_exit'] == 0
    assert not inspect['State']['OOMKilled'] and inspect['State']['ExitCode'] == 0
    assert r['rss_start_mib'] <= r['rss_peak_mib'] and r['rss_end_mib'] <= r['rss_peak_mib']
    if r['loaded']:
        assert r['loadgen_exit'] == 0
    if r['implementation'] != 'baseline':
        assert len(r['lifecycle_cpu']) == 6
        assert {entry['pid'] for entry in r['lifecycle_cpu']} == set(r['worker_pids'])
        for entry in r['lifecycle_cpu']:
            assert entry['shutdown_succeeded']
            assert entry['cpu_before_configure'] <= entry['cpu_after_configure'] <= entry['cpu_before_shutdown'] <= entry['cpu_after_shutdown']
profiles = json.loads((root/'profile-summary.json').read_text())
assert len(profiles) == 18 and all(sum(p['totals']) > 0 for p in profiles.values())
result = dict(completed_runs=24, cpus=6, workers=6, gil_only=False, signal_timer='CTimer',
              wsgi_sha256=expected_hash, nonempty_profile_sets=18,
              successful_measured_requests=sum(r['requests'] for r in runs),
              http_errors=0, detected_rust_panics=0, worker_replacements=0,
              maximum_cgroup_throttled_seconds=max(r['cgroup_cpu_delta']['throttled_usec']/1e6 for r in runs))
(root/'validation.json').write_text(json.dumps(result, indent=2)+'\n')
print(json.dumps(result, indent=2))
