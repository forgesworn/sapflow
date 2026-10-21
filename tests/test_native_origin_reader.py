#!/usr/bin/env python3
"""Process ownership and playlist-selection regressions; no media or devices."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


harness = load("native_reader", ROOT / "tests/native-origin-reader.py")
epoch = load("epoch_reader", ROOT / "tests/origin-epoch-integration.py")


class Child:
    def __init__(self, failure=None):
        self.failure = failure
        self.stopped = False
        self.calls = []

    def poll(self):
        return 0 if self.stopped else None

    def terminate(self):
        self.calls.append("terminate")
        if self.failure == "race":
            self.stopped = True
            raise ProcessLookupError()
        if self.failure == "terminate":
            raise OSError("simulated terminate failure")
        if self.failure != "wait":
            self.stopped = True

    def wait(self, timeout):
        self.calls.append("wait")
        if not self.stopped:
            raise subprocess.TimeoutExpired("owned-fake-child", timeout)
        return 0

    def kill(self):
        self.calls.append("kill")
        self.stopped = True


class NativeReaderHarnessTests(unittest.TestCase):
    def test_absolute_deadline_is_not_a_retryable_poll_error(self):
        caught = False
        with self.assertRaises(harness.HarnessDeadline):
            try:
                raise harness.HarnessDeadline()
            except Exception:
                caught = True
        self.assertFalse(caught)

    def test_independence_requires_exact_attribute(self):
        prefix = '#EXT-X-PART:DURATION=0.200,URI="abc_part0.mp4",'
        for attributes in ["INDEPENDENT=YESNO", "XINDEPENDENT=YES", "INDEPENDENT=yes"]:
            self.assertEqual(harness.independent_part_uris(epoch, (prefix + attributes).encode()), set())
        self.assertEqual(harness.independent_part_uris(epoch, (prefix + "INDEPENDENT=YES").encode()), {"abc_part0.mp4"})

    def test_cleanup_failure_still_stops_other_child_and_removes_scratch(self):
        with tempfile.TemporaryDirectory(prefix="native-reader-unit-") as parent:
            scratch = Path(parent) / "own"
            scratch.mkdir()
            failed, other = Child("terminate"), Child()
            result = harness.cleanup_owned([failed, other], scratch)
            self.assertTrue(result["ownedChildrenStopped"])
            self.assertTrue(result["ownSyntheticScratchRemoved"])
            self.assertTrue(result["cleanupErrors"])
            self.assertIn("kill", failed.calls)
            self.assertIn("terminate", other.calls)

    def test_terminate_race_and_wait_timeout_release_all_owners(self):
        with tempfile.TemporaryDirectory(prefix="native-reader-unit-") as parent:
            scratch = Path(parent) / "own"
            scratch.mkdir()
            race, slow = Child("race"), Child("wait")
            result = harness.cleanup_owned([race, slow], scratch)
            self.assertTrue(result["ownedChildrenStopped"])
            self.assertTrue(result["ownSyntheticScratchRemoved"])
            self.assertEqual(result["cleanupErrors"], [])
            self.assertIn("kill", slow.calls)

    def test_scratch_removal_failure_is_reported_after_children_stop(self):
        with tempfile.TemporaryDirectory(prefix="native-reader-unit-") as parent:
            scratch = Path(parent) / "own"
            scratch.mkdir()
            child = Child()
            with patch.object(harness.shutil, "rmtree", side_effect=OSError("simulated removal failure")):
                result = harness.cleanup_owned([child], scratch)
            self.assertTrue(result["ownedChildrenStopped"])
            self.assertFalse(result["ownSyntheticScratchRemoved"])
            self.assertTrue(result["cleanupErrors"])


if __name__ == "__main__":
    unittest.main()
