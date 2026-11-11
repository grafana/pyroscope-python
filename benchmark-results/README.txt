Profiler benchmarks and investigations

Latest report:
  ctimer-fixed-both/vm/benchmark-report.md

Both-machine results:
  ctimer-fixed-both/comparison.txt
  ctimer-fixed-both/comparison.csv

Per-machine raw results and measurements:
  ctimer-fixed-both/vm/
  ctimer-fixed-both/laptop/

Reproduction package:
  ctimer-fixed-both/vm/profiler-benchmark-report.zip

Earlier measurements and diagnostic evidence:
  python-bench/
  remote-docker/
  remote-docker/investigations/

The measured SDK revision, workload, package versions, configuration and hashes
are recorded with the results. Earlier and interrupted runs are retained in
separate directories and are excluded from the latest report.

Python bytecode caches, private coordination prompts and the generated SDK
source export are kept out of Git. Recreate that export with:
  python3 benchmark-results/ctimer-fixed-both/export_source_context.py
after initializing the repository submodules. The export uses pinned revisions
from source.json, not the current working-tree contents.
