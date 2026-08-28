"""Launching one measured run inside its own transient cgroup scope."""

import json
import os
import subprocess
import uuid
from pathlib import Path

import workloads
from protocol import RESULT_MARKER

HERE = Path(__file__).resolve().parent

# Disjoint CPU sets so measurement tooling cannot compete with the subject for
# the cores whose consumption is being measured.
SUBJECT_CPUS = os.environ.get("BENCH_SUBJECT_CPUS", "2-5")
LOADGEN_CPUS = os.environ.get("BENCH_LOADGEN_CPUS", "8-11")
SINK_CPUS = os.environ.get("BENCH_SINK_CPUS", "14-15")

PYTHON = os.environ.get(
    "BENCH_PYTHON",
    os.path.join(os.path.expanduser("~"), "bench-venv", "bin", "python"))


def cpuset_list(spec):
    """Expand a cpuset spec like "2-5,8" into [2, 3, 4, 5, 8].

    Needed because taskset wants an explicit list and a naive "-" to ","
    substitution silently drops the interior of every range.
    """
    cpus = []
    for part in spec.split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            a, b = part.split("-")
            cpus.extend(range(int(a), int(b) + 1))
        else:
            cpus.append(int(part))
    return cpus


def cpuset_size(spec):
    return len(cpuset_list(spec))


def scope_cmd(cpus, argv, name=None):
    """Wrap argv in a transient systemd scope pinned to `cpus`.

    The scope is the isolation boundary and the measurement instrument at once:
    cpuset pinning keeps other tenants out, and the scope's own cgroup node
    carries cpu.stat and memory.peak for exactly this run. A CPU *quota* would
    also limit the run but would add throttling jitter to the thing being
    measured, so pinning is used instead.
    """
    unit = name or f"bench-{uuid.uuid4().hex[:10]}"
    return [
        "sudo", "systemd-run", "--scope", "-q",
        f"--unit={unit}",
        "-p", f"AllowedCPUs={cpus}",
        "-p", "CPUAccounting=yes",
        "-p", "MemoryAccounting=yes",
        "--setenv=HOME=" + os.path.expanduser("~"),
        # The scope runs as root; letting it write .pyc files into the source
        # tree leaves root-owned artefacts that the next sync cannot replace.
        "--setenv=PYTHONDONTWRITEBYTECODE=1",
        "--",
        *argv,
    ]


def run_worker(*, shape, variant, iterations, inner, threads, batch,
               warmup_iterations, sink_url, sample_rate=None,
               upload_interval=None, timeout=900):
    """Execute one run and return its result dict, or raise with diagnostics.

    Each run is a fresh process in a fresh scope: global state, installed hooks
    and warmed caches from a previous variant would otherwise contaminate the
    next one.
    """
    run_id = f"{shape}-{variant}-{uuid.uuid4().hex[:8]}"

    argv = [
        PYTHON, str(HERE / "worker.py"),
        "--shape", shape,
        "--variant", variant,
        "--iterations", str(iterations),
        "--inner", str(inner),
        "--threads", str(threads),
        "--batch", str(batch),
        "--warmup-iterations", str(warmup_iterations),
        "--run-id", run_id,
        "--sink-url", sink_url,
    ]
    if sample_rate is not None:
        argv += ["--sample-rate", str(sample_rate)]
    if upload_interval is not None:
        argv += ["--upload-interval", str(upload_interval)]

    proc = subprocess.run(
        scope_cmd(SUBJECT_CPUS, argv),
        capture_output=True, text=True, timeout=timeout, cwd=str(HERE),
    )

    # Check the exit status of the thing that matters and confirm the artefact
    # exists. A wrapper reporting success while the real command failed is a
    # standing trap.
    if proc.returncode != 0:
        raise RuntimeError(
            f"run {run_id} exited {proc.returncode}\n"
            f"stdout: {proc.stdout[-2000:]}\nstderr: {proc.stderr[-2000:]}"
        )
    payload = None
    for line in proc.stdout.splitlines():
        if line.startswith(RESULT_MARKER):
            payload = line[len(RESULT_MARKER):]
    if payload is None:
        raise RuntimeError(
            f"run {run_id} emitted no result line\n"
            f"stdout: {proc.stdout[-2000:]}\nstderr: {proc.stderr[-2000:]}"
        )
    result = json.loads(payload)

    result["expected_iterations"] = workloads.expected_iterations(
        shape, iterations, threads)
    result["stderr_tail"] = proc.stderr[-500:] if proc.stderr else ""
    return result



