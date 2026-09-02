#!/usr/bin/env python3
"""Runs many profiled workers in parallel, in rounds, until one dies of a
native fault (SIGSEGV / SIGABRT from a bad free) or the time budget is spent.

Usage: runner.py [--mode threads|gunicorn] [--procs N] [--duration S] [--budget S]
"""
import argparse
import os
import signal
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
PY = HERE / ".venv" / "bin" / "python"
if not PY.exists():  # inside the container there is no venv
    PY = Path(sys.executable)
LOGS = HERE / "logs"
CORES = Path(os.environ.get("CORES_DIR", HERE / "cores"))


def start_sink(port):
    script = "tls_sink.py" if os.environ.get("TLS") == "1" else "sink.py"
    return subprocess.Popen([str(PY), str(HERE / script), str(port)],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def spawn(mode, env, log, cwd=None):
    if mode == "threads":
        cmd = [str(PY), str(HERE / "workload.py")]
    elif mode == "fork":
        cmd = [str(PY), str(HERE / "forkload.py")]
    elif mode == "asan":
        cmd = [str(HERE / "run-asan.sh")]
    else:
        cmd = [str(PY), "-m", "gunicorn",
               "-c", str(HERE / "gunicorn_conf.py"), "wsgi:app"]
    return subprocess.Popen(cmd, cwd=str(cwd or HERE), env=env,
                            stdout=log, stderr=subprocess.STDOUT)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", default="threads", choices=["threads", "gunicorn", "fork", "asan"])
    ap.add_argument("--procs", type=int, default=12)
    ap.add_argument("--duration", type=float, default=60)
    ap.add_argument("--budget", type=float, default=900)
    ap.add_argument("--port", type=int, default=4040)
    args = ap.parse_args()

    if LOGS.exists():
        shutil.rmtree(LOGS)
    LOGS.mkdir(parents=True)
    CORES.mkdir(parents=True, exist_ok=True)

    sink = start_sink(args.port)
    env = dict(os.environ)
    env["DURATION"] = str(args.duration)
    scheme = "https" if os.environ.get("TLS") == "1" else "http"
    host = "localhost" if os.environ.get("TLS") == "1" else "127.0.0.1"
    env.setdefault("PYROSCOPE_SERVER", f"{scheme}://{host}:{args.port}")
    env["PYTHONUNBUFFERED"] = "1"
    env["PYTHONPATH"] = str(HERE) + ":" + env.get("PYTHONPATH", "")

    started = time.time()
    deadline = started + args.budget
    rnd = 0
    total = 0
    crashes = []
    try:
        while time.time() < deadline and not crashes:
            rnd += 1
            procs = []
            for i in range(args.procs):
                path = LOGS / f"r{rnd}-w{i}.log"
                log = open(path, "wb")
                # Each worker gets its own cwd so that a core dump (the usual
                # core_pattern is the relative name "core") is not overwritten
                # by another worker.
                cwd = CORES / f"r{rnd}-w{i}"
                cwd.mkdir(parents=True, exist_ok=True)
                procs.append((spawn(args.mode, env, log, cwd), log, path))
            for p, log, path in procs:
                try:
                    rc = p.wait(timeout=args.duration + 120)
                except subprocess.TimeoutExpired:
                    p.kill()
                    rc = p.wait()
                log.close()
                total += 1
                if rc != 0:
                    crashes.append((path, rc))
            print(f"round {rnd}: {total} runs, {len(crashes)} crashes, "
                  f"{int(time.time() - started)}s elapsed", flush=True)
    finally:
        sink.send_signal(signal.SIGTERM)

    for path, rc in crashes:
        sig = -rc if rc < 0 else None
        name = signal.Signals(sig).name if sig else f"exit {rc}"
        print(f"\n=== CRASH {path.name}: {name} ===", flush=True)
        print(path.read_text(errors="replace")[-3000:], flush=True)
    print(f"\ntotal runs: {total}, crashes: {len(crashes)}")
    return 1 if crashes else 0


if __name__ == "__main__":
    sys.exit(main())
