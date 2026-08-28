from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import subprocess
import tempfile
import time
import uuid
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from metrics import CgroupMonitor, locate_cgroup
from report import write_report


ALL_WORKLOADS = ("cpu-1", "cpu-4", "io-1", "io-4", "http-cpu", "http-io")


def command(
    arguments: list[str], *, timeout: float | None = None, check: bool = True
) -> str:
    result = subprocess.run(
        arguments,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )
    if check and result.returncode:
        rendered = " ".join(arguments)
        raise RuntimeError(
            f"command failed ({result.returncode}): {rendered}\n{result.stderr.strip()}"
        )
    return result.stdout.strip()


def docker(*arguments: str, timeout: float | None = None, check: bool = True) -> str:
    return command(["docker", *arguments], timeout=timeout, check=check)


def cpu_count(cpuset: str) -> int:
    cpus: set[int] = set()
    for item in cpuset.split(","):
        if "-" in item:
            first, last = (int(value) for value in item.split("-", 1))
            cpus.update(range(first, last + 1))
        else:
            cpus.add(int(item))
    if not cpus:
        raise ValueError("CPU set must not be empty")
    return len(cpus)


def wait_for_file(path: Path, container: str, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while not path.exists():
        if time.monotonic() >= deadline:
            raise TimeoutError(f"timed out waiting for {path}")
        running = docker("inspect", "-f", "{{.State.Running}}", container, check=False)
        if running == "false":
            logs = docker("logs", container, check=False)
            raise RuntimeError(f"{container} exited before {path.name} appeared\n{logs}")
        time.sleep(0.02)


def collector_stats(container: str) -> dict[str, int]:
    script = (
        "import json,urllib.request;"
        "print(urllib.request.urlopen('http://127.0.0.1:4040/stats').read().decode())"
    )
    return json.loads(docker("exec", container, "python", "-c", script))


def wait_for_collector(container: str) -> None:
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        try:
            collector_stats(container)
            return
        except (RuntimeError, json.JSONDecodeError):
            time.sleep(0.1)
    raise RuntimeError("collector did not become ready")


def wait_for_http(container: str) -> None:
    script = (
        "import urllib.request;"
        "urllib.request.urlopen('http://127.0.0.1:8000/health',timeout=1).read();"
        "print('ready')"
    )
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if docker("exec", container, "python", "-c", script, check=False) == "ready":
            return
        running = docker("inspect", "-f", "{{.State.Running}}", container, check=False)
        if running == "false":
            break
        time.sleep(0.1)
    logs = docker("logs", container, check=False)
    raise RuntimeError(f"Gunicorn did not become ready\n{logs}")


def cgroup_for_container(container: str) -> Path:
    pid = int(docker("inspect", "-f", "{{.State.Pid}}", container))
    return locate_cgroup(pid)


def remove_container(container: str) -> None:
    docker("rm", "--force", container, check=False)


def finish_record(
    *,
    workload: str,
    profiler: str,
    repetition: int,
    result: dict[str, Any],
    measured: dict[str, Any],
    uploads: int,
    profile_bytes: int,
    incomplete_profiles: int,
    decoded_profiles: int,
    profile_decode_errors: int,
    deep_profile_requests: int,
) -> dict[str, Any]:
    operations = int(result["operations"])
    if operations <= 0:
        raise RuntimeError(f"{workload}/{profiler} completed no operations")
    if int(result["errors"]) != 0:
        raise RuntimeError(f"{workload}/{profiler} had {result['errors']} errors")
    record = {
        "workload": workload,
        "profiler": profiler,
        "repetition": repetition,
        "profile_uploads": uploads,
        "profile_bytes": profile_bytes,
        "incomplete_profiles": incomplete_profiles,
        "decoded_profiles": decoded_profiles,
        "profile_decode_errors": profile_decode_errors,
        "deep_profile_requests": deep_profile_requests,
        **result,
        **measured,
    }
    record["cpu_seconds_per_1000_ops"] = (
        float(record["cpu_seconds"]) * 1_000 / operations
    )
    return record


def run_standalone(
    *,
    image: str,
    network: str,
    collector: str,
    workload: str,
    profiler: str,
    repetition: int,
    duration: float,
    warmup: float,
    cpus: str,
    memory: str,
    control: Path,
) -> dict[str, Any]:
    container = f"pyroscope-overhead-{uuid.uuid4().hex[:10]}"
    before = collector_stats(collector)
    control.mkdir()
    try:
        docker(
            "run",
            "--detach",
            "--name",
            container,
            "--network",
            network,
            "--cpuset-cpus",
            cpus,
            "--memory",
            memory,
            "--pids-limit",
            "512",
            "--mount",
            f"type=bind,src={control},dst=/control",
            image,
            "python",
            "-m",
            "bench.target",
            "--workload",
            workload,
            "--profiler",
            profiler,
            "--duration",
            str(duration),
            "--warmup",
            str(warmup),
            "--io-server",
            "http://io-sink:4040",
        )
        wait_for_file(control / "ready.json", container, timeout=warmup + 45)
        monitor = CgroupMonitor(cgroup_for_container(container))
        monitor.start()
        (control / "start").touch()
        wait_for_file(control / "result.json", container, timeout=duration + 30)
        measured = monitor.finish(cpu_count(cpus))
        result = json.loads((control / "result.json").read_text())
        (control / "release").touch()
        exit_code = int(docker("wait", container, timeout=30))
        if exit_code:
            logs = docker("logs", container, check=False)
            raise RuntimeError(f"{container} exited with {exit_code}\n{logs}")
        after = collector_stats(collector)
        return finish_record(
            workload=workload,
            profiler=profiler,
            repetition=repetition,
            result=result,
            measured=measured,
            uploads=after["profile_requests"] - before["profile_requests"],
            profile_bytes=after["profile_bytes"] - before["profile_bytes"],
            incomplete_profiles=(
                after["incomplete_profiles"] - before["incomplete_profiles"]
            ),
            decoded_profiles=after["decoded_profiles"] - before["decoded_profiles"],
            profile_decode_errors=(
                after["profile_decode_errors"] - before["profile_decode_errors"]
            ),
            deep_profile_requests=(
                after["deep_profile_requests"] - before["deep_profile_requests"]
            ),
        )
    finally:
        remove_container(container)


def run_loadgen(
    *,
    image: str,
    network: str,
    target: str,
    endpoint: str,
    duration: float,
    cpus: str,
    output_dir: Path,
    output_name: str,
    concurrency: int,
) -> dict[str, Any]:
    output = output_dir / output_name
    docker(
        "run",
        "--rm",
        "--network",
        network,
        "--cpuset-cpus",
        cpus,
        "--memory",
        "1g",
        "--mount",
        f"type=bind,src={output_dir},dst=/output",
        image,
        "python",
        "-m",
        "bench.loadgen",
        "--url",
        f"http://{target}:8000/{endpoint}",
        "--duration",
        str(duration),
        "--concurrency",
        str(concurrency),
        "--output",
        f"/output/{output_name}",
        timeout=duration + 30,
    )
    return json.loads(output.read_text())


def start_loadgen(
    *,
    image: str,
    network: str,
    target: str,
    endpoint: str,
    duration: float,
    cpus: str,
    output_dir: Path,
    output_name: str,
    concurrency: int,
) -> str:
    container = f"pyroscope-overhead-client-{uuid.uuid4().hex[:10]}"
    ready = output_dir / "loadgen-ready"
    start = output_dir / "loadgen-start"
    docker(
        "run",
        "--detach",
        "--name",
        container,
        "--network",
        network,
        "--cpuset-cpus",
        cpus,
        "--memory",
        "1g",
        "--mount",
        f"type=bind,src={output_dir},dst=/output",
        image,
        "python",
        "-m",
        "bench.loadgen",
        "--url",
        f"http://{target}:8000/{endpoint}",
        "--duration",
        str(duration),
        "--concurrency",
        str(concurrency),
        "--output",
        f"/output/{output_name}",
        "--ready-file",
        "/output/loadgen-ready",
        "--start-file",
        "/output/loadgen-start",
    )
    wait_for_file(ready, container, timeout=30)
    if start.exists():
        raise RuntimeError(f"stale load-generator start marker: {start}")
    return container


def run_http(
    *,
    image: str,
    network: str,
    collector: str,
    workload: str,
    profiler: str,
    repetition: int,
    duration: float,
    warmup: float,
    target_cpus: str,
    load_cpus: str,
    memory: str,
    control: Path,
    concurrency: int,
) -> dict[str, Any]:
    container = f"pyroscope-overhead-http-{uuid.uuid4().hex[:10]}"
    endpoint = workload.removeprefix("http-")
    before = collector_stats(collector)
    control.mkdir()
    try:
        docker(
            "run",
            "--detach",
            "--name",
            container,
            "--network",
            network,
            "--cpuset-cpus",
            target_cpus,
            "--memory",
            memory,
            "--pids-limit",
            "512",
            "--env",
            f"BENCH_PROFILER={profiler}",
            "--env",
            "BENCH_COLLECTOR=http://collector:4040",
            image,
            "gunicorn",
            "--bind",
            "0.0.0.0:8000",
            "--workers",
            "4",
            "--worker-class",
            "sync",
            "--timeout",
            "30",
            "--graceful-timeout",
            "10",
            "--keep-alive",
            "5",
            "--access-logfile",
            "/dev/null",
            "--error-logfile",
            "-",
            "--log-level",
            "warning",
            "bench.http_app:application",
        )
        wait_for_http(container)
        if warmup > 0:
            warmup_result = run_loadgen(
                image=image,
                network=network,
                target=container,
                endpoint=endpoint,
                duration=warmup,
                cpus=load_cpus,
                output_dir=control,
                output_name="warmup.json",
                concurrency=concurrency,
            )
            if warmup_result["errors"]:
                raise RuntimeError(f"HTTP warmup had {warmup_result['errors']} errors")

        client = start_loadgen(
            image=image,
            network=network,
            target=container,
            endpoint=endpoint,
            duration=duration,
            cpus=load_cpus,
            output_dir=control,
            output_name="result.json",
            concurrency=concurrency,
        )
        try:
            monitor = CgroupMonitor(cgroup_for_container(container))
            monitor.start()
            (control / "loadgen-start").touch()
            wait_for_file(control / "result.json", client, timeout=duration + 30)
            measured = monitor.finish(cpu_count(target_cpus))
            exit_code = int(docker("wait", client, timeout=15))
            if exit_code:
                logs = docker("logs", client, check=False)
                raise RuntimeError(f"{client} exited with {exit_code}\n{logs}")
            result = json.loads((control / "result.json").read_text())
        finally:
            remove_container(client)
        docker("stop", "--time", "15", container, timeout=25)
        after = collector_stats(collector)
        return finish_record(
            workload=workload,
            profiler=profiler,
            repetition=repetition,
            result=result,
            measured=measured,
            uploads=after["profile_requests"] - before["profile_requests"],
            profile_bytes=after["profile_bytes"] - before["profile_bytes"],
            incomplete_profiles=(
                after["incomplete_profiles"] - before["incomplete_profiles"]
            ),
            decoded_profiles=after["decoded_profiles"] - before["decoded_profiles"],
            profile_decode_errors=(
                after["profile_decode_errors"] - before["profile_decode_errors"]
            ),
            deep_profile_requests=(
                after["deep_profile_requests"] - before["deep_profile_requests"]
            ),
        )
    finally:
        remove_container(container)


def image_metadata(image: str) -> dict[str, Any]:
    script = (
        "import importlib.metadata,json,platform;"
        "from bench.workloads import STACK_TRACE_DEPTH;"
        "print(json.dumps({'python':platform.python_version(),"
        "'pyroscope':importlib.metadata.version('pyroscope-io'),"
        "'gunicorn':importlib.metadata.version('gunicorn'),"
        "'stack_trace_depth':STACK_TRACE_DEPTH}))"
    )
    return json.loads(docker("run", "--rm", image, "python", "-c", script))


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", default="pyroscope-profiler-overhead:local")
    parser.add_argument("--duration", type=float, default=20.0)
    parser.add_argument("--warmup", type=float, default=2.0)
    parser.add_argument("--repetitions", type=int, default=7)
    parser.add_argument("--target-cpus", default="0-3")
    parser.add_argument("--load-cpus", default="4-7")
    parser.add_argument("--collector-cpus", default="8-9")
    parser.add_argument("--io-cpus", default="10-11")
    parser.add_argument("--memory", default="2g")
    parser.add_argument("--concurrency", type=int, default=32)
    parser.add_argument("--workloads", default=",".join(ALL_WORKLOADS))
    parser.add_argument("--output", type=Path)
    parser.add_argument("--smoke", action="store_true")
    return parser.parse_args()


def main() -> None:
    args = parse_arguments()
    if shutil.which("docker") is None:
        raise RuntimeError("docker is required")

    workloads = tuple(filter(None, args.workloads.split(",")))
    unknown = set(workloads) - set(ALL_WORKLOADS)
    if unknown:
        raise ValueError(f"unknown workloads: {', '.join(sorted(unknown))}")
    if args.smoke:
        args.duration = 0.75
        args.warmup = 0.2
        args.repetitions = 1

    cpu_sets = {
        "target": set(_expand_cpuset(args.target_cpus)),
        "load generator": set(_expand_cpuset(args.load_cpus)),
        "profile collector": set(_expand_cpuset(args.collector_cpus)),
        "I/O sink": set(_expand_cpuset(args.io_cpus)),
    }
    names = list(cpu_sets)
    for index, name in enumerate(names):
        for other in names[index + 1 :]:
            if cpu_sets[name] & cpu_sets[other]:
                raise ValueError(f"{name} and {other} CPU sets must not overlap")
    if hasattr(os, "sched_getaffinity"):
        available = os.sched_getaffinity(0)
        requested = set().union(*cpu_sets.values())
        if not requested <= available:
            missing = ", ".join(str(cpu) for cpu in sorted(requested - available))
            raise ValueError(f"requested CPUs are unavailable on this host: {missing}")
    if not Path("/sys/fs/cgroup/cgroup.controllers").exists():
        raise RuntimeError("the benchmark requires Linux cgroup v2")

    timestamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    output = args.output or Path(__file__).parent / "results" / timestamp
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    network = f"pyroscope-overhead-{uuid.uuid4().hex[:10]}"
    collector = f"{network}-collector"
    io_sink = f"{network}-io"
    records: list[dict[str, Any]] = []

    package = image_metadata(args.image)
    metadata: dict[str, Any] = {
        "created_at": datetime.now(UTC).isoformat(),
        "host": f"{platform.node()} ({platform.platform()})",
        "python": package["python"],
        "pyroscope": package["pyroscope"],
        "gunicorn": package["gunicorn"],
        "stack_trace_depth": int(package["stack_trace_depth"]),
        "docker": docker("version", "--format", "{{.Server.Version}}"),
        "image": args.image,
        "image_id": docker("image", "inspect", "-f", "{{.Id}}", args.image),
        "target_cpus": args.target_cpus,
        "load_cpus": args.load_cpus,
        "collector_cpus": args.collector_cpus,
        "io_cpus": args.io_cpus,
        "cpu_count": cpu_count(args.target_cpus),
        "memory_limit": args.memory,
        "duration_seconds": args.duration,
        "warmup_seconds": args.warmup,
        "repetitions": args.repetitions,
        "concurrency": args.concurrency,
        "workloads": workloads,
    }
    (output / "metadata.json").write_text(json.dumps(metadata, indent=2, sort_keys=True))

    docker("network", "create", network)
    try:
        docker(
            "run",
            "--detach",
            "--name",
            collector,
            "--network",
            network,
            "--network-alias",
            "collector",
            "--cpuset-cpus",
            args.collector_cpus,
            "--memory",
            "1g",
            args.image,
            "python",
            "-m",
            "bench.collector",
        )
        wait_for_collector(collector)
        docker(
            "run",
            "--detach",
            "--name",
            io_sink,
            "--network",
            network,
            "--network-alias",
            "io-sink",
            "--cpuset-cpus",
            args.io_cpus,
            "--memory",
            "1g",
            args.image,
            "python",
            "-m",
            "bench.collector",
        )
        wait_for_collector(io_sink)
        with tempfile.TemporaryDirectory(prefix="control-", dir=output) as temporary:
            controls = Path(temporary)
            raw = output / "raw.jsonl"
            for repetition in range(args.repetitions):
                for workload_index, workload in enumerate(workloads):
                    modes = ("none", "current")
                    if (repetition + workload_index) % 2:
                        modes = tuple(reversed(modes))
                    for profiler in modes:
                        print(
                            f"[{len(records) + 1}/{args.repetitions * len(workloads) * 2}] "
                            f"{workload} {profiler} repetition {repetition + 1}",
                            flush=True,
                        )
                        control = controls / f"{repetition}-{workload}-{profiler}"
                        if workload.startswith("http-"):
                            record = run_http(
                                image=args.image,
                                network=network,
                                collector=collector,
                                workload=workload,
                                profiler=profiler,
                                repetition=repetition,
                                duration=args.duration,
                                warmup=args.warmup,
                                target_cpus=args.target_cpus,
                                load_cpus=args.load_cpus,
                                memory=args.memory,
                                control=control,
                                concurrency=args.concurrency,
                            )
                        else:
                            record = run_standalone(
                                image=args.image,
                                network=network,
                                collector=collector,
                                workload=workload,
                                profiler=profiler,
                                repetition=repetition,
                                duration=args.duration,
                                warmup=args.warmup,
                                cpus=args.target_cpus,
                                memory=args.memory,
                                control=control,
                            )
                        expected_uploads = profiler == "current"
                        has_profile = (
                            record["profile_uploads"] > 0 and record["profile_bytes"] > 0
                        )
                        has_partial_profile = (record["profile_uploads"] > 0) != (
                            record["profile_bytes"] > 0
                        )
                        if (
                            has_partial_profile
                            or record["incomplete_profiles"] != 0
                            or record["profile_decode_errors"] != 0
                            or record["decoded_profiles"] != record["profile_uploads"]
                            or expected_uploads
                            != (record["deep_profile_requests"] > 0)
                            or expected_uploads != has_profile
                        ):
                            raise RuntimeError(
                                f"unexpected profile data for {workload}/{profiler}: "
                                f"{record['profile_uploads']} requests, "
                                f"{record['profile_bytes']} bytes, "
                                f"{record['incomplete_profiles']} incomplete, "
                                f"{record['profile_decode_errors']} decode errors, "
                                f"{record['deep_profile_requests']} with at least "
                                f"{metadata['stack_trace_depth']} frames"
                            )
                        records.append(record)
                        with raw.open("a") as output_file:
                            output_file.write(json.dumps(record, sort_keys=True) + "\n")
    finally:
        remove_container(collector)
        remove_container(io_sink)
        docker("network", "rm", network, check=False)

    write_report(records, metadata, output / "report.md")
    print(f"report: {output / 'report.md'}")


def _expand_cpuset(value: str) -> list[int]:
    cpus: list[int] = []
    for item in value.split(","):
        if "-" in item:
            first, last = (int(part) for part in item.split("-", 1))
            cpus.extend(range(first, last + 1))
        else:
            cpus.append(int(item))
    return cpus


if __name__ == "__main__":
    main()

