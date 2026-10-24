#!/usr/bin/env python3
"""Verify one built app's required-reason resource, not App Store compliance.

Only the explicit --app-root/PrivacyInfo.xcprivacy is read. Collection,
tracking and SDK-wide disclosures need their own assessment; this checkpoint
neither invents those declarations nor establishes that no data is collected.
"""

import argparse
import json
import os
from pathlib import Path
import plistlib
import stat
import sys
from xml.parsers.expat import ExpatError


RESOURCE = "PrivacyInfo.xcprivacy"
MAX_RESOURCE_BYTES = 16 * 1024
REQUIRED_REASONS = {
    "NSPrivacyAccessedAPICategoryFileTimestamp": frozenset({"C617.1", "3B52.1"}),
    "NSPrivacyAccessedAPICategoryDiskSpace": frozenset({"E174.1"}),
    "NSPrivacyAccessedAPICategorySystemBootTime": frozenset({"35F9.1"}),
}


class PrivacyResourceError(ValueError):
    """The resource is missing, unreadable or outside this frozen profile."""


class _UniqueDictionary(dict):
    def __setitem__(self, key, value):
        if key in self:
            raise ValueError("Duplicate property-list key")
        super().__setitem__(key, value)


def validate_manifest(value):
    if not isinstance(value, dict) or set(value) != {"NSPrivacyAccessedAPITypes"}:
        raise PrivacyResourceError("Expected only the required-reason root key; other disclosures are outside this checkpoint")
    entries = value["NSPrivacyAccessedAPITypes"]
    if not isinstance(entries, list) or len(entries) != len(REQUIRED_REASONS):
        raise PrivacyResourceError("Expected exactly three required-reason categories")
    seen = set()
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"NSPrivacyAccessedAPIType", "NSPrivacyAccessedAPITypeReasons"}:
            raise PrivacyResourceError("Invalid required-reason entry keys")
        category = entry["NSPrivacyAccessedAPIType"]
        if not isinstance(category, str) or category not in REQUIRED_REASONS or category in seen:
            raise PrivacyResourceError("Unsupported or duplicate required-reason category")
        seen.add(category)
        reasons = entry["NSPrivacyAccessedAPITypeReasons"]
        if not isinstance(reasons, list) or not all(isinstance(reason, str) for reason in reasons):
            raise PrivacyResourceError("Reasons must be an array of strings")
        expected = REQUIRED_REASONS[category]
        if len(reasons) != len(expected) or set(reasons) != expected:
            raise PrivacyResourceError("Required-reason set differs from the reviewed profile")
    if seen != set(REQUIRED_REASONS):
        raise PrivacyResourceError("Missing required-reason category")


def verify_app_resource(app_root):
    root = Path(app_root)
    if not root.is_dir():
        raise PrivacyResourceError("App root is not a directory")
    try:
        # NONBLOCK also prevents a substituted FIFO from blocking before fstat.
        flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
        descriptor = os.open(root / RESOURCE, flags)
        with os.fdopen(descriptor, "rb") as source:
            before = os.fstat(source.fileno())
            if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= MAX_RESOURCE_BYTES:
                raise PrivacyResourceError("Privacy resource must be a bounded regular file")
            data = source.read(MAX_RESOURCE_BYTES + 1)
            after = os.fstat(source.fileno())
        identity = lambda item: (item.st_dev, item.st_ino, item.st_size, item.st_mtime_ns)
        if len(data) != before.st_size or identity(before) != identity(after):
            raise PrivacyResourceError("Privacy resource changed while being read")
        value = plistlib.loads(data, dict_type=_UniqueDictionary)
    except PrivacyResourceError:
        raise
    except (OSError, ValueError, TypeError, OverflowError, RecursionError, ExpatError, plistlib.InvalidFileException) as error:
        # Do not echo resource contents, arbitrary paths or parser diagnostics.
        raise PrivacyResourceError("Missing, unreadable or malformed privacy resource") from error
    validate_manifest(value)
    return {
        "privacyResourceVerified": True,
        "resource": RESOURCE,
        "requiredReasons": {category: sorted(reasons) for category, reasons in sorted(REQUIRED_REASONS.items())},
        "scope": "Required-reason resource checkpoint only",
        "collectionAndTrackingAssessment": "Not assessed by this verifier",
        "appStoreReadinessEstablished": False,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app-root", required=True, type=Path, help="Exact built .app directory to inspect")
    args = parser.parse_args()
    try:
        result = verify_app_resource(args.app_root)
    except PrivacyResourceError as error:
        print(str(error), file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
