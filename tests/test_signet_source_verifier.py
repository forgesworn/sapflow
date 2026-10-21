"""Generated-only source packaging faults; no dependency build or network."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("verify_signet_sources", ROOT / "scripts/verify-signet-sources.py")
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)


class SourceVerifierTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="signet-verifier-", dir=ROOT / "target")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.records = []
        (self.root / "vendor/patches").mkdir(parents=True)
        for name in sorted(verifier.NAMES):
            directory = self.root / "vendor" / name
            directory.mkdir()
            (directory / "generated.rs").write_bytes(b"// generated fixture\n")
            patch = "vendor/patches/" + name + ".patch"
            (self.root / patch).write_bytes(b"generated patch\n")
            self.records.append(dict(name=name, files={"generated.rs": hashlib.sha256(b"// generated fixture\n").hexdigest()}, patch=patch, patchSHA256=hashlib.sha256(b"generated patch\n").hexdigest()))
        self.write()

    def write(self):
        (self.root / "vendor/signet-auth-sources.json").write_text(json.dumps(dict(version=1, sources=self.records)))

    def test_exact_source_and_patch_inventory_passes(self):
        verifier.verify(self.root)

    def test_changed_or_extra_source_refused(self):
        path = self.root / "vendor/nostr-sdk/generated.rs"
        path.write_bytes(b"changed")
        with self.assertRaises(ValueError):
            verifier.verify(self.root)
        path.write_bytes(b"// generated fixture\n")
        (path.parent / "extra.rs").write_bytes(b"extra")
        with self.assertRaises(ValueError):
            verifier.verify(self.root)

    def test_symlink_never_followed(self):
        (self.root / "vendor/nostr-sdk/link.rs").symlink_to(self.root / "vendor/nostr-connect/generated.rs")
        with self.assertRaises(ValueError):
            verifier.verify(self.root)

    def test_patch_drift_or_arbitrary_patch_path_refused(self):
        path = self.root / self.records[0]["patch"]
        path.write_bytes(b"changed")
        with self.assertRaises(ValueError):
            verifier.verify(self.root)
        path.write_bytes(b"generated patch\n")
        self.records[0]["patch"] = "../unrelated"
        self.write()
        with self.assertRaises(ValueError):
            verifier.verify(self.root)

    def test_duplicate_or_missing_snapshot_refused(self):
        self.records[0] = self.records[1]
        self.write()
        with self.assertRaises(ValueError):
            verifier.verify(self.root)


if __name__ == "__main__":
    unittest.main()
