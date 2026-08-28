from __future__ import annotations

import gzip
import sys
import unittest
from pathlib import Path


SUITE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SUITE))

from bench.loadgen import percentile
from bench.collector import Counters, max_pprof_stack_depth
from bench.profilers import start_profiler
from bench.target import parse_workload
from bench.workloads import STACK_TRACE_DEPTH, run_with_stack_depth
from metrics import parse_key_values
from report import median_confidence_interval, pair_records, render_report
from run import cpu_count


def record(workload: str, profiler: str, repetition: int, factor: float) -> dict[str, object]:
    return {
        "workload": workload,
        "profiler": profiler,
        "repetition": repetition,
        "throughput": 100 / factor,
        "cpu_seconds": 2 * factor,
        "cpu_seconds_per_1000_ops": 0.2 * factor,
        "cpu_utilization_percent": 50 * factor,
        "memory_mean_bytes": 10 * 1024 * 1024 * factor,
        "memory_peak_bytes": 12 * 1024 * 1024 * factor,
        "latency_p95_ms": 5 * factor,
    }


class ParsingTests(unittest.TestCase):
    def test_controlled_workload_stack_is_at_least_128_frames(self) -> None:
        def observed_depth() -> int:
            frame = sys._getframe()
            count = 0
            while frame is not None:
                if frame.f_code is run_with_stack_depth.__code__:
                    count += 1
                frame = frame.f_back
            return count

        self.assertGreaterEqual(
            run_with_stack_depth(STACK_TRACE_DEPTH, observed_depth),
            128,
        )

    def test_collector_tracks_complete_profile_bodies(self) -> None:
        counters = Counters()
        counters.add_profile(128, incomplete=False, stack_depth=132)
        counters.add_profile(32, incomplete=True, stack_depth=None)
        self.assertEqual(
            counters.snapshot(),
            {
                "profile_requests": 2,
                "profile_bytes": 160,
                "incomplete_profiles": 1,
                "decoded_profiles": 1,
                "profile_decode_errors": 1,
                "deep_profile_requests": 1,
                "max_stack_depth": 132,
                "io_requests": 0,
            },
        )

    def test_collector_reads_stack_depth_from_uploaded_pprof(self) -> None:
        def varint(value: int) -> bytes:
            encoded = bytearray()
            while value >= 0x80:
                encoded.append((value & 0x7F) | 0x80)
                value >>= 7
            encoded.append(value)
            return bytes(encoded)

        def message(field: int, payload: bytes) -> bytes:
            return varint((field << 3) | 2) + varint(len(payload)) + payload

        packed_locations = b"".join(varint(index) for index in range(1, 129))
        pprof_sample = message(1, packed_locations)
        pprof_profile = message(2, pprof_sample)
        raw_sample = message(1, pprof_profile)
        series = message(2, raw_sample)
        push_request = message(1, series)
        self.assertEqual(max_pprof_stack_depth(gzip.compress(push_request)), 128)

    def test_parse_key_values(self) -> None:
        self.assertEqual(
            parse_key_values("usage_usec 123\nuser_usec 100\nsystem_usec 23\n"),
            {"usage_usec": 123, "user_usec": 100, "system_usec": 23},
        )

    def test_workload_and_cpuset_parsing(self) -> None:
        self.assertEqual(parse_workload("cpu-4"), ("cpu", 4))
        self.assertEqual(parse_workload("io-1"), ("io", 1))
        self.assertEqual(cpu_count("0-2,7"), 4)

    def test_percentile_interpolates(self) -> None:
        self.assertEqual(percentile([1.0, 2.0, 3.0], 0.5), 2.0)
        self.assertEqual(percentile([], 0.95), 0.0)


class ProfilerModeTests(unittest.TestCase):
    def test_none_mode_does_not_import_pyroscope(self) -> None:
        previous = sys.modules.pop("pyroscope", None)
        try:
            stop = start_profiler("none", "test", "http://unused")
            self.assertNotIn("pyroscope", sys.modules)
            stop()
        finally:
            if previous is not None:
                sys.modules["pyroscope"] = previous


class ReportTests(unittest.TestCase):
    def test_pairs_modes_by_repetition(self) -> None:
        records = [
            record("cpu-1", "current", 1, 1.1),
            record("cpu-1", "none", 0, 1.0),
            record("cpu-1", "current", 0, 1.2),
            record("cpu-1", "none", 1, 1.0),
        ]
        self.assertEqual(len(pair_records(records)["cpu-1"]), 2)

    def test_confidence_interval_contains_median(self) -> None:
        center, lower, upper = median_confidence_interval([1, 2, 3, 4, 5], resamples=500)
        self.assertEqual(center, 3)
        self.assertLessEqual(lower, center)
        self.assertGreaterEqual(upper, center)

    def test_markdown_report_contains_summary(self) -> None:
        records = [
            record("http-cpu", "none", 0, 1.0),
            record("http-cpu", "current", 0, 1.1),
        ]
        metadata = {
            "host": "linux",
            "python": "3.13",
            "image_id": "sha256:test",
            "target_cpus": "0-3",
            "load_cpus": "4-7",
            "collector_cpus": "8-9",
            "io_cpus": "10-11",
            "cpu_count": 4,
            "memory_limit": "2g",
            "stack_trace_depth": 128,
            "duration_seconds": 1,
            "repetitions": 1,
        }
        rendered = render_report(records, metadata)
        self.assertIn("## Paired overhead", rendered)
        self.assertIn("http-cpu", rendered)
        self.assertIn("p95 latency", rendered)


if __name__ == "__main__":
    unittest.main()

