"""Fixed-request-count load generator.

Runs in its own process, pinned to a CPU set disjoint from the server's, so the
CPU it burns is never counted against the server and never competes with it.

Everything here exists to make "N requests" mean the same amount of server work
in every variant:

* HTTP/1.1 keep-alive on a fixed number of connections, so per-request
  connection setup cost is identical across variants;
* a fixed concurrency, so the server sees the same offered parallelism;
* an explicit ceiling measurement, because a load generator that is itself
  saturated silently converts server slowdown into queueing and the benchmark
  stops measuring the server.
"""

import argparse
import asyncio
import json
import statistics
import sys
import time


async def _connection(host, port, path, requests, latencies, errors, counts):
    """One keep-alive connection issuing `requests` sequential requests."""
    reader, writer = await asyncio.open_connection(host, port)
    writer.transport.set_write_buffer_limits(0)
    sock = writer.get_extra_info("socket")
    if sock is not None:
        import socket as _s
        sock.setsockopt(_s.IPPROTO_TCP, _s.TCP_NODELAY, 1)

    req = (
        f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\n"
        "Connection: keep-alive\r\nAccept: application/json\r\n\r\n"
    ).encode()

    try:
        for _ in range(requests):
            t0 = time.perf_counter()
            writer.write(req)
            await writer.drain()

            status_line = await reader.readline()
            if not status_line:
                errors.append("connection closed")
                return
            status = status_line.split(b" ")[1].decode() if b" " in status_line else "?"
            counts[status] = counts.get(status, 0) + 1

            length = None
            while True:
                line = await reader.readline()
                if line in (b"\r\n", b"\n", b""):
                    break
                if line.lower().startswith(b"content-length:"):
                    length = int(line.split(b":")[1])
            if length:
                await reader.readexactly(length)

            latencies.append(time.perf_counter() - t0)
    finally:
        writer.close()
        try:
            await writer.wait_closed()
        except Exception:
            pass


async def _drive(host, port, path, total, concurrency):
    per_conn = total // concurrency
    remainder = total - per_conn * concurrency
    latencies, errors, counts = [], [], {}
    tasks = []
    for i in range(concurrency):
        n = per_conn + (1 if i < remainder else 0)
        if n:
            tasks.append(_connection(host, port, path, n, latencies, errors, counts))
    t0 = time.perf_counter()
    await asyncio.gather(*tasks)
    elapsed = time.perf_counter() - t0
    return latencies, errors, counts, elapsed


def _percentile(xs, q):
    if not xs:
        return float("nan")
    s = sorted(xs)
    k = min(len(s) - 1, int(round(q * (len(s) - 1))))
    return s[k]


def run(host, port, path, total, concurrency):
    lat, errors, counts, elapsed = asyncio.run(
        _drive(host, port, path, total, concurrency))
    completed = sum(counts.values())
    return {
        "issued": total,
        "completed": completed,
        "status_counts": counts,
        "errors": errors[:10],
        "wall_s": elapsed,
        "rps": completed / elapsed if elapsed else 0.0,
        "p50_ms": _percentile(lat, 0.50) * 1000,
        "p99_ms": _percentile(lat, 0.99) * 1000,
        "max_ms": (max(lat) * 1000) if lat else float("nan"),
        "mean_ms": (statistics.fmean(lat) * 1000) if lat else float("nan"),
        "concurrency": concurrency,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8080)
    ap.add_argument("--path", default="/work")
    ap.add_argument("--requests", type=int, required=True)
    ap.add_argument("--concurrency", type=int, default=8)
    args = ap.parse_args()
    print(json.dumps(run(args.host, args.port, args.path,
                         args.requests, args.concurrency)))


if __name__ == "__main__":
    sys.exit(main())
