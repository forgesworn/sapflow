"""Public native matrix refuses incomplete evidence before starting an oracle."""
import copy
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "mobile/ios"))
spec = importlib.util.spec_from_file_location(
    "capture_recording_gate", ROOT / "mobile/ios/check-capture-recording.py")
driver = importlib.util.module_from_spec(spec)
with patch("subprocess.Popen", side_effect=AssertionError("Import launched a process")):
    spec.loader.exec_module(driver)


def complete_matrix():
    stored = ["healthy", "mixed-zero-raw", "publishing-independent-failure",
              "missing-file-EOF", "recording-queue-failure"]
    sdk = ["producer-audio", "producer-av", "producer-publishing-failure",
           "producer-late-native-drop", "producer-recording-loss", "producer-input-not-closed",
           "producer-wrong-worker", "producer-late-encoded-output", "producer-late-timing",
           "producer-wrong-ticket"]
    legacy = ["producer-audio", "producer-av", "producer-recording-loss",
              "producer-late-native-drop", "producer-input-not-closed",
              "producer-wrong-worker", "producer-wrong-ticket"]

    def producer(name, backend, finalised, video):
        return dict(name=name, passed=True, backend=backend, finalised=finalised,
                    normalStopCancellationPresent=True, producerCapabilitySingleUse=True,
                    duplicateFinishPreservedBytes=True, recordingClosedBeforeTransport=True,
                    hasVideo=video, file="Sapflow-generated.mov" if video else "Sapflow-generated.m4a")

    return dict(passed=True, generatedOnly=True,
                cases=[dict(name=name, passed=True, sdkReceiptState="incomplete",
                            appAdmission="unknown", actualCheckedCleanup=True,
                            recordingClosedBeforeTransport=True, validatedExactlyOnce=True,
                            exactPayloadProbeRequired=index < 4, media="Sapflow-generated.m4a",
                            originalPackets=[dict(size=8, sha256="a" * 64)])
                       for index, name in enumerate(stored)],
                producerCases=[producer(name, "sdkCheckpointed", index < 3, index == 1)
                               for index, name in enumerate(sdk)],
                legacyProducerCases=[producer(name, "legacy", index < 2, index in (1, 2, 3))
                                     for index, name in enumerate(legacy)])


class NativeMatrixValidationTests(unittest.TestCase):
    def setUp(self):
        self.matrix = complete_matrix()

    def reject(self):
        with self.assertRaises((RuntimeError, KeyError, TypeError, ValueError)):
            driver.validate_matrix(self.matrix)

    def test_accepts_complete_generated_twenty_two_case_matrix(self):
        driver.validate_matrix(self.matrix)

    def test_refuses_old_fifteen_case_result(self):
        del self.matrix["legacyProducerCases"]
        self.reject()

    def test_refuses_missing_legacy_negative(self):
        self.matrix["legacyProducerCases"].pop()
        self.reject()

    def test_refuses_duplicate_legacy_case_with_same_total(self):
        self.matrix["legacyProducerCases"][-1] = copy.deepcopy(self.matrix["legacyProducerCases"][0])
        self.reject()

    def test_refuses_reordered_sdk_cases(self):
        self.matrix["producerCases"][0:2] = reversed(self.matrix["producerCases"][0:2])
        self.reject()

    def test_refuses_failed_legacy_case_despite_top_level_success(self):
        self.matrix["legacyProducerCases"][2]["passed"] = False
        self.reject()

    def test_refuses_promoted_incomplete_legacy_recording(self):
        self.matrix["legacyProducerCases"][2]["finalised"] = True
        self.reject()

    def test_refuses_sdk_case_reported_as_legacy(self):
        self.matrix["producerCases"][0]["backend"] = "legacy"
        self.reject()

    def test_refuses_unconfirmed_cleanup(self):
        self.matrix["cases"][0]["actualCheckedCleanup"] = False
        self.reject()

    def test_refuses_lost_single_use_producer_fact(self):
        self.matrix["legacyProducerCases"][0]["producerCapabilitySingleUse"] = False
        self.reject()

    def test_refuses_duplicate_finish_that_changed_file(self):
        self.matrix["producerCases"][0]["duplicateFinishPreservedBytes"] = False
        self.reject()

    def test_refuses_truthy_integer_as_boolean_evidence(self):
        self.matrix["legacyProducerCases"][0]["passed"] = 1
        self.reject()

    def test_refuses_unprobed_stored_control(self):
        self.matrix["cases"][0]["exactPayloadProbeRequired"] = False
        self.reject()

    def test_refuses_non_generated_result(self):
        self.matrix["generatedOnly"] = False
        self.reject()

    def test_refuses_path_escape_before_oracle(self):
        self.matrix["producerCases"][0]["file"] = "../other.m4a"
        self.reject()

    def test_refuses_absolute_media_path(self):
        self.matrix["cases"][0]["media"] = "/private/other.m4a"
        self.reject()

    def test_refuses_symlink_media(self):
        with tempfile.TemporaryDirectory(prefix="native-matrix-") as temporary:
            base = Path(temporary)
            folder = base / "producer-audio"
            folder.mkdir()
            original = base / "outside.m4a"
            original.write_bytes(b"generated fixture")
            (folder / "alias.m4a").symlink_to(original)
            with self.assertRaises((RuntimeError, ValueError)):
                driver.fixture_media(base, "producer-audio", "alias.m4a")


class NativeCommandValidationTests(unittest.TestCase):
    def setUp(self):
        self.commands = [dict(log=name, exit=0, groupStopped=True, error=None)
                         for name in driver.COMMAND_LOGS]

    def test_current_direct_binding_build_and_all_media_oracles_pass(self):
        self.assertEqual(len(self.commands), 19)
        driver.validate_commands(self.commands)

    def test_each_missing_stage_is_refused(self):
        for index in range(len(self.commands)):
            with self.subTest(stage=index), self.assertRaises(RuntimeError):
                driver.validate_commands(self.commands[:index] + self.commands[index + 1:])

    def test_duplicate_or_reordered_stage_is_refused(self):
        for commands in (self.commands + [self.commands[-1]], list(reversed(self.commands))):
            with self.assertRaises(RuntimeError):
                driver.validate_commands(commands)

    def test_failed_or_unclosed_stage_is_refused(self):
        for name, value in [('exit', 1), ('groupStopped', False), ('error', 'failed')]:
            commands = copy.deepcopy(self.commands)
            commands[2][name] = value
            with self.subTest(field=name), self.assertRaises(RuntimeError):
                driver.validate_commands(commands)

    def test_payload_and_producer_checkpoints_require_the_exact_prefix(self):
        for stage, count in [('missing-file-EOF-packets.local.json', 9),
                             ('producer-publishing-failure-decode.local.log', 15)]:
            driver.validate_commands(self.commands[:count], stage)
            with self.assertRaises(RuntimeError):
                driver.validate_commands(self.commands[:count - 1], stage)


if __name__ == "__main__":
    unittest.main()
