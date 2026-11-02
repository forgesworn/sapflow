"""Contract checks for the generated-source simulator report CLI."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'mobile/ios/check-simulator-report.py'


class SimulatorReportTests(unittest.TestCase):
    def report(self):
        return {
            'state': 'finished', 'softwareVideo': True, 'source': 'generated picture and PCM',
            'ordinaryHardwareGuard': 'Ordinary initialiser rejected absent hardware',
            'videoUnits': 1800, 'audioUnits': 2815, 'keyframes': 60, 'maxKeyframeSeconds': 1,
            'publisherCleanupComplete': True,
            'viewer': {'frames': 1504, 'width': 1280, 'height': 720, 'time': 58.8, 'error': 0, 'audioTracks': 1},
        }

    def check_report(self, report, passed, require_recovery=False):
        with tempfile.TemporaryDirectory(prefix='sapflow-simulator-report-') as directory:
            path = Path(directory) / 'report.json'
            path.write_text(json.dumps(report))
            result = subprocess.run([sys.executable, '-B', str(SCRIPT), str(path)]
                                    + (['--require-recovery'] if require_recovery else []),
                                    capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0 if passed else 1, result.stderr)
        summary = json.loads(result.stdout)
        self.assertEqual(summary['passed'], passed)
        return summary

    def test_completed_native_and_viewer_run(self):
        self.check_report(self.report(), True)

    def test_encoding_alone_does_not_prove_playback(self):
        report = self.report()
        report['viewer']['frames'] = 0
        self.assertFalse(self.check_report(report, False)['checks']['decoded_video'])

    def test_audio_track_is_required(self):
        report = self.report()
        report['viewer']['audioTracks'] = 0
        self.check_report(report, False)

    def test_dropped_frames_do_not_count_as_presented_video(self):
        report = self.report()
        report['viewer'].update(frames=100, dropped=100)
        self.assertFalse(self.check_report(report, False)['checks']['decoded_video'])

    def test_stop_must_complete_without_native_errors(self):
        for change in [{'state': 'stopping'}, {'publisherCleanupComplete': False},
                       {'error': 'Codec failed'}, {'finalisationError': 'Native EOF failed'}]:
            with self.subTest(change=change):
                report = self.report(); report.update(change)
                self.check_report(report, False)

    def test_software_selection_does_not_replace_hardware_guard(self):
        report = self.report(); del report['ordinaryHardwareGuard']
        self.check_report(report, False)

    def test_long_gop_is_rejected(self):
        report = self.report(); report['maxKeyframeSeconds'] = 2
        self.check_report(report, False)

    def test_wrong_picture_or_absent_progress_is_rejected(self):
        for change in [{'width': 0}, {'height': 0}, {'time': 0}, {'error': 3}]:
            with self.subTest(change=change):
                report = self.report(); report['viewer'].update(change)
                self.check_report(report, False)

    def test_peak_decode_survives_player_counter_reset(self):
        report = self.report(); report['viewerBest'] = copy.deepcopy(report['viewer'])
        report['viewer'].update(frames=0, width=0, height=0)
        self.check_report(report, True)
        self.assertEqual(report['viewer']['frames'], 0)  # Preserve the current recovery observation too.

    def recovery_report(self):
        report = self.report()
        report.update(recoveryCheck=True, run=3)
        report['recoveryReadings'] = [
            dict(report['viewer'], run=run, state='broadcasting', ready=4,
                 frames=i * 30, time=i, at=run * 25 + i)
            for run in range(1, 4) for i in range(12)
        ]
        return report

    def test_each_restart_must_decode_fresh_frames(self):
        self.check_report(self.report(), False, require_recovery=True)
        self.check_report(self.recovery_report(), True)
        for run in [2, 3]:
            report = self.recovery_report()
            for sample in report['recoveryReadings']:
                if sample['run'] == run: sample['frames'] = 330
            self.assertFalse(self.check_report(report, False)['checks'][f'run_{run}_playback'])

    def test_early_playback_cannot_hide_a_late_stall(self):
        report = self.recovery_report()
        for sample in report['recoveryReadings']:
            if sample['run'] == 3 and sample['time'] >= 7:
                sample.update(frames=180, time=6)
        self.check_report(report, False)

    def test_recovery_rejects_drops_missing_audio_and_skipped_runs(self):
        for change in [{'dropped': 330}, {'audioTracks': 0}, {'error': 3}]:
            report = self.recovery_report()
            for sample in report['recoveryReadings']:
                if sample['run'] == 2:
                    sample.update(change)
                    if 'dropped' in change: sample['dropped'] = sample['frames']
            self.check_report(report, False)
        report = self.recovery_report()
        report['recoveryReadings'] = [s for s in report['recoveryReadings'] if s['run'] != 2]
        self.check_report(report, False)

    def test_actual_presentation_callbacks_handle_webkit_quality_counter_sticking(self):
        report = self.recovery_report()
        report['viewer'].update(frameCallbacksSupported=True, presentedCallbacks=1504, frames=0)
        for sample in report['recoveryReadings']:
            sample.update(frameCallbacksSupported=True, presentedCallbacks=sample['frames'], frames=0)
        self.check_report(report, True)
        # A moving media clock and a historical quality counter cannot hide a frozen picture.
        for sample in report['recoveryReadings']:
            if sample['run'] == 3: sample.update(presentedCallbacks=330, frames=sample['time'] * 30)
        self.check_report(report, False)


if __name__ == '__main__':
    unittest.main()
