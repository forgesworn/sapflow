#!/usr/bin/env python3
"""Dependency-free single-input fuzz entry. Reads at most 128 KiB plus one byte.

Feed a bounded file on stdin; success and rejected input both exit zero. An
unexpected exception is a finding. This never starts a publisher or network IO.
"""
import importlib.util
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location(
    "epoch_harness", Path(__file__).with_name("origin-epoch-integration.py"))
harness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(harness)


def exercise(data):
    try:
        parsed = harness.parse_playlist(data)
        history = harness.History()
        history.accept(parsed)
        history.accept(parsed)
        order = harness.EpochRetention()
        for segment in parsed["segments"]:
            if not segment["gap"]:
                order.observe(segment)
    except harness.Failure:
        pass


if __name__ == "__main__":
    exercise(sys.stdin.buffer.read(harness.MAX_PLAYLIST+1))
