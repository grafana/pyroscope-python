#!/usr/bin/env bash
set -euo pipefail
cd /home/user/pyroscope-ctimer-fixed-3638f62
tar xzf source-context.tar.gz
sudo -n docker build -f context/Builder.Dockerfile -t pyroscope-python-bench-build:3638f62 context > wheel-build.log 2>&1
mkdir -p runtime/wheels
container_id=$(sudo -n docker create pyroscope-python-bench-build:3638f62)
sudo -n docker cp "$container_id:/fixed-wheel/." runtime/wheels/
sudo -n docker rm "$container_id" >/dev/null
cp Dockerfile runtime/Dockerfile
cp -a /home/user/pyroscope-python-bench-r7CPmW/context/app runtime/app
cp /home/user/pyroscope-python-bench-r7CPmW/loadgen ./loadgen
python3 - <<'PY'
import hashlib,json,zipfile
from pathlib import Path
p=next(Path('runtime/wheels').glob('*.whl'))
source=json.loads(Path('source.json').read_text())
source['wheel_filename']=p.name
source['wheel_sha256']=hashlib.sha256(p.read_bytes()).hexdigest()
with zipfile.ZipFile(p) as z:
    name=next(n for n in z.namelist() if n.startswith('pyroscope/_native') and n.endswith('.so'))
    source['sdk_native_sha256']=hashlib.sha256(z.read(name)).hexdigest()
Path('source.json').write_text(json.dumps(source,indent=2)+'\n')
print(json.dumps(source),flush=True)
PY
tar czf runtime-context.tar.gz runtime source.json
sudo -n docker build -t pyroscope-python-bench:fixed-3638f62 runtime > runtime-build.log 2>&1
echo BUILD_COMPLETE
