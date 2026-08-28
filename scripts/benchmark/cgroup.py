"""Read CPU and memory counters from the caller's own cgroup v2 node.

Every measured run is launched inside its own transient systemd scope, so the
scope's cgroup counts exactly one run: the Python threads, the Rust sampler and
uploader threads, and -- for the gunicorn shape -- the whole process tree.
That is why this is the headline metric rather than time.process_time().
"""

import os
from pathlib import Path

CGROUP_ROOT = Path("/sys/fs/cgroup")


def own_cgroup_path():
    """Resolve this process's cgroup v2 directory.

    cgroup v2 gives a single line, "0::<path>", where <path> is relative to the
    v2 mount root. Resolved once at startup: the path is stable for the lifetime
    of the scope.
    """
    with open("/proc/self/cgroup") as f:
        for line in f:
            hid, controllers, path = line.rstrip("\n").split(":", 2)
            if hid == "0" and controllers == "":
                return CGROUP_ROOT / path.lstrip("/")
    raise RuntimeError("no cgroup v2 entry in /proc/self/cgroup (v1-only host?)")


def _read_flat_keyed(path):
    out = {}
    with open(path) as f:
        for line in f:
            parts = line.split()
            if len(parts) == 2:
                out[parts[0]] = int(parts[1])
    return out


class CgroupCounters:
    """Snapshot source for one cgroup node."""

    def __init__(self, path=None):
        self.path = Path(path) if path else own_cgroup_path()
        # Fail loudly at construction rather than mid-run with a partial result.
        for required in ("cpu.stat", "memory.current"):
            if not (self.path / required).exists():
                raise RuntimeError(
                    f"{self.path / required} missing; the scope needs "
                    "-p CPUAccounting=yes -p MemoryAccounting=yes"
                )

    def cpu_usec(self):
        return _read_flat_keyed(self.path / "cpu.stat")["usage_usec"]

    def cpu_breakdown(self):
        st = _read_flat_keyed(self.path / "cpu.stat")
        return {
            "usage_usec": st["usage_usec"],
            "user_usec": st.get("user_usec", 0),
            "system_usec": st.get("system_usec", 0),
        }

    def memory_current(self):
        return int((self.path / "memory.current").read_text().strip())

    def memory_peak(self):
        # memory.peak arrived in 5.19; fall back to current so a run on an older
        # kernel degrades to a usable-but-labelled number rather than crashing.
        p = self.path / "memory.peak"
        if p.exists():
            return int(p.read_text().strip())
        return self.memory_current()

    def snapshot(self):
        s = self.cpu_breakdown()
        s["memory_current"] = self.memory_current()
        s["memory_peak"] = self.memory_peak()
        return s


def nproc_effective():
    return len(os.sched_getaffinity(0))
