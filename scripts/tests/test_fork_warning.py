import os
import warnings

import pyroscope


WARNING_TEXT = "Forking after Pyroscope starts is unsupported"


def exercise_memory_allocations():
    allocations = [
        {"index": index, "payload": f"allocation-{index}"}
        for index in range(50_000)
    ]
    if len(allocations) != 50_000:
        raise AssertionError("failed to create memory-profiler test allocations")
    return allocations


def fork_and_capture_pyroscope_warnings(exercise_child_memory=False):
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        pid = os.fork()
        if pid == 0:
            if exercise_child_memory:
                try:
                    exercise_memory_allocations()
                except BaseException:
                    os._exit(1)
            os._exit(0)

        _, status = os.waitpid(pid, 0)
        if os.waitstatus_to_exitcode(status) != 0:
            raise AssertionError(f"forked child exited with status {status}")

    return [warning for warning in caught if WARNING_TEXT in str(warning.message)]


def fork_and_configure_in_child():
    pid = os.fork()
    if pid == 0:
        exit_code = 1
        try:
            configured = pyroscope.configure(
                application_name="pyroscope.fork-child-test"
            )
            if configured and pyroscope.shutdown():
                exit_code = 0
        finally:
            os._exit(exit_code)

    _, status = os.waitpid(pid, 0)
    if os.waitstatus_to_exitcode(status) != 0:
        raise AssertionError(
            "child failed to configure a new Pyroscope agent after fork"
        )


def main():
    if fork_and_capture_pyroscope_warnings():
        raise AssertionError("Pyroscope warned before the agent was started")

    pyroscope.configure(application_name="pyroscope.fork-warning-test")
    try:
        fork_and_configure_in_child()
        active_warnings = fork_and_capture_pyroscope_warnings(
            exercise_child_memory=True
        )
        if len(active_warnings) != 1:
            raise AssertionError(
                "expected exactly one Pyroscope fork warning while the agent "
                f"was running, got {len(active_warnings)}"
            )
        if active_warnings[0].category is not DeprecationWarning:
            raise AssertionError(
                "expected the Pyroscope fork warning to be a DeprecationWarning"
            )
        exercise_memory_allocations()
    finally:
        pyroscope.shutdown()

    if fork_and_capture_pyroscope_warnings():
        raise AssertionError("Pyroscope warned after the agent was shut down")


if __name__ == "__main__":
    main()
