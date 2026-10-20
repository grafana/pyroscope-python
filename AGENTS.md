# AGENTS.md

## OpenSSL

- Use the OpenSSL LTS branch with the longest remaining support. Now this is 3.5 (supported until 2030-04-08). Do not move to a non-LTS branch (3.6, 4.0, ...), also not for a CVE fix: the LTS branch gets the same security fixes.
- Use the same OpenSSL version in all Pyroscope SDK repos: pyroscope-python, pyroscope-ruby, pyroscope-dotnet.
- `renovate.json` allows only patch updates for `openssl/openssl`. Move to a new branch only when a newer LTS has a longer support period.
