#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Build the current ARM64 SDK and check the Android app without a device."""
from pathlib import Path
import argparse, hashlib, importlib.util, json, os, re, shutil, signal, stat, struct, subprocess, sys, tempfile, tomllib, uuid
import xml.etree.ElementTree as ET
from zipfile import ZipFile, ZIP_STORED

ROOT = Path(__file__).resolve().parents[2]
PROFILE_CLASS = 'dev.forgesworn.sapflow.broadcaster.AudioAacProfileTest'
PROFILE_TEST = 'mobile/android/app/src/test/java/dev/forgesworn/sapflow/broadcaster/AudioAacProfileTest.kt'
NATIVE_TEST_SUITES = {
    'dev.forgesworn.sapflow.broadcaster.AudioNativeAacGenerationTest':
        ('mobile/android/app/src/test/java/dev/forgesworn/sapflow/broadcaster/AudioNativeAacGenerationTest.kt', 24),
    'dev.forgesworn.sapflow.broadcaster.AudioNativeAacRunTest':
        ('mobile/android/app/src/test/java/dev/forgesworn/sapflow/broadcaster/AudioNativeAacRunTest.kt', 22),
    'dev.forgesworn.sapflow.broadcaster.AudioSourcePreservationTest':
        ('mobile/android/app/src/test/java/dev/forgesworn/sapflow/broadcaster/AudioSourcePreservationTest.kt', 21),
}
HOST_OWNER_SOURCES = tuple('mobile/android/app/src/main/java/dev/forgesworn/sapflow/broadcaster/' + name for name in (
    'CaptureProfile.kt', 'CaptureIntent.kt', 'RecordingSourceGate.kt', 'AudioAacOutputDisposition.kt',
    'AudioAacProfile.kt', 'AudioNativeAacGeneration.kt', 'AudioNativeAacRun.kt', 'RecordingCatalogue.kt',
    'RecordingAppendContext.kt', 'RecordingCheckpointStorage.kt', 'AndroidRecordingStorageIO.kt', 'RecordingCheckpointOwner.kt',
)) + ('mobile/android/host-tests/RecordingCheckpointOwnerHostTest.kt',)
NATIVE_LIBRARIES = ('libsapflow_ffi.so', 'libsapflow_jni.so')
MAX_METADATA = 16 * 1024 * 1024

def need(value, message):
    if not value: raise RuntimeError(message)

def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(65536), b''): digest.update(block)
    return digest.hexdigest()

def redact_bytes(data, replacements):
    for old, new in replacements: data = data.replace(old.encode(), new.encode())
    return data

def profile_test_methods(source):
    methods = re.findall(r'@Test\s+fun\s+(\w+)\s*\(\s*\)', source)
    need(len(methods) == len(set(methods)) == 17, 'Expected all seventeen selected AAC profile controls')
    return methods

def native_test_methods(sources):
    need(type(sources) is dict and set(sources) == {path for path, count in NATIVE_TEST_SUITES.values()},
         'Exact captured native AAC test sources required')
    required = {}
    for classname, (path, count) in NATIVE_TEST_SUITES.items():
        source = sources[path]
        need(type(source) is str, 'Native AAC test source text required')
        methods = re.findall(r'@Test\s+fun\s+(\w+)\s*\(\s*\)', source)
        need(len(methods) == len(set(methods)) == count, 'Complete unique native AAC controls required: ' + classname)
        required[classname] = methods
    return required


def xml_root(path):
    need(path.is_file() and not path.is_symlink() and 0 < path.stat().st_size <= MAX_METADATA, 'Bounded generated XML required')
    data = path.read_bytes()
    need(b'<!DOCTYPE' not in data and b'<!ENTITY' not in data, 'XML declarations with external/custom entities refused')
    return ET.fromstring(data)

def check_junit(paths, methods, minimum_tests=17, required_suites=None):
    need(paths and len(paths) <= 1024 and len(methods) == len(set(methods)) == 17 and type(minimum_tests) is int and minimum_tests >= 17, 'Complete bounded JUnit reports/profile methods required')
    required = {} if required_suites is None else required_suites
    need(type(required) is dict and len(required) <= 32 and (required_suites is None or required),
         'Bounded nonempty exact required suite map')
    for classname, expected in required.items():
        need(type(classname) is str and 0 < len(classname) <= 1024 and classname != PROFILE_CLASS and
             type(expected) is list and 0 < len(expected) <= 256 and
             all(type(name) is str and 0 < len(name) <= 1024 for name in expected) and
             len(expected) == len(set(expected)), 'Unique named required suite methods')
    totals = dict(tests=0, errors=0, failures=0, skipped=0)
    profile = []; suites_seen = set(); cases_seen = set(); required_seen = {}
    for path in paths:
        suite = xml_root(path); need(suite.tag == 'testsuite', 'Expected Gradle JUnit testsuite')
        suite_name = suite.get('name')
        need(type(suite_name) is str and 0 < len(suite_name) <= 1024 and suite_name not in suites_seen, 'Expected unique named JUnit suites')
        suites_seen.add(suite_name)
        need(all(re.fullmatch(r'[0-9]{1,9}', suite.get(name, '')) for name in totals), 'All standard JUnit count attributes are required')
        counts = {name: int(suite.get(name)) for name in totals}
        cases = suite.findall('testcase')
        need(all(value >= 0 for value in counts.values()) and counts['tests'] == len(cases), 'JUnit count does not match actual cases')
        need(not any(counts[name] for name in ('errors', 'failures', 'skipped')), 'Failed or skipped JVM tests')
        need(all(not any(case.findall(name) for name in ('error', 'failure', 'skipped')) for case in cases), 'JUnit child failure/skip cannot be hidden by summary')
        for case in cases:
            identity = (case.get('classname'), case.get('name'))
            need(all(type(value) is str and 0 < len(value) <= 1024 for value in identity) and identity not in cases_seen, 'Expected unique named/classed JUnit cases')
            need(identity[0] not in required or identity[0] == suite_name, 'Required methods must remain in their actual suite')
            cases_seen.add(identity)
        for name in totals: totals[name] += counts[name]
        if suite.get('name') == PROFILE_CLASS:
            need(all(case.get('classname') == PROFILE_CLASS for case in cases), 'Profile methods must belong to the actual profile class')
            profile.append([case.get('name') for case in cases])
        if suite_name in required:
            need(all(case.get('classname') == suite_name for case in cases), 'Required methods must belong to the actual required class')
            required_seen[suite_name] = [case.get('name') for case in cases]
    need(totals['tests'] >= minimum_tests and len(profile) == 1 and sorted(profile[0]) == sorted(methods), 'Missing, duplicated or incomplete AAC profile suite')
    need(set(required_seen) == set(required) and all(sorted(required_seen[name]) == sorted(expected) for name, expected in required.items()),
         'Missing, duplicated or incomplete native AAC suite')
    result = dict(totals, suites=len(paths), profileMethods=profile[0])
    if required: result['requiredSuiteMethods'] = required_seen
    return result

def host_test_methods(source):
    need(type(source) is str and len(source) <= MAX_METADATA, 'Bounded host control source required')
    lists = re.findall(r'\bval tests = listOf\(([^)]*)\)', source)
    need(len(lists) == 1 and re.fullmatch(r'\s*::[A-Za-z][A-Za-z0-9_]*(?:\s*,\s*::[A-Za-z][A-Za-z0-9_]*)*\s*', lists[0]),
         'One explicit host control function list required')
    methods = re.findall(r'::([A-Za-z][A-Za-z0-9_]*)', lists[0])
    need(len(methods) == len(set(methods)) == 20, 'All twenty unique host controls required')
    return methods


def check_host_controls(text, methods):
    need(type(text) is str and len(text) <= MAX_METADATA and type(methods) is list and
         len(methods) == 20 and all(type(name) is str and re.fullmatch(r'[A-Za-z][A-Za-z0-9_]*', name) for name in methods) and
         len(set(methods)) == 20, 'Bounded output and exact twenty host methods required')
    expected = ['SDK recording owner host control passed: ' + name for name in methods]
    expected.append('SDK recording owner host controls passed: 20')
    observed = [line for line in text.splitlines() if line.startswith('SDK recording owner host control')]
    need(observed == expected, 'Missing, duplicated, skipped, reordered or false host control result')
    return dict(scope='SDK_HOST_FILESYSTEM_ONLY', passed=True, controls=list(methods), count=20,
                nativeEncoderQualified=False, presentationQualified=False, deviceActions=False)


def host_serialization_jars(gradle_home):
    result = []
    for module in ('kotlinx-serialization-core-jvm', 'kotlinx-serialization-json-jvm'):
        parent = gradle_home / 'caches/modules-2/files-2.1/org.jetbrains.kotlinx' / module / '1.7.3'
        paths = sorted(parent.glob('*/' + module + '-1.7.3.jar'))
        need(len(paths) == 1 and re.fullmatch(r'[0-9a-f]{40}', paths[0].parent.name) and
             paths[0].is_file() and not paths[0].is_symlink() and paths[0].resolve() == paths[0],
             'One canonical already resolved serialization1.7.3 JAR required: ' + module)
        result.append(paths[0])
    return result


def check_fresh_tasks(text):
    for task in ('testDebugUnitTest', 'testQualificationUnitTest'):
        need(re.findall(r'(?m)^> Task :app:' + task + r'(?: ([^\n]+))?$', text) == [''], 'Expected actual fresh execution: ' + task)

def check_lint(path):
    report = xml_root(path); need(report.tag == 'issues', 'Expected Android lint issues')
    issues = report.findall('issue')
    need(all(item.get('severity') in ('Fatal', 'Error', 'Warning', 'Information', 'Ignore') for item in issues), 'Malformed Android lint severity')
    need(not any(item.get('severity') in ('Fatal', 'Error') for item in issues), 'Android lint errors')
    return dict(errors=0, warnings=sum(item.get('severity') == 'Warning' for item in issues), sha256=sha(path))

def elf_alignments(data):
    need(64 <= len(data) <= 128 * 1024 * 1024 and data[:6] == b'\x7fELF\x02\x01' and struct.unpack_from('<H', data, 18)[0] == 183, 'Expected bounded ARM64 ELF')
    offset = struct.unpack_from('<Q', data, 32)[0]
    size, count = struct.unpack_from('<HH', data, 54)
    need(size >= 56 and 0 < count <= 128 and offset + size * count <= len(data), 'Invalid ELF program headers')
    values = [struct.unpack_from('<Q', data, offset + i * size + 48)[0] for i in range(count) if struct.unpack_from('<I', data, offset + i * size)[0] == 1]
    need(values and all(value >= 16384 and not value & (value - 1) for value in values), 'Native ELF requires 16 KiB LOAD alignment')
    return values

def zip_read(archive, name, limit):
    entries = [item for item in archive.infolist() if item.filename == name]
    need(len(entries) == 1 and 0 < entries[0].file_size <= limit, 'Missing, duplicated or oversized ZIP member: ' + name)
    return archive.read(entries[0])

def inspect_aar(path):
    with ZipFile(path) as archive:
        names = [name for name in archive.namelist() if name.startswith('jni/') and name.endswith('.so')]
        need(len(names) == len(NATIVE_LIBRARIES) and set(names) == {'jni/arm64-v8a/' + name for name in NATIVE_LIBRARIES},
             'Expected fresh single-ABI ARM64 Rust and JNI SDK libraries')
        libraries = {}
        for name in NATIVE_LIBRARIES:
            data = zip_read(archive, 'jni/arm64-v8a/' + name, 128 * 1024 * 1024)
            libraries[name] = dict(sha256=hashlib.sha256(data).hexdigest(), loadAlignments=elf_alignments(data))
        classes = zip_read(archive, 'classes.jar', 32 * 1024 * 1024)
        notices = {name: hashlib.sha256(zip_read(archive, name, MAX_METADATA)).hexdigest() for name in ('META-INF/LICENSE', 'META-INF/THIRD_PARTY_LICENSES.txt')}
    native = libraries['libsapflow_ffi.so']
    return dict(aarSHA256=sha(path), nativeSHA256=native['sha256'], classesSHA256=hashlib.sha256(classes).hexdigest(),
                nativeLoadAlignments=native['loadAlignments'], nativeLibraries=libraries, notices=notices, abis=['arm64-v8a'])

def inspect_apk(path, expected_libraries):
    need(set(expected_libraries) == set(NATIVE_LIBRARIES), 'Both fresh SDK library identities required')
    libraries = {}
    with ZipFile(path) as archive:
        names = [name for name in archive.namelist() if name.startswith('lib/') and name.endswith('.so')]
        need(names and len(names) <= 16 and len(names) == len(set(names)) and {name.split('/')[1] for name in names} == {'arm64-v8a'}, 'Expected bounded ARM64-only application libraries')
        for name in names:
            data = zip_read(archive, name, 128 * 1024 * 1024); info = archive.getinfo(name)
            need(info.compress_type == ZIP_STORED, 'APK native library must be uncompressed')
            with path.open('rb') as stream:
                stream.seek(info.header_offset); header = stream.read(30)
            need(len(header) == 30 and header[:4] == b'PK\x03\x04', 'Invalid ZIP local header')
            filename, extra = struct.unpack_from('<HH', header, 26)
            need((info.header_offset + 30 + filename + extra) % 16384 == 0, 'APK native ZIP data requires 16 KiB alignment')
            libraries[name] = dict(sha256=hashlib.sha256(data).hexdigest(), loadAlignments=elf_alignments(data))
    for name, expected in expected_libraries.items():
        need(libraries.get('lib/arm64-v8a/' + name, {}).get('sha256') == expected['sha256'],
             'APK library differs from this run\'s fresh SDK: ' + name)
    return dict(apkSHA256=sha(path), libraries=libraries, abis=['arm64-v8a'])

def fact(path):
    node = path.lstat()
    if stat.S_ISLNK(node.st_mode):
        target = path.resolve(strict=True)
        return dict(link=os.readlink(path), resolved=str(target), target=fact(target), epoch=[node.st_dev, node.st_ino, node.st_mode, node.st_mtime_ns, node.st_ctime_ns])
    need(stat.S_ISREG(node.st_mode) and node.st_size <= 1024 * 1024 * 1024, 'Bounded regular input required')
    result = dict(sha256=sha(path), bytes=node.st_size, mode=node.st_mode, uid=node.st_uid, links=node.st_nlink, epoch=[node.st_dev, node.st_ino, node.st_size, node.st_mtime_ns, node.st_ctime_ns])
    after = path.lstat(); need(result['epoch'] == [after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns], 'Input changed while hashing')
    return result

def capture_package_files(path):
    need(path.is_dir() and not path.is_symlink() and path.resolve() == path, 'Canonical fresh package namespace')
    values = {}
    for member in path.rglob('*'):
        need(not member.is_symlink(), 'Unexpected package symlink')
        if member.is_file(): values[str(member)] = fact(member)
        else: need(member.is_dir(), 'Unexpected special package member')
    return values

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence', type=Path, help='Fresh direct child of target/android-sdk-app (default unique)')
    options = parser.parse_args()
    need(sys.platform in ('linux', 'darwin'), 'Unix host required; no device is used')
    need(not os.environ.get('CARGO_TARGET_DIR') and not os.environ.get('CARGO_BUILD_TARGET') and os.environ.get('CARGO_INCREMENTAL') == '0', 'Use the checkout target and existing CARGO_INCREMENTAL=0')
    need(not any(name.startswith('DYLD_') for name in os.environ), 'Dynamic loader overrides refused')
    overrides = ('JAVA_TOOL_OPTIONS', '_JAVA_OPTIONS', 'JDK_JAVA_OPTIONS', 'GRADLE_OPTS', 'JAVA_OPTS', 'NODE_OPTIONS', 'RUSTC', 'RUSTC_WRAPPER', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'LD_LIBRARY_PATH')
    need(not any(os.environ.get(name) for name in overrides), 'Hidden compiler/runtime overrides refused')
    root = ROOT; need(root.resolve() == root, 'Canonical checkout required')
    rust_channel = tomllib.loads((root / 'rust-toolchain.toml').read_text())['toolchain']['channel']
    selected_rust = os.environ.get('RUSTUP_TOOLCHAIN', '')
    need(not selected_rust or selected_rust == rust_channel or selected_rust.startswith(rust_channel + '-'), 'Use the repository\'s pinned Rust toolchain')
    jdk = Path(os.environ['JAVA_HOME']).resolve(strict=True)
    sdk = Path(os.environ['ANDROID_HOME']).resolve(strict=True)
    ndk = Path(os.environ['ANDROID_NDK_HOME']).resolve(strict=True)
    kotlin = Path(os.environ['KOTLIN_HOME']).resolve(strict=True)
    need('JAVA_VERSION="21.' in (jdk / 'release').read_text(), 'Select JDK21')
    need(re.search(r'Pkg.Revision\s*=\s*27\.1\.12297006\s*$', (ndk / 'source.properties').read_text(), re.MULTILINE), 'Select the existing builder\'s NDK27.1.12297006')
    tools = {name: Path(shutil.which(name)).absolute() for name in ('node', 'cargo', 'kotlinc', 'zip', 'unzip', 'git') if shutil.which(name)}
    need(len(tools) == 6 and tools['kotlinc'].resolve() == (kotlin / 'bin/kotlinc').resolve(), 'Required tools and explicit Kotlin compiler selection')
    aapt2 = sdk / 'build-tools/36.1.0/aapt2'; zipalign = sdk / 'build-tools/36.1.0/zipalign'
    host_tag = 'darwin-x86_64' if sys.platform == 'darwin' else 'linux-x86_64'
    linker = ndk / ('toolchains/llvm/prebuilt/' + host_tag + '/bin/aarch64-linux-android23-clang')
    fixed = [Path(sys.executable).resolve(), jdk / 'bin/java', jdk / 'release', jdk / 'lib/modules', aapt2, zipalign, sdk / 'platforms/android-36/android.jar', sdk / 'platforms/android-36/source.properties', ndk / 'source.properties', linker, linker.parent / 'clang', linker.parent / 'ld.lld', kotlin / 'lib/kotlin-compiler.jar', kotlin / 'lib/kotlin-stdlib.jar', *tools.values()]
    target = root / 'target'; target.mkdir(exist_ok=True)
    parent = target / 'android-sdk-app'; parent.mkdir(mode=0o700, exist_ok=True)
    need(parent.resolve() == parent and not parent.is_symlink(), 'Canonical evidence parent')
    if options.evidence:
        evidence = options.evidence.absolute()
        need(evidence.parent == parent and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,79}', evidence.name) and not evidence.exists() and not evidence.is_symlink(), 'Fresh direct evidence child required')
        evidence.mkdir(mode=0o700)
    else: evidence = Path(tempfile.mkdtemp(prefix='run-', dir=parent))
    scratch = Path(tempfile.mkdtemp(prefix='sapflow-android-sdk-app-', dir=target))
    replacements = sorted({str(root): 'CHECKOUT', str(Path.home()): 'USER_HOME', str(jdk): 'JDK_HOME', str(sdk): 'ANDROID_SDK', str(ndk): 'ANDROID_NDK', str(kotlin): 'KOTLIN_HOME'}.items(), key=lambda row: -len(row[0]))
    def text_public(text):
        for old, new in replacements: text = text.replace(old, new)
        return text
    def public(value):
        if isinstance(value, str): return text_public(value)
        if isinstance(value, list): return [public(item) for item in value]
        if isinstance(value, dict): return {text_public(str(name)): public(item) for name, item in value.items()}
        return value
    def write(path, value):
        with path.open('x') as stream: json.dump(public(value), stream, indent=2); stream.write('\n')
    def retain_log(source, destination, limit=64*1024*1024):
        need(source.is_file() and not source.is_symlink() and source.stat().st_size <= limit, 'Bounded command log')
        data = redact_bytes(source.read_bytes(), replacements)
        with destination.open('xb') as stream: stream.write(data)
        return sha(destination)
    git = tools['git']
    def git_read(*args):
        return subprocess.check_output([str(git), '--no-optional-locks', '-c', 'core.fsmonitor=false', *args], cwd=root, timeout=20)
    def capture():
        gradle_home = Path(os.environ.get('GRADLE_USER_HOME') or Path.home() / '.gradle').resolve()
        need(not (root / 'mobile/android/local.properties').exists(), 'Clean SDK selection requires no ignored local.properties')
        need(not any(name.startswith('ORG_GRADLE_PROJECT_') for name in os.environ), 'Unbound Gradle project environment properties refused')
        need(not any((gradle_home / name).exists() for name in ('gradle.properties', 'init.gradle', 'init.gradle.kts')), 'Unbound global Gradle properties/init scripts refused')
        need(not any(path.is_file() for path in (gradle_home / 'init.d').rglob('*') if path.name.endswith(('.gradle', '.gradle.kts'))), 'Global Gradle init scripts refused')
        names = git_read('ls-files', '--cached', '--others', '--exclude-standard', '-z')
        files = {name.decode(): fact(root / name.decode()) for name in names.split(b'\0') if name}
        need(names == git_read('ls-files', '--cached', '--others', '--exclude-standard', '-z'), 'Complete source namespace changed')
        return dict(head=git_read('rev-parse', 'HEAD').decode().strip(), status=git_read('status', '--porcelain=v1', '--untracked-files=all').decode(), source=files, tools={str(path): fact(path) for path in fixed})
    before = capture(); write(evidence / 'inputs-before.json', before)
    spec = importlib.util.spec_from_file_location('android_owned', root / 'mobile/ios/checkpoint_test_processes.py')
    owner = importlib.util.module_from_spec(spec); spec.loader.exec_module(owner)
    active = None; launching = retiring = finalising = False; pending = None; unknown = False
    commands = []; cleanup_errors = []; failure = None; after = None; bracket = None; stable = False; package = None; tests = {}; lint = {}; apks = {}; isolation = None; host_controls = None; generated = {}; package_captured = False
    env = {**os.environ, 'JAVA_HOME': str(jdk), 'ANDROID_HOME': str(sdk), 'ANDROID_SDK_ROOT': str(sdk), 'ANDROID_NDK_HOME': str(ndk), 'KOTLIN_HOME': str(kotlin), 'PYTHONDONTWRITEBYTECODE': '1', 'PATH': str(jdk / 'bin') + os.pathsep + str(kotlin / 'bin') + os.pathsep + os.environ.get('PATH', '')}
    def caught(number, frame):
        nonlocal pending
        pending = pending or number
        if not launching and not retiring and not finalising: raise InterruptedError('Signal ' + str(number))
    previous = {number: signal.signal(number, caught) for number in (signal.SIGINT, signal.SIGTERM)}
    def retire(owned, label):
        nonlocal active, retiring
        if owned['cleanupAttempted']: return (owned.get('groupEvidence') or {}).get('stopped', False)
        retiring = True; owned['cleanupAttempted'] = True
        try: return owner.stop_owned_group(owned, scratch, label)
        except BaseException as error: cleanup_errors.append(type(error).__name__ + ': ' + str(error)); return False
        finally:
            group = owned.get('groupEvidence')
            if group:
                try:
                    for index, inventory in enumerate(group['inventories'], 1):
                        for suffix in ('stdout', 'stderr'):
                            name = 'owned-cleanup-' + label + '-' + str(index) + '.' + suffix
                            inventory['original' + suffix.title() + 'SHA256'] = inventory[suffix + 'SHA256']
                            inventory[suffix + 'SHA256'] = retain_log(scratch / name, evidence / name, 2*1024*1024)
                    group['commandPathsRedacted'] = True
                except BaseException as error: cleanup_errors.append('Inventory retention: ' + str(error))
            if (group or {}).get('stopped'): active = None
            retiring = False
    def run(label, argv, timeout):
        nonlocal active, launching, unknown
        need(active is None and not unknown and pending is None and not cleanup_errors and capture() == before, 'Previous process/signal or source/tool change unresolved')
        need(current_generated() == generated, 'Fresh SDK/APK input or package namespace changed')
        row = dict(label=label, argv=list(map(str, argv)), exit=None, closed=False, launchAttempted=False, launchOwnershipKnown=False)
        commands.append(row); owned = None
        try:
            with (scratch / (label + '.stdout')).open('xb') as output, (scratch / (label + '.stderr')).open('xb') as diagnostics:
                launching = True; row['launchAttempted'] = True
                try:
                    child = subprocess.Popen(row['argv'], cwd=root, env=env, stdin=subprocess.DEVNULL, stdout=output, stderr=diagnostics, start_new_session=True)
                    owned = dict(child=child, cleanupAttempted=False); active = owned; row['launchOwnershipKnown'] = True
                finally: launching = False
                need(pending is None, 'Signal during process launch')
                child.wait(timeout=timeout)
        except BaseException as error: row['failure'] = type(error).__name__ + ': ' + str(error)
        finally:
            launching = False
            if owned:
                row['closed'] = retire(owned, label); row['exit'] = owned['child'].returncode; row['processGroup'] = owned.get('groupEvidence')
            elif row['launchAttempted']: unknown = True
            for suffix in ('stdout', 'stderr'):
                try:
                    raw = scratch / (label + '.' + suffix)
                    row[suffix + 'SHA256'] = retain_log(raw, evidence / (label + '.' + suffix)) if raw.exists() else None
                except BaseException as error: row.setdefault('failure', 'Log retention: ' + str(error))
        need(row['exit'] == 0 and row['closed'] and 'failure' not in row and active is None and not unknown and pending is None and not cleanup_errors, 'Failed or unclosed command: ' + label)
        need(capture() == before, 'Source/tool change during ' + label)
        return (evidence / (label + '.stdout')).read_text() + (evidence / (label + '.stderr')).read_text()
    package_root = target / ('native-packages-android-sdk-app-' + uuid.uuid4().hex[:16])
    def current_generated():
        values = capture_package_files(package_root) if package_captured else {}
        values.update({name: fact(Path(name)) for name in generated if not Path(name).is_relative_to(package_root)})
        return values
    try:
        version = tomllib.loads((root / 'Cargo.toml').read_text())['workspace']['package']['version']
        need(re.fullmatch(r'[0-9A-Za-z][0-9A-Za-z.+-]{0,127}', version), 'Filename-safe workspace version')
        need(re.search(r'kotlinc-jvm\s+2\.2\.20\b', run('kotlin-version', [tools['kotlinc'], '-version'], 60)), 'Select the pinned Kotlin2.2.20 compiler')
        run('sdk-build', [tools['node'], 'scripts/build-native.mjs', 'android', '--abis=arm64-v8a', '--output-root=' + str(package_root)], 1800)
        generated = capture_package_files(package_root); package_captured = True
        host_libraries = tuple(target / ('debug/' + name.removesuffix('.so') + ('.dylib' if sys.platform == 'darwin' else '.so'))
                               for name in NATIVE_LIBRARIES)
        need(all(path.is_file() and not path.is_symlink() and path.resolve() == path for path in host_libraries),
             'Current SDK builder host Rust and JNI libraries required')
        generated.update({str(path): fact(path) for path in host_libraries})
        folder = package_root / 'android-arm64-v8a'; aar = folder / ('sapflow-' + version + '.aar')
        package = inspect_aar(aar)
        need(package['notices']['META-INF/LICENSE'] == sha(root / 'LICENSE') and package['notices']['META-INF/THIRD_PARTY_LICENSES.txt'] == sha(folder / 'THIRD_PARTY_LICENSES.txt'), 'Actual AAR/source dependency notices')
        build = json.loads((folder / 'build-info.json').read_text())
        need(build['version'] == version and build['platform'] == 'android' and build['built'] is True and build['installedConsumerPassed'] is True and build['abis'] == ['arm64-v8a'] and build['physicalDeviceQualified'] is False, 'Fresh packaged consumer must pass')
        verification = json.loads((folder / 'verification.json').read_text())
        need(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._+-]{0,254}', verification['archive']) and sha(package_root / verification['archive']) == verification['sha256'], 'Actual packaged SDK archive checksum')
        package['archiveSHA256'] = verification['sha256']
        methods = profile_test_methods((root / PROFILE_TEST).read_text())
        native_sources = {}
        for path, count in NATIVE_TEST_SUITES.values():
            source = (root / path).read_bytes()
            need(hashlib.sha256(source).hexdigest() == before['source'][path]['sha256'], 'Native AAC test source differs from captured input: ' + path)
            native_sources[path] = source.decode('utf-8')
        required_suites = native_test_methods(native_sources)
        text = run('gradle', ['mobile/android/build-debug.sh', '--max-workers=1', '--rerun-tasks', '-Dorg.gradle.jvmargs=-Xmx1024m', '-Dorg.gradle.java.home=' + str(jdk), '-PsapflowAar=' + str(aar), ':app:testQualificationUnitTest', ':app:assembleQualification', ':app:assembleRelease', ':app:lintDebug', ':app:lintQualification', ':app:lintRelease', ':app:verifyQualificationIsolation'], 1800)
        check_fresh_tasks(text)
        for task in ('testDebugUnitTest', 'testQualificationUnitTest'):
            paths = sorted((target / 'mobile-android/app/test-results' / task).glob('TEST-*.xml'))
            tests[task] = check_junit(paths, methods, {'testDebugUnitTest': 825, 'testQualificationUnitTest': 796}[task], required_suites=required_suites)
            tests[task]['reports'] = {}
            for path in paths:
                destination = evidence / 'xml' / task / path.name; destination.parent.mkdir(parents=True, exist_ok=True)
                tests[task]['reports'][path.name] = dict(originalSHA256=sha(path), retainedSHA256=retain_log(path, destination, MAX_METADATA))
        for variant in ('debug', 'qualification', 'release'):
            report = target / 'mobile-android/app/reports' / ('lint-results-' + variant + '.xml')
            lint[variant] = check_lint(report); destination = evidence / 'lint' / report.name; destination.parent.mkdir(exist_ok=True)
            lint[variant]['originalXMLSHA256'] = lint[variant]['sha256']
            lint[variant]['sha256'] = retain_log(report, destination, MAX_METADATA)
            paths = list((target / 'mobile-android/app/outputs/apk' / variant).glob('*.apk')); need(len(paths) == 1, 'One APK per variant')
            apks[variant] = dict(path=str(paths[0]), **inspect_apk(paths[0], package['nativeLibraries']))
            generated[str(paths[0])] = fact(paths[0])
        host_source = None
        for name in HOST_OWNER_SOURCES:
            source = (root / name).read_bytes()
            need(hashlib.sha256(source).hexdigest() == before['source'][name]['sha256'], 'Host owner source differs from captured input: ' + name)
            if name == HOST_OWNER_SOURCES[-1]: host_source = source.decode('utf-8')
        host_methods = host_test_methods(host_source)
        gradle_home = Path(os.environ.get('GRADLE_USER_HOME') or Path.home() / '.gradle').resolve()
        serialization = host_serialization_jars(gradle_home)
        generated.update({str(path): fact(path) for path in serialization})
        host_root = scratch / 'recording-owner-host'; host_root.mkdir(mode=0o700)
        host_tmp = host_root / 'tmp'; host_tmp.mkdir(mode=0o700)
        host_data = host_root / 'data'; host_data.mkdir(mode=0o700)
        host_classes = host_root / 'classes.jar'; host_binary = host_root / 'controls.jar'
        with ZipFile(aar) as archive:
            with host_classes.open('xb') as stream: stream.write(zip_read(archive, 'classes.jar', 32 * 1024 * 1024))
        need(sha(host_classes) == package['classesSHA256'], 'Host controls must use the actual fresh AAR classes')
        generated[str(host_classes)] = fact(host_classes)
        classpath = os.pathsep.join(str(path) for path in (host_classes, *serialization, sdk / 'platforms/android-36/android.jar'))
        run('host-owner-compile', [tools['kotlinc'], '-language-version', '2.2', '-api-version', '2.2', '-jvm-target', '17',
            '-J-Djava.io.tmpdir=' + str(host_tmp), *[root / name for name in HOST_OWNER_SOURCES], '-cp', classpath,
            '-include-runtime', '-d', host_binary], 180)
        need(host_binary.is_file() and not host_binary.is_symlink(), 'Actual compiled host controls required')
        generated[str(host_binary)] = fact(host_binary)
        output = run('host-owner-controls', [jdk / 'bin/java', '-Djava.io.tmpdir=' + str(host_tmp),
            '-Djava.library.path=' + str(host_libraries[0].parent),
            '-cp', str(host_binary) + os.pathsep + classpath,
            'dev.forgesworn.sapflow.broadcaster.RecordingCheckpointOwnerHostTestKt', host_data], 180)
        host_controls = check_host_controls(output, host_methods)
        host_controls['compiledSources'] = list(HOST_OWNER_SOURCES)
        controls = run('python-controls', [sys.executable, '-B', '-m', 'unittest', 'discover', '-s', 'mobile/android/tests', '-p', 'test_verify_qualification.py', '-v'], 120)
        need(re.findall(r'Ran (\d+) tests?', controls) == ['39'] and re.search(r'(?m)^OK$', controls), 'All39 artifact-verifier controls must pass')
        result = run('native-isolation', [sys.executable, '-B', 'mobile/android/verify-qualification.py', apks['qualification']['path'], aar, '--aapt2', aapt2, '--expected-aar-sha256', package['aarSHA256'], '--expected-native-sha256', package['nativeSHA256'], '--expect-pose-hook', '--release-apk', apks['release']['path'], '--expect-v4v-connector'], 120)
        isolation = json.loads(result)
        need(isolation['native_sha256'] == package['nativeSHA256'] and isolation['release_isolation']['native_sha256'] == package['nativeSHA256'], 'Actual native isolation proof')
        for variant in ('debug', 'qualification', 'release'):
            run(variant + '-zipalign', [zipalign, '-c', '-P', '16', '-v', '4', apks[variant]['path']], 60)
        tree = run('debug-manifest', [aapt2, 'dump', 'xmltree', '--file', 'AndroidManifest.xml', apks['debug']['path']], 60)
        need(re.search(r':debuggable\([^)]*\)=(true|0xffffffff)', tree) and 'FfiBenchmarkActivity' in tree and 'NativeAacQualificationActivity' in tree, 'Actual debug manifest/profile components')
    except BaseException as error: failure = type(error).__name__ + ': ' + str(error)
    finally:
        finalising = True
        if active is not None and not active['cleanupAttempted']: retire(active, 'terminal-active')
        generated_after = None
        try:
            bracket = capture()
            generated_after = current_generated()
            need(generated_after == generated, 'Generated SDK/APK inputs changed')
            after = capture(); stable = before == bracket == after
            write(evidence / 'inputs-after.json', after)
        except BaseException as error: failure = failure or type(error).__name__ + ': ' + str(error)
        if not stable: failure = failure or 'Source/tool bindings changed'
        stopped = active is None and not unknown and not cleanup_errors and all(row['closed'] for row in commands)
        if stopped:
            try:
                need(scratch.parent == target and scratch.name.startswith('sapflow-android-sdk-app-') and not scratch.is_symlink(), 'Owned raw-log scratch boundary')
                shutil.rmtree(scratch)
            except BaseException as error: cleanup_errors.append('Scratch cleanup: ' + str(error))
        passed = failure is None and stable and stopped and not cleanup_errors and pending is None and len(commands) == 11 and host_controls is not None and len(tests) == 2 and len(lint) == 3 and len(apks) == 3 and isolation is not None and not scratch.exists()
        result = dict(schema='android-current-sdk-app-gate-v1', passed=passed, failure=failure, sourceUnchanged=stable, inputsBefore=before, inputsAtFinalReadbackStart=bracket, inputsAfter=after, generatedInputsBefore=generated, generatedInputsAfter=generated_after, package=package, tests=tests, lint=lint, apks=apks, nativeIsolation=isolation, hostOwnerControls=host_controls, commands=commands, cleanupErrors=cleanup_errors, childrenStopped=stopped, unconfirmedLaunchAttempt=unknown, activeOwnedPID=active['child'].pid if active else None, pendingSignal=pending, scratchRemoved=not scratch.exists(), actualStages=len(commands), expectedStages=11, expectedBuildStages=10, sdkBuiltFromCurrentSource=package is not None and any(row['label'] == 'sdk-build' and row['exit'] == 0 and row['closed'] for row in commands), allAbiSdkQualified=False, nativeEncoderQualified=False, presentationQualified=False, deviceActions=False, recorderAdoption=False, releaseQualified=False)
        mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
        try:
            result['terminalPublication'] = dict(protocol='MASKED_PENDING_INSPECTION_AND_ATOMIC_PUBLICATION_V1', committed=True)
            temporary = evidence / 'receipt.precommit.json'; write(temporary, result)
            observed = pending or next(iter(sorted(signal.sigpending() & {signal.SIGINT, signal.SIGTERM})), None)
            if observed is not None:
                result.update(passed=False, pendingSignal=observed, failure=result['failure'] or 'Deferred signal ' + str(observed))
                temporary.unlink(); write(temporary, result)
            os.replace(temporary, evidence / 'receipt.json')
        except BaseException as error:
            result.update(passed=False, failure=result['failure'] or 'Terminal publication: ' + str(error)); result['terminalPublication']['committed'] = False
            try:
                failed = evidence / 'receipt.failed-precommit.json'; write(failed, result); os.replace(failed, evidence / 'receipt.json')
            except BaseException as secondary: print(text_public(str(secondary)), file=sys.stderr)
            raise
        finally:
            for number, handler in previous.items(): signal.signal(number, handler)
            signal.pthread_sigmask(signal.SIG_SETMASK, mask)
        print(json.dumps(public(dict(passed=result['passed'], failure=result['failure'], receipt=str(evidence / 'receipt.json')))), flush=True)
    if not result['passed']: raise SystemExit(1)

if __name__ == '__main__': main()
