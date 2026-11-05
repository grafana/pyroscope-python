"""Install the same checksum-verified Pyroscope release on Linux and macOS."""

import hashlib
import io
import platform
import sys
import tarfile
from pathlib import Path
from urllib.request import urlopen

VERSION = "2.3.1"


def main():
    system = platform.system().lower()
    arch = {"x86_64": "amd64", "aarch64": "arm64", "arm64": "arm64"}[platform.machine()]
    if system not in ("linux", "darwin"):
        raise RuntimeError(f"unsupported platform: {system}")
    name = f"pyroscope_{VERSION}_{system}_{arch}.tar.gz"
    base = f"https://github.com/grafana/pyroscope/releases/download/v{VERSION}"
    with urlopen(f"{base}/checksums.txt", timeout=60) as response:
        checksums = dict(
            line.split()[::-1] for line in response.read().decode().splitlines()
        )
    with urlopen(f"{base}/{name}", timeout=120) as response:
        data = response.read()
    if hashlib.sha256(data).hexdigest() != checksums[name]:
        raise RuntimeError(f"checksum mismatch for {name}")
    destination = Path(sys.argv[1]).resolve()
    destination.mkdir(parents=True, exist_ok=True)
    # Extract only the executable; never use paths supplied by the archive.
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        member = archive.getmember("pyroscope")
        if not member.isfile():
            raise RuntimeError("pyroscope archive member is not a regular file")
        executable = destination / "pyroscope"
        with archive.extractfile(member) as source:
            executable.write_bytes(source.read())
    executable.chmod(0o755)
    print(executable)


if __name__ == "__main__":
    main()
