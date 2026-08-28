#!/usr/bin/env python3
"""Re-run aggregation, guards and reporting over a stored results.json.

The point of writing every individual run to disk is that the analysis can be
corrected without paying to measure again. When a guard turns out to encode the
wrong model -- which happened -- the fix should be verifiable against the data
that exposed it, not against a fresh run whose conditions differ.

    python reanalyze.py results/<timestamp>/results.json [--out-dir DIR]
"""

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import report as report_mod
import run_bench


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("results_json")
    ap.add_argument("--out-dir", default=None,
                    help="defaults to overwriting the reports beside the input")
    args = ap.parse_args()

    src = Path(args.results_json)
    payload = json.loads(src.read_text())
    meta, runs = payload["meta"], payload["runs"]

    agg = run_bench.aggregate(runs, meta["variants"])
    rep = run_bench.check_all(agg, runs, meta["variants"],
                              meta["upload_interval"])
    if meta.get("mode") == "sweep":
        run_bench.check_sweep(agg, rep, meta["variants"])

    out = Path(args.out_dir) if args.out_dir else src.parent
    out.mkdir(parents=True, exist_ok=True)
    (out / "report.md").write_text(report_mod.markdown(meta, agg, rep))
    (out / "report.html").write_text(report_mod.html(meta, agg, rep))
    (out / "results.json").write_text(json.dumps(
        {"meta": meta, "aggregate": agg, "runs": runs,
         "checks": [c.__dict__ for c in rep.checks]}, indent=2, default=str))

    print(report_mod.markdown(meta, agg, rep))
    print(f"\nrewritten in {out}")
    return 0 if rep.green else 1


if __name__ == "__main__":
    sys.exit(main())
