import json
from pathlib import Path

p = Path('measured/runs.jsonl')
runs = [json.loads(s) for s in p.read_text().splitlines()] if p.exists() else []
print(f'Completed {len(runs)}/24')
for r in runs[-2:]:
    print(r['case'], f"{r['requests_per_s']:.1f} req/s; {r['cpu_s']:.2f} CPU s; "
          f"RSS mean/peak {r['rss_mean_mib']:.2f}/{r['rss_peak_mib']:.2f} MiB; errors {r['errors']}")
lines = Path('run.log').read_text().splitlines()
if lines:
    print(lines[-1][:160])
