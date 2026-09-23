# pyroscope-python

Pyroscope continuous profiling agent for Python applications.

Uses [py-spy](https://github.com/benfred/py-spy) for stack sampling and the [pyroscope](https://crates.io/crates/pyroscope) Rust crate to send profiles to a Pyroscope server.

## Installation

```bash
pip install pyroscope-io
```

## Building from source

Clone with the memory profiler submodule:

```bash
git clone --recurse-submodules https://github.com/grafana/pyroscope-python.git
```

For an existing checkout, initialize or update it after pulling changes:

```bash
git submodule update --init --recursive
```

Run this before building locally or preparing a Docker build context. Published
source distributions already contain the native sources and do not require Git.

The `cpp/` submodule points to the public
[Pyroscope memory profiler fork](https://github.com/korniltsev-grafanista-yolo-vibecoder239/pyroscope-memory-profiler).
Its `pyroscope` branch preserves the history of DataDog/dd-trace-py v4.11.1
(`823648d32ed5d2ab843be193a3abcc4d63feb1f7`), followed by separate pruning,
relocation, and Pyroscope patch commits. The patch reproduces the native files
from pyroscope-python commit `33c30bb8951585b148654646e8defeb23985be44`
([original import](https://github.com/grafana/pyroscope-python/pull/83)).
The parent repository pins an exact submodule commit; builds do not follow the
fork's branch automatically.

Inspect the pinned patch and earlier history with:

```bash
git -C cpp show HEAD
git -C cpp log --follow -- _memalloc.cpp
```

## License

Apache-2.0
