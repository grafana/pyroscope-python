import logging
import os
import subprocess
import sys


def child():
    import pyroscope

    logging.getLogger().setLevel(logging.DEBUG)
    pyroscope.configure(
        application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
        server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
        enable_logging=True,
        mem_enabled=True,
    )
    retained = [bytearray(64 * 1024) for _ in range(64)]
    assert len(retained) == 64


def main():
    result = subprocess.run(
        [sys.executable, __file__, "child"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=60,
        text=True,
    )
    sys.stderr.write(result.stderr)

    if result.returncode != 0:
        raise AssertionError(
            f"child exited with {result.returncode} instead of 0"
        )
    if "panicked" in result.stderr:
        raise AssertionError("child stderr contains a native panic")
    if "Agent Shutdown" not in result.stderr:
        raise AssertionError(
            "the atexit hook did not stop the agent before exit"
        )
    print("good atexit shutdown")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "child":
        child()
    else:
        main()
