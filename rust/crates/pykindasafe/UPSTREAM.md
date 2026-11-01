# Upstream provenance

Vendored from https://github.com/grafana/pyroscope-kinda-safe at commit
`c789c81225750b14779d0457190631b06615f7e8` (kindasafe and kindasafe_init 0.1.1).
Copyright and license: see the unmodified upstream Apache-2.0 LICENSE.

The initial vendor commit combines kindasafe/src/lib.rs and
kindasafe_init/src/lib.rs into one std crate named pykindasafe, with the
initializer in an internal recovery module and its public API re-exported.
The upstream tests are retained with import paths renamed. Subsequent commits
carry local review fixes; they do not imply that upstream has been fixed.
