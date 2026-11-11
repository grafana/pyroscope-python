"""Recreate the SDK build context from the recorded Git revisions."""
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile


def export(repository, revision, destination):
    destination.mkdir(parents=True, exist_ok=True)
    data = subprocess.check_output([
        'git', '-C', str(repository), 'archive', revision,
    ])
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        archive.extractall(destination, filter='data')


def main():
    here = Path(__file__).resolve().parent
    repository = here.parents[1]
    revision = json.loads((here/'source.json').read_text())['profiler_commit']
    with tempfile.TemporaryDirectory(prefix='profiler-source-export-') as temporary:
        context = Path(temporary)/'context'
        export(repository, revision, context/'profiler')
        for name in ('dd-trace-py', 'kindasafe', 'py-spy'):
            entry = subprocess.check_output([
                'git', '-C', str(repository), 'ls-tree', revision, name,
            ], text=True).split()
            if len(entry) < 3 or entry[0] != '160000':
                raise RuntimeError(f'{name} is not a submodule at {revision}')
            export(repository/name, entry[2], context/'profiler'/name)
        shutil.copyfile(here/'Builder.Dockerfile', context/'Builder.Dockerfile')
        destination = here/'source-context.tar.gz'
        with tarfile.open(destination, 'w:gz') as archive:
            archive.add(context, arcname='context')
        print(destination)


if __name__ == '__main__':
    main()
