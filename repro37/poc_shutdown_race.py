"""configure() / shutdown() cycles under load.

From the core dump of a reproduced crash: the crashing thread is pyroscope's
py-spy consumer thread, dropping a `Sample` (nested anyhow::error::object_drop
-> free -> abort), while py-spy's sampling thread is concurrently inside
`std::sync::mpmc::Sender::send`.

That pairing only happens at teardown. `Pyspy::shutdown_thread` sets
`running = false` and joins the consumer thread; the consumer leaves its
`for sample in sampler_output` loop and drops the `Sampler`, whose Drop does:

    self.rx = None;                                  // discards queued samples
    if let Some(t) = self.sampling_thread.take() { t.join().unwrap(); }

The receiver (and every Sample still queued in the unbounded channel) is
dropped *before* the producing thread is joined, so the discard races with a
live `send`.

This script drives that window as often as possible: a high sample rate and
deep stacks so the channel is backed up with queued samples, then an immediate
shutdown, in a loop.
"""
import os
import sys
import threading
import time

import workload


def cycle(i, threads, depth, warmup):
    import pyroscope

    stop = threading.Event()
    workers = [threading.Thread(target=workload.churn, args=(stop, depth), daemon=True)
               for _ in range(threads)]
    for t in workers:
        t.start()
    workload.configure()
    # Let the sampler fill the channel faster than the consumer drains it.
    time.sleep(warmup)
    pyroscope.shutdown()
    stop.set()
    for t in workers:
        t.join(timeout=5)


def main():
    cycles = int(os.environ.get("CYCLES", "200"))
    threads = int(os.environ.get("THREADS", "8"))
    depth = int(os.environ.get("DEPTH", "150"))
    warmup = float(os.environ.get("WARMUP", "0.5"))
    for i in range(cycles):
        cycle(i, threads, depth, warmup)
        if i % 10 == 0:
            print(f"cycle {i}", flush=True)
    print("survived", flush=True)


if __name__ == "__main__":
    sys.exit(main())
