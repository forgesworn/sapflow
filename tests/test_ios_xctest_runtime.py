"""Selected-Xcode runtime discovery; generated directories, no native tools."""
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "mobile/ios"))
spec = importlib.util.spec_from_file_location(
    "checkpoint_sdk_tests", ROOT / "mobile/ios/test-recording-checkpoints-sdk.py")
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class XCTestRuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="xctest-runtime-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.contents = self.root / "Xcode_16.4.app/Contents"
        self.sdk = self.contents / "Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX15.5.sdk"
        self.sdk.mkdir(parents=True)

    def test_uses_shared_runtime_from_selected_versioned_xcode(self):
        selected = self.contents / "SharedFrameworks"
        (selected / "XCTestCore.framework").mkdir(parents=True)
        (self.root / "Xcode.app/Contents/SharedFrameworks").mkdir(parents=True)
        self.assertEqual(driver.xcode_shared_frameworks(self.sdk), selected)

    def test_older_xcode_without_shared_runtime_keeps_existing_paths(self):
        self.assertIsNone(driver.xcode_shared_frameworks(self.sdk))

    def test_does_not_borrow_runtime_outside_selected_xcode(self):
        outer = self.root / "Contents"
        (outer / "SharedFrameworks").mkdir(parents=True)
        inner_sdk = outer / "Xcode.app/Contents/Developer/SDKs/MacOSX.sdk"
        inner_sdk.mkdir(parents=True)
        self.assertIsNone(driver.xcode_shared_frameworks(inner_sdk))


if __name__ == "__main__":
    unittest.main()
