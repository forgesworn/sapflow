#!/usr/bin/env python3
"""Reject a simulator report without native media, decoded playback and cleanup."""
import argparse
import json
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('report', type=Path)
parser.add_argument('--require-recovery', action='store_true', help='Require decoded playback in all three publisher runs')
args = parser.parse_args()
report = json.loads(args.report.read_text())
viewer = report.get('viewerBest', report.get('viewer', {}))
def presented_count(sample):
    if sample.get('frameCallbacksSupported') is True:
        return sample.get('presentedCallbacks', 0)
    return sample.get('frames', 0) - sample.get('dropped', 0)


presented_frames = presented_count(viewer)
checks = {
    'finished': report.get('state') == 'finished',
    'no_native_error': not report.get('error') and not report.get('finalisationError'),
    'generated_source': report.get('softwareVideo') is True and report.get('source') == 'generated picture and PCM',
    'ordinary_hardware_guard': report.get('ordinaryHardwareGuard') in [
        'Ordinary initialiser rejected absent hardware', 'Hardware selected by ordinary initialiser'],
    'native_video': report.get('videoUnits', 0) >= 300,
    'native_audio': report.get('audioUnits', 0) >= 300,
    'keyframe_interval': report.get('keyframes', 0) >= 10 and 0 < report.get('maxKeyframeSeconds', 0) <= 1.1,
    'decoded_video': presented_frames >= 60 and viewer.get('width') == 1280 and viewer.get('height') == 720,
    'playback_progress': viewer.get('time', 0) >= 5 and viewer.get('error', 0) == 0,
    'audio_track': viewer.get('audioTracks') == 1,
    'transport_cleanup': report.get('publisherCleanupComplete') is True,
}
recovery = []
if args.require_recovery or report.get('recoveryCheck'):
    checks['recovery_workload'] = report.get('recoveryCheck') is True and report.get('run') == 3
    for run in range(1, 4):
        samples = [sample for sample in report.get('recoveryReadings', [])
                   if sample.get('run') == run and sample.get('state') == 'broadcasting']
        presented = 0
        progress = 0
        advancing = []
        for previous, current in zip(samples, samples[1:]):
            frames = presented_count(current) - presented_count(previous)
            position_key = 'callbackMediaTime' if current.get('frameCallbacksSupported') is True else 'time'
            elapsed = (current.get(position_key, current.get('time', 0))
                       - previous.get(position_key, previous.get('time', 0)))
            counters_valid = all(
                sample.get('presentedCallbacks', 0) >= 0 if sample.get('frameCallbacksSupported') is True
                else sample.get('frames', 0) >= sample.get('dropped', 0) >= 0
                for sample in [previous, current])
            # A source reload or seek is not evidence of decoded progress.
            if (frames > 0 and 0 < elapsed <= 3 and current.get('error', 0) == 0
                    and counters_valid
                    and current.get('audioTracks') == 1 and current.get('ready', 0) >= 3
                    and current.get('width') == 1280 and current.get('height') == 720):
                presented += frames
                progress += elapsed
                advancing.append(current.get('at', 0))
        # Playback must still advance near the end, not only during startup.
        recent = bool(samples and advancing and samples[-1].get('at', 0) - advancing[-1] <= 3)
        passed = presented >= 120 and progress >= 5 and recent
        checks[f'run_{run}_playback'] = passed
        recovery.append({'run': run, 'presentedFrames': presented, 'progressSeconds': progress,
                         'advancingAtEnd': recent, 'passed': passed})
print(json.dumps({'passed': all(checks.values()), 'checks': checks, 'viewer': viewer,
                  'presentedFrames': presented_frames, 'recovery': recovery}, indent=2))
raise SystemExit(0 if all(checks.values()) else 1)
