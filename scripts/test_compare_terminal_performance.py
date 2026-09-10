"""Failure-path checks that do not launch terminals or run performance loads."""

import json
from pathlib import Path
import runpy
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch


RUNNER = runpy.run_path(str(Path(__file__).with_name("compare-terminal-performance.py")))
EXECUTE = RUNNER["execute_sample"]


class SampleLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name) / "sample"
        self.args = SimpleNamespace(backend="x11", screen="alternate", settle_seconds=0, timeout=1)
        self.command = patch.dict(EXECUTE.__globals__, {
            "terminal_command": Mock(return_value=(["terminal"], {})),
            "stop_owned_child": Mock(),
        })
        self.command.start()
        self.addCleanup(self.command.stop)

    def execute(self):
        return EXECUTE("kokuban", Path("terminal"), "test", self.directory, {}, self.args)

    def test_case_write_failure_stops_terminal_and_owned_child(self):
        process = Mock(pid=123, returncode=None)
        process.poll.return_value = None
        real_record = RUNNER["record"]

        def fail_case(directory, name, value):
            if name == "case.json":
                raise OSError("injected case write failure")
            real_record(directory, name, value)

        with patch.object(RUNNER["subprocess"], "Popen", return_value=process), \
                patch.dict(EXECUTE.__globals__, {"record": fail_case}):
            sample = self.execute()
        process.terminate.assert_called_once_with()
        process.wait.assert_called_once_with(timeout=3)
        EXECUTE.__globals__["stop_owned_child"].assert_called_once_with(self.directory)
        self.assertEqual(sample["status"], "failed")
        self.assertIn("injected case write failure", sample["error"])
        self.assertEqual(json.loads((self.directory / "sample.json").read_text()), sample)

    def test_launch_failure_is_recorded_as_failed_sample(self):
        with patch.object(RUNNER["subprocess"], "Popen", side_effect=OSError("injected launch failure")):
            sample = self.execute()
        self.assertEqual(sample["status"], "failed")
        self.assertIn("injected launch failure", sample["error"])
        self.assertEqual(json.loads((self.directory / "sample.json").read_text()), sample)


if __name__ == "__main__":
    unittest.main()
