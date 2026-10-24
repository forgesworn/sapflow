#!/usr/bin/env python3
"""Check reviewed cold-auth snapshots. Never rewrite a manifest during a build."""
import hashlib
import json
from pathlib import Path

NAMES = {"signet-nip46-client", "nostr-connect", "nostr-sdk", "async-wsocket"}


def verify(root: Path) -> None:
    manifest = json.loads((root / "vendor/signet-auth-sources.json").read_text())
    sources = manifest.get("sources", [])
    if manifest.get("version") != 1 or len(sources) != len(NAMES) or {s["name"] for s in sources} != NAMES:
        raise ValueError("unsupported Signet source manifest")
    for source in sources:
        directory = root / "vendor" / source["name"]
        actual = {}
        for path in sorted(directory.rglob("*")):
            if path.is_symlink():
                raise ValueError("Signet source symlink refused")
            if path.is_file():
                actual[str(path.relative_to(directory))] = hashlib.sha256(path.read_bytes()).hexdigest()
        if actual != source["files"]:
            raise ValueError("Signet source snapshot mismatch: " + source["name"])
        expected_patch = "vendor/patches/" + source["name"] + ".patch"
        if source["patch"] != expected_patch:
            raise ValueError("Signet source patch path refused")
        if hashlib.sha256((root / expected_patch).read_bytes()).hexdigest() != source["patchSHA256"]:
            raise ValueError("Signet source patch mismatch: " + source["name"])


if __name__ == "__main__":
    verify(Path(__file__).resolve().parent.parent)
    print("cold-auth exact source snapshots PASS")
