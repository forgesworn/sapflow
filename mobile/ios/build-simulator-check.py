#!/usr/bin/env python3
"""Build the separate generated-media simulator app using existing Apple SDKs."""
import argparse
import json
import os
import plistlib
from pathlib import Path
import platform
import shutil
import signal
import subprocess

root = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--sdk-root', type=Path, default=root, help='Checkout containing target/native-packages/apple and signet-apple')
parser.add_argument('--output', type=Path, default=root / 'target/simulator-broadcast-check')
args = parser.parse_args()
output = args.output.resolve()
marker = output / '.sapflow-simulator-check'
if output.exists() and not marker.is_file():
    parser.error('Output already exists without this check\'s ownership marker')
for command in ['xcodegen', 'xcodebuild']:
    if not shutil.which(command):
        parser.error('Existing ' + command + ' is required')
packages = args.sdk_root.resolve() / 'target/native-packages'
for package in ['apple', 'signet-apple']:
    if not (packages / package / 'Package.swift').is_file():
        parser.error('Existing packaged Apple SDK missing: ' + str(packages / package))
output.mkdir(parents=True, exist_ok=True)
marker.touch()
plist = {
    'CFBundleExecutable': '$(EXECUTABLE_NAME)',
    'CFBundleIdentifier': '$(PRODUCT_BUNDLE_IDENTIFIER)',
    'CFBundleName': '$(PRODUCT_NAME)',
    'CFBundlePackageType': 'APPL',
    'CFBundleVersion': '$(CURRENT_PROJECT_VERSION)',
    'CFBundleShortVersionString': '$(MARKETING_VERSION)',
    'UILaunchScreen': {},
    'NSAppTransportSecurity': {'NSAllowsArbitraryLoadsInWebContent': True},
    'NSLocalNetworkUsageDescription': 'This simulator check sends generated media to its local test receiver.',
}
(output / 'Info.plist').write_bytes(plistlib.dumps(plist))
project = {
    'name': 'SapflowSimulatorCheck',
    'options': {'deploymentTarget': {'iOS': '16.0'}},
    'packages': {'Sapflow': {'path': str(packages / 'apple')}, 'SignetAuth': {'path': str(packages / 'signet-apple')}},
    'targets': {'SapflowSimulatorCheck': {
        'type': 'application', 'platform': 'iOS',
        'sources': [
            {'path': str(root / 'mobile/ios/Sources'), 'excludes': ['SapflowBroadcasterApp.swift']},
            {'path': str(root / 'mobile/ios/Tools/SimulatorBroadcastCheck.swift')},
        ],
        'dependencies': [{'package': 'Sapflow'}, {'package': 'SignetAuth'}],
        'settings': {'base': {
            'SWIFT_VERSION': '5.0', 'CURRENT_PROJECT_VERSION': '1', 'MARKETING_VERSION': '0.1.0',
            'CODE_SIGNING_ALLOWED': 'NO', 'PRODUCT_BUNDLE_IDENTIFIER': 'dev.forgesworn.sapflow.simulatorcheck',
            'GENERATE_INFOPLIST_FILE': 'NO', 'INFOPLIST_FILE': str(output / 'Info.plist'),
            'SWIFT_ACTIVE_COMPILATION_CONDITIONS': '$(inherited) SAPFLOW_SIMULATOR_TEST_SOURCE',
            'TARGETED_DEVICE_FAMILY': '1', 'SAPFLOW_V4V_ENABLED': 'NO',
            'ARCHS': platform.machine(), 'ONLY_ACTIVE_ARCH': 'YES',
            'SAPFLOW_V4V_API_ORIGIN': '', 'SAPFLOW_V4V_PLAYBACK_ORIGIN': '', 'SAPFLOW_V4V_PLAYBACK_PREFIX': '',
        }},
    }},
    'schemes': {'SapflowSimulatorCheck': {'build': {'targets': {'SapflowSimulatorCheck': 'all'}}}},
}
(output / 'project.json').write_text(json.dumps(project, indent=2) + '\n')


def invoke(command, name, timeout):
    log = output / name
    with log.open('wb') as stream:
        process = subprocess.Popen(command, stdout=stream, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = process.wait(timeout=timeout)
        except BaseException:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
            raise
    if code:
        errors = [line for line in log.read_text(errors='replace').splitlines() if 'error:' in line or 'FAILED' in line]
        raise SystemExit('\n'.join(errors[-10:]) + '\nBuild failed; inspect ' + str(log))


invoke(['xcodegen', '--spec', str(output / 'project.json'), '--project', str(output),
        '--cache-path', str(output / 'xcodegen-cache')], 'generate.local.log', 30)
invoke(['xcodebuild', '-project', str(output / 'SapflowSimulatorCheck.xcodeproj'), '-scheme', 'SapflowSimulatorCheck',
        '-sdk', 'iphonesimulator', '-configuration', 'Debug', '-destination', 'generic/platform=iOS Simulator',
        '-derivedDataPath', str(output / 'DerivedData'), '-jobs', '2', 'CODE_SIGNING_ALLOWED=NO', 'build'], 'build.local.log', 300)
print(output / 'DerivedData/Build/Products/Debug-iphonesimulator/SapflowSimulatorCheck.app')
