#!/usr/bin/env python3
"""Reject a simulator report without native media, decoded playback and cleanup."""
import argparse
import json
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('report', type=Path)
args = parser.parse_args()
report = json.loads(args.report.read_text())
viewer = report.get('viewerBest', report.get('viewer', {}))
checks = {
    'finished': report.get('state') == 'finished',
    'no_native_error': not report.get('error') and not report.get('finalisationError'),
    'generated_source': report.get('softwareVideo') is True and report.get('source') == 'generated picture and PCM',
    'ordinary_hardware_guard': report.get('ordinaryHardwareGuard') in [
        'Ordinary initialiser rejected absent hardware', 'Hardware selected by ordinary initialiser'],
    'native_video': report.get('videoUnits', 0) >= 300,
    'native_audio': report.get('audioUnits', 0) >= 300,
    'keyframe_interval': report.get('keyframes', 0) >= 10 and 0 < report.get('maxKeyframeSeconds', 0) <= 1.1,
    'decoded_video': viewer.get('frames', 0) >= 60 and viewer.get('width') == 1280 and viewer.get('height') == 720,
    'playback_progress': viewer.get('time', 0) >= 5 and viewer.get('error', 0) == 0,
    'audio_track': viewer.get('audioTracks') == 1,
    'transport_cleanup': report.get('publisherCleanupComplete') is True,
}
print(json.dumps({'passed': all(checks.values()), 'checks': checks, 'viewer': viewer}, indent=2))
raise SystemExit(0 if all(checks.values()) else 1)
