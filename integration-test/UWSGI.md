# uWSGI integration test

`TestPythonProfilerUWSGI` builds uWSGI 2.0.31 from its checksum-pinned source
distribution against the selected Python image. Its compiler and Python build
dependencies are confined to that test image. The SDK wheel is built separately
and installed offline, using the existing harness settings.

Each profiler runs in two loading modes:

| Subtest | Python loading |
| --- | --- |
| `embedded` | `uwsgi` includes the Python plugin and links directly to libpython. |
| `plugin` | `uwsgi-core --plugin /usr/local/lib/uwsgi/python_plugin.so` dlopens the Python plugin, whose dependencies load libpython. |

Both executables and the shared plugin come from the same source archive. The
build cleans object files before compiling the core and uses uWSGI's `core`
profile for both the core and the shared plugin. Docker build assertions check
that libpython is a `DT_NEEDED` dependency of `uwsgi` and `python_plugin.so`,
and is absent from `uwsgi-core`. This guards against accidentally exercising
the embedded layout twice.

Two uWSGI worker processes serve real HTTP requests for 15 seconds. Every request
executes the same two arithmetic loops through distinct Python call paths and
returns a checked result, worker ID and PID. Both workers must serve requests;
PID changes fail the test. Profilers initialize separately after the workers
fork (`--lazy-apps`), with Python threads enabled and a single interpreter per
worker. `--py-call-uwsgi-fork-hooks` enables CPython's fork bookkeeping.

After the requests finish, the test stops uWSGI, verifies both workers shut down
their profilers, and queries the profiles uploaded to the real Pyroscope server.
The existing container helpers remove the containers and network on failures as
well as success. The test removes its image tag after all selected subtests.

For **each worker**, these complete root-to-leaf stacks must each receive
35–65% of all sample weight and together cover at least 90%:

```text
application;workload;first_branch;burn_cpu
application;workload;second_branch;burn_cpu
```

The fixed loops do equal work. The 15 percentage point tolerance allows sampling
and scheduling noise; the remaining 10% allows startup, HTTP/JSON bookkeeping
and dropped samples. Each worker must also have at least 2,000,000,000 ns of
sample weight (200 periods at 100 Hz). The denominator includes every unexpected
stack. The shared shape helper logs the full observed profile on both success
and assertion failure. uWSGI enters Python directly from C, so the observed
request stacks have neither a module nor a Python thread bootstrap prefix.

Run from the repository root:

```sh
make -C integration-test test/TestPythonProfilerUWSGI
PYTHON_VERSION=3.14 make -C integration-test test/TestPythonProfilerUWSGI PROFILER=signal
PYTHON_VERSION=3.12 PYTHON_IMAGE_SUFFIX=alpine make -C integration-test test/TestPythonProfilerUWSGI
```

`PROFILER=all` selects py-spy, Signal ITimer, Signal CTimer and Stack.
`py-spy`, `signal`, `stack`, and the existing `dd-trace-py` alias also work.
With prebuilt wheels, set `PYROSCOPE_WHEEL_DIR` to their absolute directory, or
run `go test -v -timeout 20m -count=1 -run '^TestPythonProfilerUWSGI$' . -profiler=all`
from `integration-test`. `PYROSCOPE_TEST_LOG_DIR` preserves all container logs.
To select just one loading mode, use Go's subtest selector, for example:

```sh
cd integration-test
go test -v -timeout 20m -count=1 -run '^TestPythonProfilerUWSGI$/^plugin$' . -profiler=all
```

`-profiler` applies to both loading modes. Each selected mode gets its own
server, requests, worker PID checks, profile queries and cleanup.

The existing CI invocation discovers the test automatically, including its
glibc/musl and amd64/arm64 matrix. The Signal subtests reuse the existing
CPython 3.11–3.14 support guard; Python 3.15 still runs py-spy and Stack.
The existing CI exclusion for Python 3.11 Alpine on arm64 remains applicable.
Stack uses its wall sampler on 3.11 and CPU timers on 3.12+; both produce the
same complete request paths here.

uWSGI has an upstream [Python 3.14.2 fork/lock crash report](https://github.com/unbit/uwsgi/issues/2748).
Local Python 3.14.10 probes of a minimal WSGI app reached HTTP readiness and
stopped cleanly both with and without the fork-hook option. The profiling test
uses the documented fork hooks and has no blanket skip for 3.14 or 3.15.
See [uWSGI's options](https://uwsgi.readthedocs.io/en/latest/Options.html#py-call-uwsgi-fork-hooks)
and [application loading guidance](https://uwsgi.readthedocs.io/en/latest/ThingsToKnow.html).

## Local validation, 2026-11-17

Based on freshly fetched `upstream/main`, commit
`faf464d692f7f2345423f415c6ddbce995e51657`, in branch `test/uwsgi-integration`.
Host: Linux amd64, Docker 29.1.3, Go 1.26.10. The local Docker socket required
`sudo -n`; `.cache/bin/docker` delegates to `sudo -n /usr/bin/docker`, and Go
1.26.10 was installed in `.cache/go`. The commands below used:

```sh
export PATH="$PWD/.cache/bin:$PWD/.cache/go/bin:$PATH"
git fetch upstream
git rebase upstream/main
git -c url.https://github.com/.insteadOf=git@github.com: submodule update --init --recursive
make linux/amd64
```

`make musllinux/amd64` hit a Docker bridge DNS failure resolving
`release-assets.githubusercontent.com`. The same build was retried with host
networking, without changing the SDK Dockerfile or its OpenSSL version:

```sh
docker buildx build --network=host --build-arg=PLATFORM=x86_64 \
  --platform=linux/amd64 --output=. -f docker/wheel-musllinux.Dockerfile .
```

The current test/build transcripts and complete container logs are retained
locally under `.cache/uwsgi-validation-both/` (ignored by Git). Earlier
embedded-only and ASAN results remain in `.cache/uwsgi-validation/`.

The loading modes were also inspected with these commands:

```sh
docker build --build-arg PYTHON_IMAGE=python:3.12-slim \
  -t pyroscope-python-uwsgi-dual-linkage:local \
  -f integration-test/testdata/uwsgi.Dockerfile integration-test/testdata
docker run --rm pyroscope-python-uwsgi-dual-linkage:local sh -ec \
  'for binary in /usr/local/bin/uwsgi /usr/local/bin/uwsgi-core /usr/local/lib/uwsgi/python_plugin.so; do printf "\n%s\n" "$binary"; readelf -d "$binary"; done'
docker run --rm -e LD_DEBUG=files pyroscope-python-uwsgi-dual-linkage:local \
  uwsgi-core --plugin /usr/local/lib/uwsgi/python_plugin.so --python-version
docker image rm pyroscope-python-uwsgi-dual-linkage:local
```

`readelf` confirmed the expected dependencies, and the glibc loader reported:

```text
file=/usr/local/lib/uwsgi/python_plugin.so [0]; dynamically loaded by uwsgi-core [0]
file=libpython3.12.so.1.0 [0]; needed by /usr/local/lib/uwsgi/python_plugin.so [0]
```

Matrix runs used the following command, with the Python version and image
suffix set to each table entry (`PROFILER` defaults to `all`):

```sh
PYTHON_VERSION=3.12 PYTHON_IMAGE_SUFFIX=alpine \
  PYROSCOPE_TEST_LOG_DIR="$PWD/.cache/uwsgi-validation-both/containers-3.12-alpine" \
  make -C integration-test test/TestPythonProfilerUWSGI PYROSCOPE_WHEEL_DIR="$PWD/dist"
```

| Python observed in server logs | glibc (`slim`) | musl (`alpine`) |
| --- | --- | --- |
| 3.11.18 | PASS, 202.494s | PASS, 208.786s |
| 3.12.16 | PASS, 198.251s | PASS, 193.581s |
| 3.13.18 | PASS, 184.609s | PASS, 221.388s |
| 3.14.10 | PASS, 216.163s | PASS, 214.953s |
| 3.15.2 | PASS, 119.351s | PASS, 128.532s |

All 72 applicable mode/profiler/version/libc cases passed, with eight explicit
Signal/Python 3.15 skips. Each case has two independently checked worker profiles
(144 profiles total). Across these captures, each leaf received 46.38–53.62% of
the weight, combined coverage was at least 99.45%, and each worker contributed
at least 8.94 seconds of sample weight. Full captures were inspected before
finalizing the assertions; both loading modes produce the same complete workload
paths. For example, the complete Stack profile for worker 2 using the shared
plugin on Python 3.12.16/glibc was (values in nanoseconds):

```text
;application 10027000
;application;dumps;JSONEncoder.encode;JSONEncoder.iterencode 10838000
;application;workload;first_branch;burn_cpu 7488004000
;application;workload;second_branch;burn_cpu 7473531000
```

The initial Alpine builds failed with `linux/limits.h: No such file or directory`;
adding `linux-headers` fixed them. The ASAN runtime first failed uWSGI linking
with missing `-lcrypt`, then unresolved `__asan_*` symbols. The test image now
installs `libcrypt-dev` on Debian/Ubuntu and passes Python's `sysconfig` linker
flags to both executables and the shared plugin. Profiler and mode selection
checks also passed:

```sh
cd integration-test
PYTHON_VERSION=3.13 \
  PYROSCOPE_TEST_LOG_DIR="$PWD/../.cache/uwsgi-validation-both/containers-selection-plugin-stack" \
  go test -v -timeout 20m -count=1 -run '^TestPythonProfilerUWSGI$/^plugin$' . -profiler=dd-trace-py
# PASS, 22.386s; only plugin/stack ran
PYTHON_VERSION=3.15 go test -v -count=1 -run '^TestPythonProfilerUWSGI$' . -profiler=signal
# PASS, 0.004s; all eight leaf subtests skipped without building an image or requiring a wheel
```

## Earlier ASAN results (embedded mode)

ASAN was not rerun for the loading-mode expansion. During the initial
embedded-only implementation, the standard ASAN command was attempted:

```sh
make asan/test ASAN_PYTHON_REF=3.12 ASAN_IMAGE=pyroscope-python-uwsgi-asan:3.12 \
  RUN='^TestPythonProfilerUWSGI$'
```

It built CPython and the SDK wheel, then stalled in the existing kindasafe
`reports_faulting_reads_after_recovery` and `reports_poisoned_reads` checks.
Their probe subprocesses each consumed a CPU for 2m07s before the build was
stopped. This used Rust 1.102.0-nightly (2026-11-16). To validate uWSGI separately,
the completed wheel layer was exported using a local copy of the Dockerfile
without that final native-test step; no SDK/build-source changes are included:

```sh
python3 - <<'PY'
from pathlib import Path
source = Path('docker/asan.Dockerfile').read_text()
prefix, rest = source.split('# Exercise kindasafe\'s naked-load ASAN checks.\n', 1)
_, export = rest.split('FROM scratch AS wheel\n', 1)
Path('.cache/uwsgi-validation/asan-wheel-only.Dockerfile').write_text(
    prefix + 'FROM scratch AS wheel\n' + export
)
PY
docker buildx build --build-arg=PYTHON_REF=3.12 --target=wheel --output=dist/asan \
  -f .cache/uwsgi-validation/asan-wheel-only.Dockerfile .
sudo -n chown -R "$(id -u):$(id -g)" dist/asan
cd integration-test
PYTHON_IMAGE=pyroscope-python-uwsgi-asan:3.12 PYTHON_VERSION=3.12 \
  PYROSCOPE_WHEEL_DIR="$PWD/../dist/asan" PYROSCOPE_WHEEL_GLOB='*.whl' \
  PYROSCOPE_TEST_LOG_DIR="$PWD/../.cache/uwsgi-validation/containers-asan-312-e2e" \
  go test -v -timeout 20m -count=1 -run '^TestPythonProfilerUWSGI$' . -profiler=all
```

Result: **FAIL, 69.853s**. py-spy (21.76s), Signal ITimer (21.66s), and Signal
CTimer (21.39s) passed with the expected profiles. Stack failed (5.01s) with an
ASAN heap-buffer-overflow: `pyunicode_to_utf8` copies a 64-byte `PyUnicodeObject`
from a 49-byte compact ASCII allocation while resolving a function name.
Both workers aborted and the HTTP clients received EOF. The full report is in
`.cache/uwsgi-validation/containers-asan-312-e2e/TestPythonProfilerUWSGI/stack/bd2a76c28bb8.log`.

The unchanged `TestPythonStackProfilerOnCPU` reproduced the same 64-byte over-read
in `pyunicode_to_utf8`, independently of uWSGI:

```sh
# From integration-test, using the same ASAN image and wheel:
PYTHON_IMAGE=pyroscope-python-uwsgi-asan:3.12 PYTHON_VERSION=3.12 \
  PYROSCOPE_WHEEL_DIR="$PWD/../dist/asan" PYROSCOPE_WHEEL_GLOB='*.whl' \
  PYROSCOPE_TEST_LOG_DIR="$PWD/../.cache/uwsgi-validation/containers-asan-existing-stack" \
  go test -v -timeout 5m -count=1 -run '^TestPythonStackProfilerOnCPU$' . -profiler=stack
# FAIL, 7.992s; AddressSanitizer: heap-buffer-overflow
```

The new test retains the failure. Fixing the C++ string reader and investigating
the existing kindasafe probes remain separate SDK work. Local ARM64 execution
and other ASAN Python versions were not tested.

Formatting and focused checks passed:

```sh
gofmt -l integration-test/uwsgi_test.go
# No output
uvx --from ruff==0.17.3 ruff format --check integration-test/testdata/uwsgi_workload.py
uvx --from ruff==0.17.3 ruff check --select I integration-test/testdata/uwsgi_workload.py
git diff --check
cd integration-test
go test -v -count=1 -run '^TestConfigurationProfileShape$' .
go test -race -count=1 -run '^TestConfigurationProfileShape$' .
go vet ./...
```
