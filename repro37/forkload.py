"""Fork-shaped variant of the workload, mirroring the setups in the reports
(gunicorn sync workers / celery prefork): a parent that has the agent running
forks children, each child starts its own agent and does the same interpreter
churn, and children are recycled continuously.

This exercises the at-fork paths (os.register_at_fork handlers installed by the
extension) as well as the sampler, at a much higher fork rate than a real
worker pool.
"""
import os
import signal
import sys
import time

import workload


def child(duration, nthreads, depth):
    # A freshly forked worker starts its own agent, the way gunicorn's
    # post_fork hook does.
    workload.configure()
    workload.run(duration, nthreads, depth)
    os._exit(0)


def main():
    duration = float(os.environ.get("DURATION", "60"))
    child_lifetime = float(os.environ.get("CHILD_LIFETIME", "5"))
    nchildren = int(os.environ.get("CHILDREN", "4"))
    nthreads = int(os.environ.get("THREADS", "4"))
    depth = int(os.environ.get("DEPTH", "40"))
    parent_agent = os.environ.get("PARENT_AGENT", "1") == "1"

    if parent_agent:
        # The pre-fork master is profiled too, so children fork out of a
        # process that has the sampler threads running.
        workload.configure()

    deadline = time.time() + duration
    kids = {}
    failures = []
    while time.time() < deadline:
        while len(kids) < nchildren:
            pid = os.fork()
            if pid == 0:
                child(min(child_lifetime, max(deadline - time.time(), 1)), nthreads, depth)
            kids[pid] = time.time()
        pid, status = os.wait()
        kids.pop(pid, None)
        if os.WIFSIGNALED(status):
            sig = os.WTERMSIG(status)
            failures.append((pid, signal.Signals(sig).name))
            print(f"CHILD {pid} killed by {signal.Signals(sig).name}", flush=True)
        elif os.WEXITSTATUS(status) != 0:
            failures.append((pid, f"exit {os.WEXITSTATUS(status)}"))
            print(f"CHILD {pid} exited {os.WEXITSTATUS(status)}", flush=True)

    for pid in list(kids):
        try:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        except OSError:
            pass
    print(f"forkload finished, {len(failures)} child failures", file=sys.stderr, flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
