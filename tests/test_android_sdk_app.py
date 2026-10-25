"""Generated XML/ZIP/ELF gate regressions; no build, JVM, SDK or device."""
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
from zipfile import ZipFile, ZipInfo, ZIP_STORED, ZIP_DEFLATED

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('android_sdk_gate', ROOT / 'mobile/android/check-sdk-app.py')
driver = importlib.util.module_from_spec(spec)
with patch('subprocess.Popen', side_effect=AssertionError('Import launched a process')):
    spec.loader.exec_module(driver)


def native_elf(alignment=16384, machine=183):
    data = bytearray(120); data[:6] = b'\x7fELF\x02\x01'
    struct.pack_into('<H', data, 18, machine)
    struct.pack_into('<Q', data, 32, 64)
    struct.pack_into('<HH', data, 54, 56, 1)
    struct.pack_into('<I', data, 64, 1)
    struct.pack_into('<Q', data, 112, alignment)
    return bytes(data)


class AndroidSdkAppTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='android-sdk-metadata-')
        self.addCleanup(self.temp.cleanup); self.root = Path(self.temp.name).resolve()
        self.methods = ['profileControl' + str(i) for i in range(17)]

    def junit(self, name='profile.xml', *, methods=None, failures=0, skipped=0, child=None):
        suite = ET.Element('testsuite', name=driver.PROFILE_CLASS, tests='17', failures=str(failures), errors='0', skipped=str(skipped))
        for method in self.methods if methods is None else methods:
            node = ET.SubElement(suite, 'testcase', name=method, classname=driver.PROFILE_CLASS)
            if child and method == self.methods[0]: ET.SubElement(node, child)
        path = self.root / name; ET.ElementTree(suite).write(path, encoding='utf-8')
        return path

    def aar(self, *, abi='arm64-v8a', data=None, classes=True, notices=True):
        path = self.root / 'sdk.aar'
        with ZipFile(path, 'w') as archive:
            archive.writestr('jni/' + abi + '/libsapflow_ffi.so', native_elf() if data is None else data)
            if classes: archive.writestr('classes.jar', b'generated-class-bytes')
            if notices:
                archive.writestr('META-INF/LICENSE', 'generated MIT notice')
                archive.writestr('META-INF/THIRD_PARTY_LICENSES.txt', 'generated dependency notice')
        return path

    def apk(self, *, align=True, data=None, compressed=False, abi='arm64-v8a'):
        path = self.root / 'app.apk'; name = 'lib/' + abi + '/libsapflow_ffi.so'
        info = ZipInfo(name); info.compress_type = ZIP_DEFLATED if compressed else ZIP_STORED
        if align:
            padding = (-(30 + len(name.encode()))) % 16384
            if padding < 4: padding += 16384
            info.extra = struct.pack('<HH', 0xffff, padding - 4) + bytes(padding - 4)
        with ZipFile(path, 'w') as archive: archive.writestr(info, native_elf() if data is None else data)
        return path

    def test_profile_method_selection_is_complete(self):
        source = '\n'.join('@Test fun ' + name + '() {}' for name in self.methods)
        self.assertEqual(driver.profile_test_methods(source), self.methods)

    def test_missing_profile_method_is_refused(self):
        with self.assertRaises(RuntimeError): driver.profile_test_methods('@Test fun one() {}')

    def test_duplicate_profile_method_is_refused(self):
        source = '\n'.join('@Test fun repeated() {}' for _ in self.methods)
        with self.assertRaises(RuntimeError): driver.profile_test_methods(source)

    def test_complete_real_case_count_is_accepted(self):
        result = driver.check_junit([self.junit()], self.methods)
        self.assertEqual(result['tests'], 17); self.assertEqual(result['profileMethods'], self.methods)

    def test_stale_or_cached_unit_tasks_are_refused(self):
        for state in ['UP-TO-DATE', 'FROM-CACHE', 'SKIPPED', 'FAILED']:
            with self.subTest(state=state), self.assertRaises(RuntimeError):
                driver.check_fresh_tasks('> Task :app:testDebugUnitTest ' + state + '\n> Task :app:testQualificationUnitTest\n')

    def test_both_actual_unit_tasks_are_required(self):
        with self.assertRaises(RuntimeError): driver.check_fresh_tasks('> Task :app:testDebugUnitTest\n')

    def test_fresh_unit_task_pair_is_accepted(self):
        driver.check_fresh_tasks('> Task :app:testDebugUnitTest\n> Task :app:testQualificationUnitTest\n')

    def test_suite_failure_is_refused(self):
        with self.assertRaises(RuntimeError): driver.check_junit([self.junit(failures=1)], self.methods)

    def test_suite_skip_is_refused(self):
        with self.assertRaises(RuntimeError): driver.check_junit([self.junit(skipped=1)], self.methods)

    def test_hidden_child_failure_is_refused(self):
        with self.assertRaises(RuntimeError): driver.check_junit([self.junit(child='failure')], self.methods)

    def test_hidden_child_skip_is_refused(self):
        with self.assertRaises(RuntimeError): driver.check_junit([self.junit(child='skipped')], self.methods)

    def test_wrong_method_with_same_total_is_refused(self):
        methods = self.methods[:-1] + ['unrelated']
        with self.assertRaises(RuntimeError): driver.check_junit([self.junit(methods=methods)], self.methods)

    def test_missing_xml_case_is_not_repaired_from_suite_count(self):
        with self.assertRaises(RuntimeError): driver.check_junit([self.junit(methods=self.methods[:-1])], self.methods)

    def test_missing_standard_count_attribute_is_refused(self):
        path = self.junit(); tree = ET.parse(path); del tree.getroot().attrib['errors']; tree.write(path)
        with self.assertRaises(RuntimeError): driver.check_junit([path], self.methods)

    def test_unnamed_suite_is_refused(self):
        path = self.junit(); tree = ET.parse(path); del tree.getroot().attrib['name']; tree.write(path)
        with self.assertRaises(RuntimeError): driver.check_junit([path], self.methods)

    def test_unclassed_case_is_refused(self):
        path = self.junit(); tree = ET.parse(path); del tree.getroot().find('testcase').attrib['classname']; tree.write(path)
        with self.assertRaises(RuntimeError): driver.check_junit([path], self.methods)

    def test_profile_named_methods_from_another_class_are_refused(self):
        path = self.junit(); tree = ET.parse(path)
        for case in tree.getroot().findall('testcase'): case.set('classname', 'Other')
        tree.write(path)
        with self.assertRaises(RuntimeError): driver.check_junit([path], self.methods)

    def test_duplicate_non_profile_case_cannot_inflate_total(self):
        profile = self.junit(); path = self.root / 'other.xml'
        suite = ET.Element('testsuite', name='Other', tests='2', failures='0', errors='0', skipped='0')
        for _ in range(2): ET.SubElement(suite, 'testcase', classname='Other', name='same')
        ET.ElementTree(suite).write(path)
        with self.assertRaises(RuntimeError): driver.check_junit([profile, path], self.methods)

    def test_duplicate_profile_suite_is_refused(self):
        with self.assertRaises(RuntimeError): driver.check_junit([self.junit('a.xml'), self.junit('b.xml')], self.methods)

    def test_whole_suite_floor_cannot_be_satisfied_by_profile_only(self):
        with self.assertRaises(RuntimeError): driver.check_junit([self.junit()], self.methods, minimum_tests=777)

    def test_custom_xml_entities_are_refused(self):
        path = self.root / 'entity.xml'; path.write_text('<!DOCTYPE x [<!ENTITY x "17">]><testsuite/>')
        with self.assertRaises(RuntimeError): driver.xml_root(path)

    def test_path_redaction_keeps_generated_xml_parseable(self):
        source = b'<testsuite path="/private/generated/check"><system-out>/private/generated/check/file</system-out></testsuite>'
        redacted = driver.redact_bytes(source, [('/private/generated/check', 'CHECKOUT')])
        self.assertNotIn(b'/private/generated/check', redacted)
        self.assertEqual(ET.fromstring(redacted).get('path'), 'CHECKOUT')

    def test_lint_warning_remains_visible(self):
        path = self.root / 'lint.xml'; path.write_text('<issues><issue severity="Warning"/></issues>')
        self.assertEqual(driver.check_lint(path)['warnings'], 1)

    def test_lint_error_is_refused(self):
        path = self.root / 'lint.xml'; path.write_text('<issues><issue severity="Error"/></issues>')
        with self.assertRaises(RuntimeError): driver.check_lint(path)

    def test_missing_lint_severity_is_refused(self):
        path = self.root / 'lint.xml'; path.write_text('<issues><issue/></issues>')
        with self.assertRaises(RuntimeError): driver.check_lint(path)

    def test_unknown_lint_severity_is_refused(self):
        path = self.root / 'lint.xml'; path.write_text('<issues><issue severity="unrecognised"/></issues>')
        with self.assertRaises(RuntimeError): driver.check_lint(path)

    def test_single_arm64_aar_with_notices_is_accepted(self):
        result = driver.inspect_aar(self.aar()); self.assertEqual(result['abis'], ['arm64-v8a'])

    def test_other_abi_aar_is_refused(self):
        with self.assertRaises(RuntimeError): driver.inspect_aar(self.aar(abi='x86_64'))

    def test_missing_classes_are_refused(self):
        with self.assertRaises(RuntimeError): driver.inspect_aar(self.aar(classes=False))

    def test_missing_dependency_notices_are_refused(self):
        with self.assertRaises(RuntimeError): driver.inspect_aar(self.aar(notices=False))

    def test_four_kib_elf_is_refused(self):
        with self.assertRaises(RuntimeError): driver.elf_alignments(native_elf(4096))

    def test_wrong_elf_machine_is_refused(self):
        with self.assertRaises(RuntimeError): driver.elf_alignments(native_elf(machine=62))

    def test_truncated_elf_program_header_is_refused(self):
        with self.assertRaises(RuntimeError): driver.elf_alignments(native_elf()[:-1])

    def test_aligned_apk_original_jni_is_accepted(self):
        import hashlib
        expected = hashlib.sha256(native_elf()).hexdigest()
        self.assertEqual(driver.inspect_apk(self.apk(), expected)['abis'], ['arm64-v8a'])

    def test_unaligned_apk_jni_is_refused(self):
        with self.assertRaises(RuntimeError): driver.inspect_apk(self.apk(align=False), 'a' * 64)

    def test_compressed_apk_jni_is_refused(self):
        with self.assertRaises(RuntimeError): driver.inspect_apk(self.apk(compressed=True), 'a' * 64)

    def test_changed_apk_jni_is_refused(self):
        with self.assertRaises(RuntimeError): driver.inspect_apk(self.apk(), 'a' * 64)

    def test_other_apk_abi_is_refused(self):
        with self.assertRaises(RuntimeError): driver.inspect_apk(self.apk(abi='x86_64'), 'a' * 64)

    def test_new_hidden_package_file_is_not_omitted(self):
        folder = self.root / 'package'; folder.mkdir(); (folder / 'sdk.aar').write_bytes(b'generated')
        before = driver.capture_package_files(folder)
        hidden = folder / '.new-input'; hidden.write_bytes(b'added')
        after = driver.capture_package_files(folder)
        self.assertIn(str(hidden), after); self.assertNotEqual(before, after)

    def test_package_symlink_is_refused(self):
        folder = self.root / 'package'; folder.mkdir(); (folder / 'link').symlink_to(self.junit())
        with self.assertRaises(RuntimeError): driver.capture_package_files(folder)


class AndroidNativeSuiteBindingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='android-native-suite-metadata-')
        self.addCleanup(self.temp.cleanup); self.root = Path(self.temp.name).resolve()
        self.profile = ['profileControl' + str(index) for index in range(17)]
        self.required = {name: ['nativeControl' + str(serial) + '_' + str(index) for index in range(count)]
                         for serial, (name, (path, count)) in enumerate(driver.NATIVE_TEST_SUITES.items())}

    def matrix(self):
        paths = []
        for serial, (name, methods) in enumerate([(driver.PROFILE_CLASS, self.profile), *self.required.items()]):
            suite = ET.Element('testsuite', name=name, tests=str(len(methods)), failures='0', errors='0', skipped='0')
            for method in methods: ET.SubElement(suite, 'testcase', name=method, classname=name)
            path = self.root / (str(serial) + '.xml'); ET.ElementTree(suite).write(path, encoding='utf-8'); paths.append(path)
        return paths

    def selected(self, paths, name):
        return next(path for path in paths if ET.parse(path).getroot().get('name') == name)

    def check(self, paths, minimum_tests=17, required=None):
        return driver.check_junit(paths, self.profile, minimum_tests, required_suites=self.required if required is None else required)

    def sources(self):
        return {path: '\n'.join('@Test fun ' + method + '() {}' for method in self.required[name])
                for name, (path, count) in driver.NATIVE_TEST_SUITES.items()}

    def test_source_bound_native_matrix_has_all_24_22_21_controls(self):
        self.assertEqual(driver.native_test_methods(self.sources()), self.required)
        self.assertEqual([len(methods) for methods in self.required.values()], [24, 22, 21])

    def test_missing_native_source_method_is_refused(self):
        for name, (path, count) in driver.NATIVE_TEST_SUITES.items():
            source = self.sources(); source[path] = '\n'.join(source[path].splitlines()[:-1])
            with self.subTest(suite=name), self.assertRaises(RuntimeError): driver.native_test_methods(source)

    def test_duplicated_native_source_method_is_refused(self):
        for name, (path, count) in driver.NATIVE_TEST_SUITES.items():
            source = self.sources(); rows = source[path].splitlines(); rows[-1] = rows[0]; source[path] = '\n'.join(rows)
            with self.subTest(suite=name), self.assertRaises(RuntimeError): driver.native_test_methods(source)

    def test_incomplete_or_foreign_native_source_namespace_is_refused(self):
        for mutation in ('missing', 'foreign'):
            source = self.sources()
            if mutation == 'missing': source.pop(next(iter(source)))
            else: source['foreign.kt'] = '@Test fun unrelated() {}'
            with self.subTest(mutation=mutation), self.assertRaises(RuntimeError): driver.native_test_methods(source)

    def test_complete_exact_native_suite_matrix_is_accepted(self):
        result = self.check(self.matrix(), minimum_tests=84)
        self.assertEqual(result['tests'], 84); self.assertEqual(result['requiredSuiteMethods'], self.required)
        self.assertEqual(result['profileMethods'], self.profile)

    def test_missing_native_suite_cannot_be_replaced_by_other_tests(self):
        for name in self.required:
            paths = self.matrix(); path = self.selected(paths, name)
            tree = ET.parse(path); suite = tree.getroot(); suite.set('name', 'Other')
            for case in suite.findall('testcase'): case.set('classname', 'Other')
            tree.write(path)
            with self.subTest(suite=name), self.assertRaises(RuntimeError): self.check(paths, minimum_tests=84)

    def test_native_suite_methods_from_foreign_class_are_refused(self):
        for name in self.required:
            paths = self.matrix(); path = self.selected(paths, name); tree = ET.parse(path)
            tree.getroot().find('testcase').set('classname', 'Other'); tree.write(path)
            with self.subTest(suite=name), self.assertRaises(RuntimeError): self.check(paths)

    def test_missing_native_method_with_honest_header_is_refused(self):
        for name in self.required:
            paths = self.matrix(); path = self.selected(paths, name); tree = ET.parse(path); suite = tree.getroot()
            suite.remove(suite.findall('testcase')[-1]); suite.set('tests', str(len(suite.findall('testcase')))); tree.write(path)
            with self.subTest(suite=name), self.assertRaises(RuntimeError): self.check(paths)

    def test_duplicate_native_method_with_unchanged_total_is_refused(self):
        for name in self.required:
            paths = self.matrix(); path = self.selected(paths, name); tree = ET.parse(path); cases = tree.getroot().findall('testcase')
            cases[-1].set('name', cases[0].get('name')); tree.write(path)
            with self.subTest(suite=name), self.assertRaises(RuntimeError): self.check(paths)

    def test_foreign_method_replayed_under_required_class_is_refused(self):
        for name in self.required:
            foreign = next(methods[0] for other, methods in self.required.items() if other != name)
            paths = self.matrix(); path = self.selected(paths, name); tree = ET.parse(path)
            tree.getroot().find('testcase').set('name', foreign); tree.write(path)
            with self.subTest(suite=name), self.assertRaises(RuntimeError): self.check(paths)

    def test_required_case_replayed_inside_foreign_suite_is_refused(self):
        for name, methods in self.required.items():
            paths = self.matrix(); path = self.root / 'replay.xml'
            suite = ET.Element('testsuite', name='Other', tests='1', errors='0', failures='0', skipped='0')
            ET.SubElement(suite, 'testcase', classname=name, name=methods[0]); ET.ElementTree(suite).write(path)
            with self.subTest(suite=name), self.assertRaises(RuntimeError): self.check([path, *paths])

    def test_hidden_native_failure_error_or_skip_is_refused(self):
        for name in self.required:
            for child in ('failure', 'error', 'skipped'):
                paths = self.matrix(); path = self.selected(paths, name); tree = ET.parse(path)
                ET.SubElement(tree.getroot().find('testcase'), child); tree.write(path)
                with self.subTest(suite=name, child=child), self.assertRaises(RuntimeError): self.check(paths)

    def test_duplicate_native_suite_is_refused(self):
        for name in self.required:
            paths = self.matrix(); duplicate = self.root / 'duplicate.xml'
            duplicate.write_bytes(self.selected(paths, name).read_bytes())
            with self.subTest(suite=name), self.assertRaises(RuntimeError): self.check([*paths, duplicate])

    def test_native_matrix_does_not_weaken_whole_suite_floor(self):
        with self.assertRaises(RuntimeError): self.check(self.matrix(), minimum_tests=85)

    def test_missing_native_standard_header_is_refused(self):
        for name in self.required:
            paths = self.matrix(); path = self.selected(paths, name); tree = ET.parse(path)
            del tree.getroot().attrib['skipped']; tree.write(path)
            with self.subTest(suite=name), self.assertRaises(RuntimeError): self.check(paths)

    def test_empty_or_duplicate_required_method_map_is_refused(self):
        with self.assertRaises(RuntimeError): self.check(self.matrix(), required={})
        for name in self.required:
            required = {key: list(methods) for key, methods in self.required.items()}; required[name][-1] = required[name][0]
            with self.subTest(suite=name), self.assertRaises(RuntimeError): self.check(self.matrix(), required=required)


class AndroidHostOwnerControlBindingTests(unittest.TestCase):
    def setUp(self):
        self.methods = ['hostControl' + str(index) for index in range(20)]

    def source(self, methods=None):
        return 'val tests = listOf(' + ', '.join('::' + name for name in (self.methods if methods is None else methods)) + ')\n'

    def output(self, methods=None, summary='SDK recording owner host controls passed: 20'):
        rows = ['SDK recording owner host control passed: ' + name for name in (self.methods if methods is None else methods)]
        if summary is not None: rows.append(summary)
        return '\n'.join(rows) + '\n'

    def test_host_source_requires_all_twenty_unique_named_controls(self):
        self.assertEqual(driver.host_test_methods(self.source()), self.methods)

    def test_missing_host_source_control_is_refused(self):
        with self.assertRaises(RuntimeError): driver.host_test_methods(self.source(self.methods[:-1]))

    def test_duplicate_host_source_control_is_refused(self):
        with self.assertRaises(RuntimeError): driver.host_test_methods(self.source(self.methods[:-1] + [self.methods[0]]))

    def test_multiple_or_computed_host_source_lists_are_refused(self):
        for source in (self.source() + self.source(), 'val tests = listOf(*unboundControls)\n', 'val tests = listOf()\n'):
            with self.subTest(source=source), self.assertRaises(RuntimeError): driver.host_test_methods(source)

    def test_exact_real_host_control_output_preserves_host_only_scope(self):
        result = driver.check_host_controls(self.output() + 'Harmless JVM diagnostic\n', driver.host_test_methods(self.source()))
        self.assertEqual(result['controls'], self.methods); self.assertEqual(result['count'], 20)
        self.assertEqual(result['scope'], 'SDK_HOST_FILESYSTEM_ONLY'); self.assertIs(result['passed'], True)
        for field in ('nativeEncoderQualified', 'presentationQualified', 'deviceActions'): self.assertIs(result[field], False)

    def test_missing_printed_control_cannot_be_repaired_by_twenty_summary(self):
        for index in range(20):
            methods = self.methods[:index] + self.methods[index + 1:]
            with self.subTest(index=index), self.assertRaises(RuntimeError): driver.check_host_controls(self.output(methods), self.methods)

    def test_replayed_printed_control_cannot_inflate_the_count(self):
        for index in range(20):
            methods = self.methods[:index] + [self.methods[index]] + self.methods[index:]
            with self.subTest(index=index), self.assertRaises(RuntimeError): driver.check_host_controls(self.output(methods), self.methods)

    def test_foreign_printed_control_with_unchanged_total_is_refused(self):
        with self.assertRaises(RuntimeError): driver.check_host_controls(self.output(self.methods[:-1] + ['foreignControl']), self.methods)

    def test_reordered_host_controls_are_refused(self):
        with self.assertRaises(RuntimeError): driver.check_host_controls(self.output(list(reversed(self.methods))), self.methods)

    def test_missing_duplicate_or_false_host_summary_is_refused(self):
        for summary in (None, 'SDK recording owner host controls passed: 0', 'SDK recording owner host controls passed: 19',
                        'SDK recording owner host controls passed: 20\nSDK recording owner host controls passed: 20'):
            with self.subTest(summary=summary), self.assertRaises(RuntimeError): driver.check_host_controls(self.output(summary=summary), self.methods)

    def test_skipped_or_failed_host_result_cannot_hide_behind_valid_output(self):
        for outcome in ('skipped', 'failed', 'error'):
            extra = 'SDK recording owner host control ' + outcome + ': ' + self.methods[0] + '\n'
            with self.subTest(outcome=outcome), self.assertRaises(RuntimeError): driver.check_host_controls(self.output() + extra, self.methods)

    def test_missing_or_repeated_expected_host_method_is_refused(self):
        for methods in (self.methods[:-1], self.methods[:-1] + [self.methods[0]]):
            with self.subTest(methods=methods), self.assertRaises(RuntimeError): driver.check_host_controls(self.output(), methods)


if __name__ == '__main__': unittest.main()
