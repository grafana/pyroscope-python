"""Compare uploaded CPU weight with observed worker lifecycle CPU bounds."""
from collections import defaultdict
import csv
import gzip
import json
from pathlib import Path
import statistics
import sys

root = Path(sys.argv[1])
runs = [json.loads(s) for s in (root/'runs.jsonl').read_text().splitlines()]
profiles = json.loads((root/'profile-summary.json').read_text())
with gzip.open(root/'complete-stacks.json.gz', 'rt') as f:
    stacks = json.load(f)
drop_names = {f'[dropped: {reason}]' for reason in (
    'contentions', 'buffer_dropped', 'no_thread_state', 'invalid_code_type',
    'read_errors', 'truncated', 'invalid', 'resolve_error',
)}
rows = []
for run in runs:
    if run['implementation'] == 'baseline':
        continue
    case = run['case']
    total = profiles[case]['totals'][0]/1e9
    drops = {}
    for stack in stacks[case]['stacks']:
        frames = stack['frames_root_to_leaf']
        if len(frames) == 1 and frames[0]['name'] in drop_names:
            drops[frames[0]['name']] = stack['values'][0]/1e9
    lower, upper = run['profile_cpu_lower_bound_s'], run['profile_cpu_upper_bound_s']
    rows.append(dict(case=case, implementation=run['implementation'], loaded=run['loaded'],
                     repeat=run['repeat'], uploaded_cpu_s=total,
                     worker_lifecycle_cpu_lower_s=lower, worker_lifecycle_cpu_upper_s=upper,
                     uploaded_over_upper_percent=100*total/upper,
                     uploaded_over_lower_percent=100*total/lower,
                     recorded_drop_weight_s=sum(drops.values()), drop_reasons=drops))
(root/'coverage.json').write_text(json.dumps(rows, indent=2)+'\n')
with (root/'coverage.csv').open('w') as f:
    writer=csv.DictWriter(f, fieldnames=rows[0].keys()); writer.writeheader(); writer.writerows(rows)
groups=defaultdict(list)
for row in rows:
    groups[row['implementation'],row['loaded']].append(row)
lines=[
    'Uploaded CPU weight versus observed worker lifecycle CPU',
    'Lower bound: sum(cpu before shutdown - cpu after configure).',
    'Upper bound: sum(cpu after shutdown - cpu before configure).',
    'These bracket the active profiler lifetime and include all worker threads.',
    'They exclude the Gunicorn master and replace the earlier extrapolated estimates.',
    'Sampling filters or unregistered threads can still make profiler targets differ.',
    '',
    'Implementation Workload Uploaded CPU s  Lifecycle CPU lower-upper s  Ratio to upper-lower',
]
for (impl,loaded),group in sorted(groups.items()):
    total=statistics.mean(r['uploaded_cpu_s'] for r in group)
    lower=statistics.mean(r['worker_lifecycle_cpu_lower_s'] for r in group)
    upper=statistics.mean(r['worker_lifecycle_cpu_upper_s'] for r in group)
    lines.append(f"{impl:14} {'loaded' if loaded else 'idle':8} {total:14.4f}  {lower:10.4f}-{upper:10.4f}  {100*total/upper:8.2f}%-{100*total/lower:8.2f}%")
(root/'coverage.txt').write_text('\n'.join(lines)+'\n')
print('\n'.join(lines))
