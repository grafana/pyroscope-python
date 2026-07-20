# Vendored py-spy

This directory contains `py-spy` 0.4.2 from upstream commit
`32080cc0c22bc23938541dfa7dabb6090e40be14`.

The source includes the Python 3.14.5 support from commit
`4a905e80a1dacc907087d0ec829c622c970ca27e`:

- generated CPython 3.14.5 bindings;
- version dispatch for Python 3.14.5 and newer patch releases;
- the matching interpreter, thread, frame, object, and type implementations.

Cargo cannot apply a patch file directly to a dependency. Keeping the patched
crate as a path dependency makes builds reproducible without depending on a
separate fork.
