"""Keep one mixed Python stack alive throughout the synchronous Rust benchmarks."""


def leaf(bench):
    bench.run(["mixed", *DEEP_NAMES, *(["recurse"] * 32), *(["map_step"] * 16), "leaf"])


# Separate code objects distinguish a deep call chain from repeated recursive frames.
DEEP_NAMES = [f"deep_{i:02d}" for i in range(32)]
for i, name in enumerate(DEEP_NAMES):
    body = (
        f"{DEEP_NAMES[i + 1]}(bench)"
        if i + 1 < len(DEEP_NAMES)
        else "recurse(bench, 31)"
    )
    exec(compile(f"def {name}(bench):\n    {body}\n", __file__, "exec"))


def mixed(bench):
    deep_00(bench)


def recurse(bench, remaining):
    if remaining:
        recurse(bench, remaining - 1)
    else:
        list(map(map_step, [(bench, 15)]))


def map_step(args):
    bench, remaining = args
    if remaining:
        list(map(map_step, [(bench, remaining - 1)]))
    else:
        leaf(bench)
