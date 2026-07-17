import os
import warnings

import pyroscope


WARNING_TEXT = "Forking after Pyroscope starts is unsupported"


def fork_and_capture_pyroscope_warnings():
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        pid = os.fork()
        if pid == 0:
            os._exit(0)

        _, status = os.waitpid(pid, 0)
        if os.waitstatus_to_exitcode(status) != 0:
            raise AssertionError(f"forked child exited with status {status}")

    return [warning for warning in caught if WARNING_TEXT in str(warning.message)]


def main():
    if fork_and_capture_pyroscope_warnings():
        raise AssertionError("Pyroscope warned before the agent was started")

    pyroscope.configure(application_name="pyroscope.fork-warning-test")
    try:
        active_warnings = fork_and_capture_pyroscope_warnings()
        if len(active_warnings) != 1:
            raise AssertionError(
                "expected exactly one Pyroscope fork warning while the agent "
                f"was running, got {len(active_warnings)}"
            )
        if active_warnings[0].category is not DeprecationWarning:
            raise AssertionError(
                "expected the Pyroscope fork warning to be a DeprecationWarning"
            )
    finally:
        pyroscope.shutdown()

    if fork_and_capture_pyroscope_warnings():
        raise AssertionError("Pyroscope warned after the agent was shut down")


if __name__ == "__main__":
    main()
