from __future__ import annotations

import random
import statistics
from collections import defaultdict
from pathlib import Path
from typing import Any, Callable


MIB = 1024 * 1024


def median_confidence_interval(
    values: list[float], *, resamples: int = 5_000, seed: int = 17
) -> tuple[float, float, float]:
    if not values:
        raise ValueError("at least one value is required")
    center = statistics.median(values)
    if len(values) == 1:
        return center, center, center
    generator = random.Random(seed)
    medians = sorted(
        statistics.median(generator.choices(values, k=len(values)))
        for _ in range(resamples)
    )
    return center, medians[int(0.025 * resamples)], medians[int(0.975 * resamples)]


def pair_records(records: list[dict[str, Any]]) -> dict[str, list[tuple[dict[str, Any], dict[str, Any]]]]:
    indexed: dict[tuple[str, int], dict[str, dict[str, Any]]] = defaultdict(dict)
    for record in records:
        indexed[(record["workload"], record["repetition"])][record["profiler"]] = record

    pairs: dict[str, list[tuple[dict[str, Any], dict[str, Any]]]] = defaultdict(list)
    for (workload, _repetition), modes in indexed.items():
        if set(modes) != {"none", "current"}:
            raise ValueError(f"incomplete profiler pair for {workload}")
        pairs[workload].append((modes["none"], modes["current"]))
    return pairs


def paired_percent(
    pairs: list[tuple[dict[str, Any], dict[str, Any]]],
    metric: str,
    transform: Callable[[float], float] = lambda ratio: (ratio - 1) * 100,
) -> tuple[float, float, float]:
    values = [
        transform(float(current[metric]) / float(baseline[metric]))
        for baseline, current in pairs
    ]
    return median_confidence_interval(values)


def paired_delta(
    pairs: list[tuple[dict[str, Any], dict[str, Any]]], metric: str, scale: float = 1
) -> tuple[float, float, float]:
    values = [
        (float(current[metric]) - float(baseline[metric])) / scale
        for baseline, current in pairs
    ]
    return median_confidence_interval(values)


def _format_ci(value: tuple[float, float, float], suffix: str = "") -> str:
    center, lower, upper = value
    return f"{center:+.2f}{suffix} [{lower:+.2f}, {upper:+.2f}]"


def _median(records: list[dict[str, Any]], metric: str) -> float:
    return statistics.median(float(record[metric]) for record in records)


def render_report(records: list[dict[str, Any]], metadata: dict[str, Any]) -> str:
    pairs_by_workload = pair_records(records)
    lines = [
        "# Pyroscope CPU profiler overhead",
        "",
        "Positive overhead means the current profiler consumed more resources or reduced throughput.",
        "Intervals are 95% bootstrap confidence intervals over paired trial medians.",
        "",
        "## Environment",
        "",
        f"- Host: `{metadata['host']}`",
        f"- Python: `{metadata['python']}`",
        f"- Image: `{metadata['image_id']}`",
        f"- CPU sets: target `{metadata['target_cpus']}`, load generator `{metadata['load_cpus']}`, "
        f"profile collector `{metadata['collector_cpus']}`, I/O sink `{metadata['io_cpus']}`",
        f"- Limit: {metadata['cpu_count']} CPUs, {metadata['memory_limit']}",
        f"- Controlled Python stack depth: {metadata['stack_trace_depth']} frames",
        f"- Uploaded pprof validation: at least one {metadata['stack_trace_depth']}-frame "
        "sample per profiled trial",
        f"- Trial duration: {metadata['duration_seconds']}s; repetitions: {metadata['repetitions']}",
        "",
        "## Paired overhead",
        "",
        "| Workload | Throughput drop | CPU seconds | CPU / 1k ops | Mean memory | Peak memory | p95 latency |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]

    for workload in sorted(pairs_by_workload):
        pairs = pairs_by_workload[workload]
        throughput = paired_percent(pairs, "throughput", lambda ratio: (1 - ratio) * 100)
        cpu = paired_percent(pairs, "cpu_seconds")
        cpu_per_ops = paired_percent(pairs, "cpu_seconds_per_1000_ops")
        memory_mean = paired_delta(pairs, "memory_mean_bytes", MIB)
        memory_peak = paired_delta(pairs, "memory_peak_bytes", MIB)
        if workload.startswith("http-"):
            latency = _format_ci(paired_percent(pairs, "latency_p95_ms"), "%")
        else:
            latency = "n/a"
        lines.append(
            "| "
            + " | ".join(
                [
                    workload,
                    _format_ci(throughput, "%"),
                    _format_ci(cpu, "%"),
                    _format_ci(cpu_per_ops, "%"),
                    _format_ci(memory_mean, " MiB"),
                    _format_ci(memory_peak, " MiB"),
                    latency,
                ]
            )
            + " |"
        )

    lines.extend(
        [
            "",
            "## Median measurements",
            "",
            "| Workload | Mode | Throughput (ops/s) | CPU utilization | CPU / 1k ops | Mean memory (MiB) | Peak memory (MiB) |",
            "|---|---|---:|---:|---:|---:|---:|",
        ]
    )
    grouped: dict[tuple[str, str], list[dict[str, Any]]] = defaultdict(list)
    for record in records:
        grouped[(record["workload"], record["profiler"])].append(record)
    for (workload, mode), mode_records in sorted(grouped.items()):
        lines.append(
            f"| {workload} | {mode} | "
            f"{_median(mode_records, 'throughput'):.2f} | "
            f"{_median(mode_records, 'cpu_utilization_percent'):.2f}% | "
            f"{_median(mode_records, 'cpu_seconds_per_1000_ops'):.6f}s | "
            f"{_median(mode_records, 'memory_mean_bytes') / MIB:.2f} | "
            f"{_median(mode_records, 'memory_peak_bytes') / MIB:.2f} |"
        )

    lines.extend(
        [
            "",
            "Raw per-trial measurements are in `raw.jsonl`; run configuration is in `metadata.json`.",
            "",
        ]
    )
    return "\n".join(lines)


def write_report(
    records: list[dict[str, Any]], metadata: dict[str, Any], destination: Path
) -> None:
    destination.write_text(render_report(records, metadata))

