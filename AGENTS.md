# AGENTS.md

## Profiler integration assertions

- Never assert a profiler stack trace with a substring or `contains` check. Inspect the actual profiler output and assert complete root-to-leaf frame sequences and their sample values or proportions.
- Check the profile's shape: expected dominant stacks must account for the expected share of total weight, with documented tolerance for sampling noise. A matching function name or a single matching sample is not sufficient.
- Account explicitly for supported Python versions and implementation-specific frames. Keep the full observed profile in assertion diagnostics.

## OpenSSL

- Use the OpenSSL LTS branch with the longest remaining support. Now this is 3.5 (supported until 2030-04-08). Do not move to a non-LTS branch (3.6, 4.0, ...), also not for a CVE fix: the LTS branch gets the same security fixes.
- Use the same OpenSSL version in all Pyroscope SDK repos: pyroscope-python, pyroscope-ruby, pyroscope-dotnet.
- `renovate.json` allows only patch updates for `openssl/openssl`. Move to a new branch only when a newer LTS has a longer support period.
