# AGENTS.md

## OpenSSL

- Use the OpenSSL LTS branch with the longest remaining support. Now this is 3.5 (supported until 2030-04-08). Do not move to a non-LTS branch (3.6, 4.0, ...), also not for a CVE fix: the LTS branch gets the same security fixes.
- Use the same OpenSSL version in all Pyroscope SDK repos: pyroscope-python, pyroscope-ruby, pyroscope-dotnet.
- `renovate.json` allows only patch updates for `openssl/openssl`. Move to a new branch only when a newer LTS has a longer support period.

## Fault recovery signal handlers

- The purpose of kindasafe reads is to read memory directly without syscalls on the successful memory-load path. Preserve this design goal: do not replace them with syscall-based memory reads or add per-read syscalls.
- The Python stack unwinder assumes kindasafe's `SIGSEGV`/`SIGBUS` handlers remain installed after successful initialization. SDK users must ensure their application and dependencies do not change these handlers during installation or afterward, including across profiler stops/restarts.
- Install other handlers before profiler initialization. Previously installed callbacks must support direct chaining and remain loaded.
- Do not add runtime handler ownership checks or automatically reclaim/reinstall handlers. Initialization is cached once; handler replacement violates the SDK contract and may crash the process.
- Successful recovery initialization before unwinding and unblocked `SIGSEGV`/`SIGBUS` throughout the walk are caller preconditions, including when sampling interrupts another signal handler. Do not add runtime initialization or signal-mask checks to the unwinder. Blocked recovery signals do not skip samples; a failed read can crash the process.
- Keep this contract documented in `README.md` and the `python-unwind` crate docs when changing recovery behavior.
