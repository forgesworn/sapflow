"""Fault injection for actual packager control flow, not native qualification.

The real host-consumer gate runs separately. Here fake external tools are scoped
inside a generated temporary repository; no Cargo, native build or network runs.
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
FAKE = r'''
import os, pathlib, shutil, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
if name == 'cargo' and os.environ.get('FAIL_TOOL') == 'cargo': sys.exit(19)
if name == 'sapflow-signet-bindgen':
    dest = pathlib.Path(args[args.index('--out-dir')+1]); dest.mkdir(exist_ok=True)
    (dest/'SignetAuth.swift').write_text('generated fault fixture')
    (dest/'SignetAuthFFI.h').write_text('int generated_signet_fixture(void);')
    (dest/'SignetAuthFFI.modulemap').write_text('module SignetAuthFFI { header \"SignetAuthFFI.h\" export * }')
if name == 'xcrun': print('/generated/sdk')
if name == 'xcodebuild':
    dest = pathlib.Path(args[args.index('-output')+1]); dest.mkdir(parents=True)
    headers = pathlib.Path(args[args.index('-headers')+1])
    snapshot = pathlib.Path(sys.argv[0]).parent.parent/'target/header-snapshot'
    shutil.copytree(headers, snapshot)
if name == 'swiftc':
    dest = pathlib.Path(sys.argv[0]).parent.parent/'target/signet-auth-apple-headers-v2/ios-sdkroots.test.txt'
    with dest.open('a') as f: f.write(os.environ.get('SDKROOT', '<absent>')+'\n')
if name == 'swift':
    consumer = pathlib.Path(args[args.index('--package-path')+1])
    (consumer.parent.parent/'consumer-package.test.txt').write_text((consumer/'Package.swift').read_text())
if name == 'zip':
    pathlib.Path(args[2]).write_bytes(b'generated incomplete archive')
    if os.environ.get('FAIL_TOOL') == 'zip': sys.exit(23)
'''


class PackagingReceiptTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='signet-packaging-fault-', dir=ROOT/'target')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root/'scripts').mkdir()
        shutil.copyfile(ROOT/'scripts/build-signet-native.mjs', self.root/'scripts/build-signet-native.mjs')
        self.bin = self.root/'fake-bin'; self.bin.mkdir()
        for name in ['cargo','python3','xcrun','xcodebuild','swift','swiftc','zip']:
            p = self.bin/name; p.write_text('#!'+sys.executable+'\n'+FAKE); p.chmod(0o700)
        p = self.root/'target/debug/sapflow-signet-bindgen'; p.parent.mkdir(parents=True)
        p.write_text('#!'+sys.executable+'\n'+FAKE); p.chmod(0o700)
        for name in ['LICENSE','tests/native/SignetConsumer.swift','crates/sapflow-signet-ffi/THIRD_PARTY_LICENSES.txt']:
            p = self.root/name; p.parent.mkdir(parents=True, exist_ok=True); p.write_text('generated fixture')
        for name in ['V4vContracts','SrtDestination','V4vSessionEngine','V4vJournal','V4vControlClient','V4vLoginContracts','V4vSignetLoginOwner','V4vNativeSignet','OriginObservationPolicy','OriginSDKConfirmation']:
            p = self.root/'mobile/ios/Sources'/ (name+'.swift'); p.parent.mkdir(parents=True, exist_ok=True); p.write_text('generated fixture')

    def invoke(self, mode, fail=''):
        env = {**os.environ, 'PATH': str(self.bin)+os.pathsep+os.environ['PATH'], 'FAIL_TOOL': fail}
        env.pop('CARGO_TARGET_DIR', None)
        return subprocess.run(['node', str(self.root/'scripts/build-signet-native.mjs'), mode], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE).returncode

    def stale_receipt(self, mode):
        output = self.root/('target/signet-auth-apple-headers-v2' if mode=='apple' else 'target/signet-auth-host')
        output.mkdir(parents=True)
        (output/'result.local.json').write_text('{"terminalExit":0}')
        return output

    def test_preexisting_archive_refuses_before_staging_and_clears_stale_success(self):
        output = self.stale_receipt('apple')
        archive = self.root/'target/native-packages/signet-auth-apple.zip'; archive.parent.mkdir(); archive.write_bytes(b'preserve existing fixture')
        self.assertNotEqual(self.invoke('apple'), 0)
        self.assertFalse((output/'result.local.json').exists())
        self.assertFalse((output/'staging').exists())
        self.assertEqual(archive.read_bytes(), b'preserve existing fixture')

    def test_failed_native_tool_never_reuses_previous_receipt(self):
        output = self.stale_receipt('check')
        self.assertNotEqual(self.invoke('check', 'cargo'), 0)
        self.assertFalse((output/'result.local.json').exists())
        self.assertTrue((output/'staging').exists())

    def test_late_zip_failure_leaves_no_success_receipt_or_false_cleanup_claim(self):
        output = self.stale_receipt('apple')
        self.assertNotEqual(self.invoke('apple', 'zip'), 0)
        self.assertFalse((output/'result.local.json').exists())
        self.assertFalse((output/'archive.local.json').exists())
        self.assertTrue((output/'staging').exists())

    def test_apple_consumer_uses_inferred_signet_apple_path_identity(self):
        self.assertEqual(self.invoke('apple'), 0)
        manifest = (self.root/'target/signet-auth-apple-headers-v2/consumer-package.test.txt').read_text()
        self.assertIn('.product(name: "SignetAuth", package: "signet-apple")', manifest)
        self.assertIn('.product(name: "Sapflow", package: "apple")', manifest)

    def test_apple_swift_tools_receive_selected_iphone_sdkroot(self):
        self.assertEqual(self.invoke('apple'), 0)
        roots = (self.root/'target/signet-auth-apple-headers-v2/ios-sdkroots.test.txt').read_text().splitlines()
        self.assertEqual(roots, ['/generated/sdk'] * 4)

    def test_namespaced_map_keeps_module_and_relative_header_together(self):
        self.assertEqual(self.invoke('apple'), 0)
        headers = self.root/'target/header-snapshot'
        self.assertFalse((headers/'module.modulemap').exists())
        self.assertTrue((headers/'SignetAuthFFI/SignetAuthFFI.h').is_file())
        self.assertEqual((headers/'SignetAuthFFI/module.modulemap').read_text(), 'module SignetAuthFFI { header "SignetAuthFFI.h" export * }')

    def test_actual_script_headers_merge_without_overwriting_other_sdk_map(self):
        self.assertEqual(self.invoke('apple'), 0)
        combined = self.root/'target/combined-include'; combined.mkdir()
        other_map = 'module OtherGeneratedSDK { header "Other.h" export * }'
        (combined/'module.modulemap').write_text(other_map)
        (combined/'Other.h').write_text('int other_generated_fixture(void);')
        shutil.copytree(self.root/'target/header-snapshot', combined, dirs_exist_ok=True)
        self.assertEqual((combined/'module.modulemap').read_text(), other_map)
        self.assertTrue((combined/'SignetAuthFFI/module.modulemap').exists())
        self.assertTrue((combined/'SignetAuthFFI/SignetAuthFFI.h').exists())

    def test_new_stamp_cannot_reuse_historical_flat_layout_success(self):
        old = self.root/'target/signet-auth-apple'; old.mkdir(parents=True)
        (old/'result.local.json').write_text('historical generated receipt')
        self.assertNotEqual(self.invoke('apple', 'cargo'), 0)
        self.assertEqual((old/'result.local.json').read_text(), 'historical generated receipt')
        self.assertFalse((self.root/'target/signet-auth-apple-headers-v2/result.local.json').exists())

    def test_host_consumer_preserves_existing_signet_auth_path_identity(self):
        self.assertEqual(self.invoke('check'), 0)
        manifest = (self.root/'target/signet-auth-host/consumer-package.test.txt').read_text()
        self.assertIn('.product(name: "SignetAuth", package: "SignetAuth")', manifest)

    def test_simulated_tool_success_reports_only_after_real_staging_removal(self):
        output = self.stale_receipt('apple')
        self.assertEqual(self.invoke('apple'), 0)
        self.assertFalse((output/'staging').exists())
        self.assertIn('"stagingRemoved": true', (output/'result.local.json').read_text())


if __name__ == '__main__': unittest.main()
