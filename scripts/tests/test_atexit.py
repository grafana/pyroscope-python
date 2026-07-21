import logging
import subprocess
import sys


def child():
    import pyroscope

    # DEBUG level makes the native agent log "Agent Shutdown" on stderr when
    # it stops, which is the parent's proof that the atexit hook ran.
    logging.getLogger().setLevel(logging.DEBUG)
    pyroscope.configure(
        application_name='pyroscope.atexit-test',
        server_address='http://localhost:4040',
        enable_logging=True,
        mem_enabled=True,
    )
    # Leave some sampled allocations behind so the final flush at exit walks
    # a non-trivial heap.
    retained = [bytearray(64 * 1024) for _ in range(64)]
    assert len(retained) == 64
    # Exit without calling pyroscope.shutdown(): the atexit hook registered
    # by the native module must stop the agent before interpreter
    # finalization, when the agent thread can no longer attach to Python.


def main():
    result = subprocess.run(
        [sys.executable, __file__, 'child'],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=60,
        text=True,
    )
    sys.stderr.write(result.stderr)

    if result.returncode != 0:
        raise AssertionError(
            f'child exited with {result.returncode} instead of 0'
        )
    if 'panicked' in result.stderr:
        raise AssertionError('child stderr contains a native panic')
    if 'Agent Shutdown' not in result.stderr:
        raise AssertionError(
            'the atexit hook did not stop the agent before exit'
        )
    print('good atexit shutdown')


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == 'child':
        child()
    else:
        main()
