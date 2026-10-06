import logging
import os
import signal
import sys
import threading
import time

# Runs before pyroscope's after_in_child hook, holding open the window in which
# a resurrected sampler would intern names that the hook then invalidates.
os.register_at_fork(after_in_child=lambda: time.sleep(0.5))

import pyroscope  # noqa: E402


logger = logging.getLogger(__name__)
stop_burning = threading.Event()

FORKS = 30
CHILD_DEADLINE_SECONDS = 20
CHILD_PROFILE_SECONDS = 30


def configure(**kwargs):
    if not pyroscope.configure(
        application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
        server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
        enable_logging=True,
        cpu_implementation=pyroscope.ProfilerImplementation.Stack,
        **kwargs,
    ):
        raise AssertionError("configure() returned False")


def parent_burn():
    x = 0
    while not stop_burning.is_set():
        x = (x * 31 + 7) % 1000003
    return x


def fork_child_burn(deadline):
    x = 0
    while time.monotonic() < deadline:
        x = (x * 31 + 7) % 1000003
    return x


def fork_child_main():
    configure(tags={"canary": os.environ["CANARY"]})
    fork_child_burn(time.monotonic() + CHILD_PROFILE_SECONDS)
    if not pyroscope.shutdown():
        raise AssertionError("shutdown() returned False")


def wait_child(pid, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        done, status = os.waitpid(pid, os.WNOHANG)
        if done:
            return os.waitstatus_to_exitcode(status)
        time.sleep(0.01)
    os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    raise AssertionError(f"fork child {pid} hung")


def fork(child):
    pid = os.fork()
    if pid == 0:
        code = 0
        try:
            child()
        except BaseException:
            logger.exception("fork child failed")
            code = 1
        finally:
            os._exit(code)
    return pid


def main():
    logging.basicConfig(level=logging.INFO)
    configure()
    burner = threading.Thread(target=parent_burn)
    burner.start()
    try:
        time.sleep(1)
        for i in range(FORKS):
            code = wait_child(fork(lambda: None), CHILD_DEADLINE_SECONDS)
            if code != 0:
                raise AssertionError(f"fork child {i} exited with {code}")
        code = wait_child(fork(fork_child_main), CHILD_PROFILE_SECONDS + CHILD_DEADLINE_SECONDS)
        if code != 0:
            raise AssertionError(f"profiled fork child exited with {code}")
    finally:
        stop_burning.set()
        burner.join()
        pyroscope.shutdown()
    logger.info("fork workload done")


if __name__ == "__main__":
    sys.exit(main())
