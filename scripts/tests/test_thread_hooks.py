"""Exercise the Python thread hooks without loading the native extension."""

import importlib.util
import sys
import threading
import types
import unittest
from pathlib import Path
from unittest.mock import patch


def load_hooks():
    native = types.ModuleType("pyroscope._native")
    native.LineNo = types.SimpleNamespace(LastInstruction=0)
    native.ProfilerImplementation = types.SimpleNamespace(PySpy=0)
    native.SignalTimer = types.SimpleNamespace(ITimer=0)
    path = Path(__file__).resolve().parents[2] / "python/pyroscope/__init__.py"
    spec = importlib.util.spec_from_file_location(
        "pyroscope", path, submodule_search_locations=[str(path.parent)]
    )
    module = importlib.util.module_from_spec(spec)
    with patch.dict(sys.modules, {native.__name__: native, "pyroscope": module}):
        spec.loader.exec_module(module)
    return module


class ThreadHookTests(unittest.TestCase):
    def test_preexisting_new_and_raising_threads_with_two_hook_callers(self):
        hooks = load_hooks()
        original_set_native_id = threading.Thread._set_native_id
        original_bootstrap_inner = threading.Thread._bootstrap_inner
        original_delete = threading.Thread._delete
        original_excepthook = threading.excepthook
        release = threading.Event()
        ready = threading.Event()
        release_new = threading.Event()
        ready_new = threading.Event()
        calls = {
            name: {"register": [], "unregister": []} for name in ("stack", "signal")
        }

        def worker():
            ready.set()
            self.assertTrue(release.wait(5))

        preexisting = threading.Thread(target=worker)
        preexisting.start()
        self.assertTrue(ready.wait(5))

        def install(name):
            hooks._install_stack_threads(
                threading,
                lambda ident, native: calls[name]["register"].append((ident, native)),
                lambda ident: calls[name]["unregister"].append(ident),
            )

        try:
            install("stack")
            install("signal")
            if sys.version_info >= (3, 11):
                self.assertIs(
                    threading.Thread._bootstrap_inner, original_bootstrap_inner
                )
                self.assertIsNot(threading.Thread._delete, original_delete)
            else:
                self.assertIs(threading.Thread._delete, original_delete)
                self.assertIsNot(
                    threading.Thread._bootstrap_inner, original_bootstrap_inner
                )

            def new_worker():
                ready_new.set()
                self.assertTrue(release_new.wait(5))

            new = threading.Thread(target=new_worker)
            new.start()
            self.assertTrue(ready_new.wait(5))

            failures = []
            if sys.version_info >= (3, 11):
                threading.excepthook = lambda args: failures.append(args.exc_type)

                def raise_error():
                    raise ValueError("worker failed")

                raising = threading.Thread(target=raise_error)
                raising.start()
                raising.join(5)
                self.assertFalse(raising.is_alive())
                self.assertEqual(failures, [ValueError])

            release_new.set()
            new.join(5)
            self.assertFalse(new.is_alive())
            release.set()
            preexisting.join(5)
            self.assertFalse(preexisting.is_alive())

            for name in calls:
                registered = calls[name]["register"]
                unregistered = calls[name]["unregister"]
                self.assertEqual(
                    registered.count((preexisting.ident, preexisting.native_id)), 1
                )
                self.assertEqual(registered.count((new.ident, new.native_id)), 1)
                self.assertEqual(unregistered.count(new.ident), 1)
                if sys.version_info >= (3, 11):
                    self.assertEqual(unregistered.count(preexisting.ident), 1)
                    self.assertEqual(
                        registered.count((raising.ident, raising.native_id)), 1
                    )
                    self.assertEqual(unregistered.count(raising.ident), 1)
                else:
                    self.assertEqual(unregistered.count(preexisting.ident), 0)
        finally:
            release_new.set()
            release.set()
            preexisting.join(5)
            threading.excepthook = original_excepthook
            threading.Thread._set_native_id = original_set_native_id
            threading.Thread._bootstrap_inner = original_bootstrap_inner
            threading.Thread._delete = original_delete


if __name__ == "__main__":
    unittest.main()
