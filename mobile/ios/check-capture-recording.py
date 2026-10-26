#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Fresh current-host, generated-only native recording matrix. No devices."""
from pathlib import Path
import argparse, datetime, hashlib, importlib.util, json, os, platform, re, shutil, signal, stat, subprocess, sys, tempfile, time

ROOT = Path(__file__).resolve().parents[2]
STORED = ['healthy', 'mixed-zero-raw', 'publishing-independent-failure', 'missing-file-EOF', 'recording-queue-failure']
SDK = ['producer-audio', 'producer-av', 'producer-publishing-failure', 'producer-late-native-drop', 'producer-recording-loss', 'producer-input-not-closed', 'producer-wrong-worker', 'producer-late-encoded-output', 'producer-late-timing', 'producer-wrong-ticket']
LEGACY = ['producer-audio', 'producer-av', 'producer-recording-loss', 'producer-late-native-drop', 'producer-input-not-closed', 'producer-wrong-worker', 'producer-wrong-ticket']
COMMAND_LOGS = (
    'sdk-path.log', 'rust-sdk.log', 'sdk-compile.log', 'fixture-compile.log', 'native-run.local.json',
    *(name + '-packets.local.json' for name in STORED[:4]),
    *(name + suffix for name in SDK[:3] for suffix in ('-streams.local.json', '-decode.local.log')),
    *('legacy-' + name + suffix for name in LEGACY[:2] for suffix in ('-streams.local.json', '-decode.local.log')),
)
NAMES = ['CaptureRecordingProducer', 'HardwareVideoEncoder', 'VideoEncodingPolicy', 'PreparedVideoSlate', 'AudioRecordingTiming', 'AudioShowLifecycle', 'NativeAudioEncoder', 'CompressedRecorder', 'QuickTimeAudioPriming', 'QuickTimeAudioPrimingFile', 'QuickTimeMovieLayout', 'RecordingCatalogue', 'RecordingStorage', 'RecordingCaptureAdmission', 'AudioInputPolicy', 'CameraControlPolicy', 'CaptureAdmission', 'CaptureProfile', 'CaptureOrientation', 'PublishingTelemetry', 'EncodedWorkBudget', 'EncodedPipeline', 'EncodedIngressWorker', 'CaptureRecording', 'RecordingCheckpointOwner']

def need(ok, message):
    if not ok: raise RuntimeError(message)

def validate_commands(commands, through=None):
    """Require each actual build/oracle stage once, in order and successful."""
    expected = COMMAND_LOGS if through is None else COMMAND_LOGS[:COMMAND_LOGS.index(through) + 1]
    need(tuple(row['log'] for row in commands) == expected, 'Exact ordered current-host SDK and media oracle stages required')
    need(all(row['exit'] == 0 and row['groupStopped'] is True and row['error'] is None for row in commands),
         'Every current-host SDK and media oracle stage must succeed and stop')

def sha(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(65536), b''): value.update(block)
    return value.hexdigest()

def read(path, limit=2*1024*1024):
    need(path.is_file() and not path.is_symlink() and path.resolve() == path and 0 < path.stat().st_size <= limit, 'Bounded canonical JSON: ' + str(path))
    return json.loads(path.read_text())

def filename_component(value):
    need(type(value) is str and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,254}', value) is not None and value not in ('.', '..'), 'Media filename must be one safe component')
    return value

def fixture_media(fixture, folder, filename):
    parent = fixture / filename_component(folder)
    media = parent / filename_component(filename)
    need(parent.is_dir() and not parent.is_symlink() and parent.resolve() == parent and media.is_file() and not media.is_symlink() and media.resolve().parent == parent, 'Generated fixture file escaped its case directory')
    need(0 < media.stat().st_size <= 512*1024*1024, 'Bounded positive generated media')
    return media

def validate_matrix(summary):
    """Refuse incomplete matrix/lifecycle/path facts before any media oracle."""
    need(type(summary) is dict and summary['passed'] is True and summary['generatedOnly'] is True, 'Successful generated-only matrix required')
    for key, expected in [('cases', STORED), ('producerCases', SDK), ('legacyProducerCases', LEGACY)]:
        rows = summary[key]
        need(type(rows) is list and [row['name'] for row in rows] == expected and all(row['passed'] is True for row in rows), 'Exact successful ordered matrix: ' + key)
    for index, row in enumerate(summary['cases']):
        need(row['sdkReceiptState'] == 'incomplete' and row['appAdmission'] == 'unknown', 'Stored controls must remain incomplete/unknown')
        need(all(row[key] is True for key in ('actualCheckedCleanup', 'recordingClosedBeforeTransport', 'validatedExactlyOnce')), 'Stored actual cleanup/validation facts')
        need(row['exactPayloadProbeRequired'] is (index < 4), 'Exactly four stored payload probes')
        filename_component(row['media'])
    for key, backend, positive in [('producerCases', 'sdkCheckpointed', 3), ('legacyProducerCases', 'legacy', 2)]:
        for index, row in enumerate(summary[key]):
            need(row['backend'] == backend and row['finalised'] is (index < positive) and type(row['hasVideo']) is bool, 'Actual producer backend/completion/topology')
            need(all(row[name] is True for name in ('normalStopCancellationPresent', 'producerCapabilitySingleUse', 'duplicateFinishPreservedBytes', 'recordingClosedBeforeTransport')), 'Actual producer lifecycle facts')
            filename_component(row['file'])

def file_meta(path, system=False):
    value = path.lstat()
    need(path.is_absolute() and path.resolve() == path and stat.S_ISREG(value.st_mode) and value.st_nlink == 1 and 0 <= value.st_size <= 512*1024*1024 and value.st_uid in ({0, os.getuid()} if system else {os.getuid()}), 'Canonical bounded single-link owned file: ' + str(path))
    return dict(type='file', mode=oct(stat.S_IMODE(value.st_mode)), bytes=value.st_size, uid=value.st_uid, links=value.st_nlink, epoch=[value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns])

def file_fact(path, system=False):
    before = file_meta(path, system); digest = sha(path)
    need(file_meta(path, system) == before, 'File changed while hashing: ' + str(path))
    return dict(before, sha256=digest)

def entry_fact(path):
    node = path.lstat()
    need(stat.S_ISREG(node.st_mode) or stat.S_ISLNK(node.st_mode), 'Compiler/tool command entry must be regular or a symlink')
    return dict(resolved=str(path.resolve()), literal=os.readlink(path) if path.is_symlink() else None, mode=oct(stat.S_IMODE(node.st_mode)), epoch=[node.st_dev, node.st_ino, node.st_mtime_ns, node.st_ctime_ns])

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence', type=Path, help='New direct child of target/ios-native-recording (default unique)')
    args = parser.parse_args()
    need(__debug__, 'Run without Python optimisation; frozen native predicates use assertions')
    need(sys.platform == 'darwin' and platform.machine() in ('arm64', 'x86_64'), 'Current macOS ARM64 or Intel host with full Xcode required')
    stale = [name for name in ('SAPFLOW_CAPTURE_RECORDING_HOST_WITNESS', 'SAPFLOW_CAPTURE_RECORDING_HOST_WITNESS_SHA256', 'SAPFLOW_CAPTURE_RECORDING_APPLE_RECEIPT', 'SAPFLOW_CAPTURE_RECORDING_APPLE_RECEIPT_SHA256') if name in os.environ]
    need(not stale, 'Historical witness mode has been replaced by a fresh host SDK build; unset old host/Apple witness variables')
    need(not os.environ.get('CARGO_TARGET_DIR') and not os.environ.get('CARGO_BUILD_TARGET') and os.environ.get('CARGO_INCREMENTAL') == '0', 'Reuse checkout target with native host and existing CARGO_INCREMENTAL=0')
    root = ROOT; need(root.resolve() == root, 'Canonical checkout')
    tools = {name: Path(shutil.which(name)).resolve() for name in ('ffprobe', 'ffmpeg') if shutil.which(name)}
    need(set(tools) == {'ffprobe', 'ffmpeg'}, 'Existing FFprobe and FFmpeg required')
    tools.update(xcrun=Path('/usr/bin/xcrun'), python=Path(sys.executable).resolve())
    cargo = shutil.which('cargo'); need(cargo is not None, 'Existing Cargo required')
    cargo_command = Path(cargo).absolute(); cargo_entry = entry_fact(cargo_command); tools['cargo'] = cargo_command.resolve()
    env_keys = ('PATH', 'LANG', 'LC_ALL', 'DEVELOPER_DIR', 'SDKROOT', 'CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET', 'CARGO_INCREMENTAL', 'CARGO_BUILD_JOBS', 'RUSTUP_TOOLCHAIN', 'RUSTC', 'RUSTC_WRAPPER', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS')
    def environment():
        need(not any(name.startswith('DYLD_') for name in os.environ), 'DYLD_ loader overrides are not supported')
        need(not any(os.environ.get(name) for name in ('TOOLCHAINS', 'SWIFT_EXEC', 'SWIFT_DRIVER_SWIFT_FRONTEND_EXEC', 'SWIFT_DRIVER_SWIFT_SCAN_EXEC', 'LD_LIBRARY_PATH', 'FFREPORT', 'FFMPEG_DATADIR', 'LADSPA_PATH', 'LV2_PATH', 'FREI0R_PATH')), 'Compiler/decoder plugin or report override refused')
        return {name: os.environ.get(name) for name in env_keys}
    env_before = environment()
    sdk_dirs = ('.cargo', 'crates', 'scripts', 'vendor', 'tests/native')
    explicit = {'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'LICENSE', 'THIRD_PARTY_LICENSES.txt', 'mobile/ios/Sources/CaptureEngine.swift', 'mobile/ios/Tools/CaptureRecordingFixtureCheck.swift', 'mobile/ios/check-capture-recording.sh', 'mobile/ios/check-capture-recording.py', 'mobile/ios/checkpoint_test_processes.py'} | {'mobile/ios/Sources/' + name + '.swift' for name in NAMES}
    def namespace():
        names = set(explicit)
        for name in sdk_dirs:
            directory = root / name
            need(directory.is_dir() and not directory.is_symlink() and directory.resolve() == directory, 'Source namespace: ' + name)
            for path in directory.rglob('*'):
                if path.is_file() or path.is_symlink(): names.add(str(path.relative_to(root)))
                else: need(path.is_dir(), 'Special source input: ' + str(path))
        return sorted(names)
    def source_fact(path):
        if not path.is_symlink(): return file_fact(path)
        node = path.lstat(); literal = os.readlink(path)
        need(path.resolve() == root / 'LICENSE', 'Only checked root-licence source links supported')
        target = file_fact(path.resolve()); after = path.lstat()
        need((node.st_dev, node.st_ino, node.st_mode, node.st_mtime_ns, node.st_ctime_ns) == (after.st_dev, after.st_ino, after.st_mode, after.st_mtime_ns, after.st_ctime_ns) and os.readlink(path) == literal, 'Licence link changed while captured')
        return dict(type='symlink', literal=literal, mode=oct(stat.S_IMODE(node.st_mode)), epoch=[node.st_dev, node.st_ino, node.st_mtime_ns, node.st_ctime_ns], target=target)
    def capture():
        names = namespace(); sources = {name: source_fact(root / name) for name in names}
        observed_tools = {str(path): file_fact(path, system=True) for path in tools.values()}
        need(namespace() == names, 'Source namespace changed during capture')
        need({name: source_fact(root / name) for name in names} == sources, 'Sources changed during complete capture')
        need(entry_fact(cargo_command) == cargo_entry, 'Cargo command entry changed')
        return dict(sources=sources, tools=observed_tools, cargoCommandPath=str(cargo_command), cargoEntry=cargo_entry, environment=environment())
    target = root / 'target'; target.mkdir(exist_ok=True)
    parent = target / 'ios-native-recording'
    parent.mkdir(mode=0o700, exist_ok=True)
    need(parent.is_dir() and not parent.is_symlink() and parent.resolve() == parent and parent.stat().st_uid == os.getuid(), 'Owned canonical evidence parent')
    requested = args.evidence or (Path(os.environ['SAPFLOW_CAPTURE_RECORDING_EVIDENCE']) if os.environ.get('SAPFLOW_CAPTURE_RECORDING_EVIDENCE') else None)
    if requested:
        evidence = requested.absolute()
        need(evidence.parent == parent and evidence.parent.resolve() == parent and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,79}', evidence.name) and not evidence.exists() and not evidence.is_symlink(), 'New direct target/ios-native-recording evidence child required')
        evidence.mkdir(mode=0o700)
    else: evidence = Path(tempfile.mkdtemp(prefix='run-', dir=parent))
    spec = importlib.util.spec_from_file_location('capture_owned', root / 'mobile/ios/checkpoint_test_processes.py')
    owner = importlib.util.module_from_spec(spec); spec.loader.exec_module(owner)
    active = None; launching = retiring = finalising = False; pending = None; unknown_launch = False
    commands = []; groups = []; cleanup_errors = []; scratch = fixture = None; before = after = None
    compiler_inputs = {}; generated_before = {}; source_unchanged = False; failure = None; passed = False; summary = None
    selection = None; now = lambda: datetime.datetime.now(datetime.timezone.utc).isoformat(); started = now()
    def stopped(number, frame):
        nonlocal pending
        pending = pending or number
        if not launching and not retiring and not finalising: raise InterruptedError('Signal ' + str(number))
    previous = {number: signal.signal(number, stopped) for number in (signal.SIGINT, signal.SIGTERM)}
    def metadata_unchanged():
        if before is None: return
        need(entry_fact(cargo_command) == cargo_entry, 'Cargo command entry changed before command')
        if selection: need(entry_fact(Path(selection['compilerArgvPath'])) == selection['compilerEntry'], 'Selected compiler command entry changed')
        need(namespace() == sorted(before['sources']), 'Source namespace changed before command')
        for name, expected in before['sources'].items():
            path = root / name
            if expected['type'] == 'symlink': need(source_fact(path) == expected, 'Licence source identity changed')
            else: need(file_meta(path) == {key: value for key, value in expected.items() if key != 'sha256'}, 'Source identity changed before command: ' + name)
        for name, expected in before['tools'].items(): need(file_meta(Path(name), True) == {key: value for key, value in expected.items() if key != 'sha256'}, 'Tool identity changed')
        for name, expected in compiler_inputs.items(): need(file_meta(Path(name), True) == {key: value for key, value in expected.items() if key != 'sha256'}, 'Selected compiler/SDK metadata changed')
        for name, expected in generated_before.items(): need(file_meta(Path(name)) == {key: value for key, value in expected.items() if key != 'sha256'}, 'Actual generated host/binding input changed')
        need(environment() == env_before, 'Runtime environment changed before command')
    def retire():
        nonlocal active, retiring
        if active is None: return True
        owned = active
        if owned['cleanupAttempted']: return (owned.get('groupEvidence') or {}).get('stopped', False)
        owned['cleanupAttempted'] = True; retiring = True
        try: return owner.stop_owned_group(owned, evidence, owned['label'])
        except BaseException as error: cleanup_errors.append(type(error).__name__ + ': ' + str(error)); return False
        finally:
            retiring = False; groups.append(owned.get('groupEvidence') or dict(stopped=False, unknownReason='No retirement evidence'))
            if (owned.get('groupEvidence') or {}).get('stopped'): active = None
    def invoke(args, name, timeout):
        nonlocal active, launching, unknown_launch
        need(active is None and not unknown_launch and pending is None and not cleanup_errors, 'Previous child/signal/cleanup unresolved')
        metadata_unchanged(); log = evidence / name; owned = None; attempted = False; problem = None; began = time.monotonic()
        try:
            with log.open('xb') as output:
                launching = attempted = True
                try:
                    child_env = {**os.environ, 'TMPDIR': str(scratch / 'tmp'), 'CLANG_MODULE_CACHE_PATH': str(scratch / 'modules'), 'PYTHONDONTWRITEBYTECODE': '1'}
                    if selection: child_env.update(DEVELOPER_DIR=selection['developerPath'], SDKROOT=selection['macOSSDKPath'])
                    child = subprocess.Popen(list(map(str, args)), cwd=root, stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT, start_new_session=True, env=child_env)
                    owned = dict(child=child, cleanupAttempted=False, label='command-' + str(len(commands) + 1)); active = owned
                finally: launching = False
                need(pending is None, 'Deferred launch signal')
                child.wait(timeout=timeout); need(child.returncode == 0, name + ' failed with exit ' + str(child.returncode))
        except BaseException as error: problem = error
        finally:
            launching = False
            if owned is not None: retire()
            elif attempted: unknown_launch = True
            digest = None; log_error = None
            try: digest = sha(log) if log.is_file() else None
            except BaseException as error:
                log_error = type(error).__name__ + ': ' + str(error)
                if problem is None: problem = error
            commands.append(dict(command=list(map(str, args)), exit=owned['child'].returncode if owned else None, elapsedSeconds=time.monotonic()-began, log=name, logSHA256=digest, logReadError=log_error, error=None if problem is None else type(problem).__name__ + ': ' + str(problem), groupStopped=(owned.get('groupEvidence') or {}).get('stopped', False) if owned else False, launchAttempted=attempted, launchOwnershipKnown=owned is not None))
        if problem is not None: raise problem
        need(active is None and not unknown_launch and not cleanup_errors and pending is None, 'Child closure or signal unresolved')
        return log
    try:
        scratch = Path(tempfile.mkdtemp(prefix='sapflow-capture-recording-', dir=target))
        (scratch / 'tmp').mkdir(); (scratch / 'modules').mkdir()
        fixture = scratch / 'generated'; fixture.mkdir(mode=0o700)
        (fixture / '.sapflow-sdk-recorder-fixture').write_text('generated-sdk-recorder-v1\n')
        before = capture()
        sdk_log = invoke(['/usr/bin/xcrun', '--sdk', 'macosx', '--show-sdk-path'], 'sdk-path.log', 30)
        need(0 < sdk_log.stat().st_size <= 16384, 'Bounded selected SDK path')
        reported = Path(sdk_log.read_text().strip()); need(reported.is_absolute() and reported.is_dir(), 'Installed selected macOS SDK')
        sdk = reported.resolve(); contents = next((path for path in sdk.parents if path.name == 'Contents'), None)
        need(contents is not None, 'Selected SDK must belong to full Xcode bundle')
        developer = contents / 'Developer'; compiler = developer / 'Toolchains/XcodeDefault.xctoolchain/usr/bin/swiftc'
        need(compiler.is_file() and compiler.resolve().parent == compiler.parent, 'Selected bundle compiler')
        metadata = [compiler.resolve(), contents / 'Info.plist', sdk / 'SDKSettings.plist', sdk / 'SDKSettings.json']
        compiler_inputs = {str(path): file_fact(path, True) for path in metadata}
        selection = dict(reportedSDKPath=str(reported), macOSSDKPath=str(sdk), developerPath=str(developer), compilerArgvPath=str(compiler), compilerResolvedPath=str(compiler.resolve()), compilerEntry=entry_fact(compiler), cargoCommandPath=str(cargo_command), cargoEntry=cargo_entry)
        invoke([str(cargo_command), 'build', '--locked', '-p', 'sapflow-ffi', '-j', '1'], 'rust-sdk.log', 1800)
        library = target / 'debug/libsapflow_ffi.dylib'
        generated_before = {str(library): file_fact(library)}
        for name, relative in [('Sapflow.swift', 'crates/sapflow-ffi/swift/Sapflow.swift'), ('SapflowFFI.h', 'crates/sapflow-ffi/include/SapflowFFI.h'), ('SapflowFFI.modulemap', 'crates/sapflow-ffi/include/module.modulemap')]:
            shutil.copyfile(root / relative, scratch / name)
            generated_before[str(scratch / name)] = file_fact(scratch / name)
        headers = scratch
        common = [str(compiler), '-sdk', str(sdk), '-swift-version', '5', '-warnings-as-errors', '-parse-as-library', '-j', '1', '-module-cache-path', str(scratch / 'modules'), '-I', str(headers), '-Xcc', '-fmodule-map-file=' + str(headers / 'SapflowFFI.modulemap')]
        names = NAMES
        invoke(common+['-emit-module','-emit-object','-module-name','Sapflow',str(scratch/'Sapflow.swift'),
           '-emit-module-path',str(scratch/'Sapflow.swiftmodule'),'-o',str(scratch/'Sapflow.o')],'sdk-compile.log',150)
        for name in ('Sapflow.swiftmodule', 'Sapflow.o'): generated_before[str(scratch / name)] = file_fact(scratch / name)
        invoke(common+['-I',str(scratch),'-L',str(root/'target/debug'),'-lsapflow_ffi',
           '-Xlinker','-rpath','-Xlinker',str(root/'target/debug')]+[str(root/('mobile/ios/Sources/'+name+'.swift')) for name in names]+
           [str(root/'mobile/ios/Tools/CaptureRecordingFixtureCheck.swift'),str(scratch/'Sapflow.o'),
           '-o',str(scratch/'capture-recording')],'fixture-compile.log',150)
        generated_before[str(scratch / 'capture-recording')] = file_fact(scratch / 'capture-recording')
        result=invoke([str(scratch/'capture-recording'),str(fixture)],'native-run.local.json',45)
        assert result.stat().st_size<=2*1024*1024
        summary=json.loads(result.read_text());validate_matrix(summary);assert summary['passed'] and summary['generatedOnly'] and len(summary['cases'])==5
        expected=['healthy','mixed-zero-raw','publishing-independent-failure','missing-file-EOF','recording-queue-failure']
        assert [c['name'] for c in summary['cases']]==expected and all(c['passed'] is True for c in summary['cases'])
        assert all(c['sdkReceiptState']=='incomplete' and c['appAdmission']=='unknown' and c['actualCheckedCleanup'] and
           c['recordingClosedBeforeTransport'] and c['validatedExactlyOnce'] for c in summary['cases'])
        ffprobe=str(tools['ffprobe'])
        packet_proofs=[]
        for case in summary['cases']:
            if not case['exactPayloadProbeRequired']:continue
            media=fixture_media(fixture,case['name'],case['media'])
            assert media.is_file() and not media.is_symlink() and media.resolve().parent==(fixture/case['name']).resolve()
            packet_log=invoke([ffprobe,'-v','error','-ignore_editlist','1','-show_packets','-show_data_hash','sha256',
              '-show_entries','packet=codec_type,size,data_hash','-of','json',str(media)],case['name']+'-packets.local.json',15)
            assert packet_log.stat().st_size<=2*1024*1024
            packets=json.loads(packet_log.read_text())['packets']
            assert all(p['codec_type']=='audio' for p in packets)
            actual=[{'size':int(p['size']),'sha256':p['data_hash'].removeprefix('SHA256:').lower()} for p in packets]
            assert actual==case['originalPackets'],case['name']+' original native AAC payload changed'
            packet_proofs.append({'name':case['name'],'actualOriginalNativePackets':len(actual),'mediaSHA256':sha(media)})
        need(len(packet_proofs) == 4, 'Four exact-payload probes required')
        validate_commands(commands, 'missing-file-EOF-packets.local.json')
        (evidence/'exact-payload-proofs.local.json').write_text(json.dumps(packet_proofs,indent=2)+'\n')
        expected_producers=['producer-audio','producer-av','producer-publishing-failure','producer-late-native-drop',
           'producer-recording-loss','producer-input-not-closed','producer-wrong-worker','producer-late-encoded-output','producer-late-timing','producer-wrong-ticket']
        producer_cases=summary['producerCases']
        assert [c['name'] for c in producer_cases]==expected_producers and all(c['passed'] is True for c in producer_cases)
        assert all(c['finalised']==(i<3) and c['normalStopCancellationPresent'] and c['producerCapabilitySingleUse'] and
           c['duplicateFinishPreservedBytes'] and c['recordingClosedBeforeTransport'] for i,c in enumerate(producer_cases))
        ffmpeg=str(tools['ffmpeg'])
        producer_proofs=[]
        for case in producer_cases[:3]:
            media=fixture_media(fixture,case['name'],case['file'])
            assert media.is_file() and not media.is_symlink() and media.resolve().parent==(fixture/case['name']).resolve()
            probe=invoke([ffprobe,'-v','error','-count_frames','-show_streams','-of','json',str(media)],case['name']+'-streams.local.json',15)
            streams=json.loads(probe.read_text())['streams'];audio=[x for x in streams if x['codec_type']=='audio'];video=[x for x in streams if x['codec_type']=='video']
            assert len(audio)==1 and audio[0]['codec_name']=='aac' and int(audio[0]['sample_rate'])==48000 and int(audio[0]['nb_read_frames'])>0
            assert len(video)==int(case['hasVideo'])
            if video:assert video[0]['codec_name']=='h264' and int(video[0]['nb_read_frames'])==10
            invoke([ffmpeg,'-nostdin','-v','error','-xerror','-threads','1','-i',str(media),'-map','0','-f','null','-'],case['name']+'-decode.local.log',20)
            producer_proofs.append({'name':case['name'],'mediaSHA256':sha(media),'bytes':media.stat().st_size,'streams':streams,'decodedWithoutErrors':True})
        validate_commands(commands, 'producer-publishing-failure-decode.local.log')
        (evidence/'producer-finalisation-proofs.local.json').write_text(json.dumps(producer_proofs,indent=2)+'\n')
        expected_legacy=['producer-audio','producer-av','producer-recording-loss','producer-late-native-drop','producer-input-not-closed','producer-wrong-worker','producer-wrong-ticket']
        legacy_cases=summary['legacyProducerCases']
        need([c['name']for c in legacy_cases]==expected_legacy and all(c['passed']is True for c in legacy_cases),'Exact seven legacy controls')
        legacy_proofs=[]
        for index,case in enumerate(legacy_cases):
            need(case['backend']=='legacy'and case['finalised']==(index<2)and case['normalStopCancellationPresent']and case['producerCapabilitySingleUse']and case['duplicateFinishPreservedBytes']and case['recordingClosedBeforeTransport'],'Legacy outcome '+case['name'])
            media=fixture_media(fixture,'legacy-'+case['name'],case['file'])
            need(media.is_file()and not media.is_symlink()and media.resolve().parent==(fixture/('legacy-'+case['name'])).resolve()and media.stat().st_size>0,'Actual legacy file/prefix')
            receipt=read(media.with_suffix('.receipt.json'))
            need(receipt['state']==('finalised'if index<2 else'incomplete'),'Actual legacy receipt state')
            proof=dict(name=case['name'],mediaSHA256=sha(media),bytes=media.stat().st_size,receiptState=receipt['state'],decodedWithoutErrors=False)
            if index<2:
                probe=invoke([ffprobe,'-v','error','-count_frames','-show_streams','-of','json',str(media)],'legacy-'+case['name']+'-streams.local.json',15)
                streams=read(probe)['streams'];audio=[x for x in streams if x['codec_type']=='audio'];video=[x for x in streams if x['codec_type']=='video']
                need(len(audio)==1 and audio[0]['codec_name']=='aac'and int(audio[0]['sample_rate'])==48000 and int(audio[0]['nb_read_frames'])>0,'Legacy positive audio')
                need(len(video)==int(case['hasVideo']),'Legacy positive track topology')
                if video:need(video[0]['codec_name']=='h264'and int(video[0]['nb_read_frames'])==10,'Legacy positive video')
                invoke([ffmpeg,'-nostdin','-v','error','-xerror','-threads','1','-i',str(media),'-map','0','-f','null','-'],'legacy-'+case['name']+'-decode.local.log',20)
                proof.update(streams=streams,decodedWithoutErrors=True)
            legacy_proofs.append(proof)
        validate_commands(commands)
        (evidence/'legacy-producer-finalisation-proofs.local.json').write_text(json.dumps(legacy_proofs,indent=2)+'\n')
        passed = True
    except BaseException as error: failure = type(error).__name__ + ': ' + str(error)
    finally:
        finalising = True
        if active is not None and not active['cleanupAttempted']: retire()
        stopped_all = active is None and not unknown_launch and not cleanup_errors and all(row['groupStopped'] for row in commands)
        generated_after = None; compiler_after = None
        try:
            metadata_unchanged()
            after = capture(); source_unchanged = before is not None and before == after
            generated_after = {name: file_fact(Path(name)) for name in generated_before}
            compiler_after = {name: file_fact(Path(name), True) for name in compiler_inputs}
            need(generated_after == generated_before and compiler_after == compiler_inputs, 'Used generated/runtime/toolchain files changed')
        except BaseException as error:
            source_unchanged = False; failure = failure or type(error).__name__ + ': ' + str(error)
        if scratch is not None and stopped_all:
            try:
                need(scratch.parent == target and scratch.name.startswith('sapflow-capture-recording-') and not scratch.is_symlink(), 'Owned scratch boundary')
                shutil.rmtree(scratch)
            except BaseException as error: cleanup_errors.append(type(error).__name__ + ': ' + str(error))
        passed = passed and source_unchanged and stopped_all and not cleanup_errors and pending is None and len(commands) == len(COMMAND_LOGS)
        result = dict(schema='ios-current-host-native-recording-outcome-v1', passed=passed, startedUTC=started, endedUTC=now(), failure=failure, sourceUnchanged=source_unchanged, inputsBefore=before, inputsAfter=after, compileSelection=selection, compilerInputsBefore=compiler_inputs, compilerInputsAfter=compiler_after, generatedInputsBefore=generated_before, generatedInputsAfter=generated_after, commands=commands, terminalProcessGroups=groups, childrenStopped=stopped_all, cleanupErrors=cleanup_errors, unconfirmedLaunchAttempt=unknown_launch, activeOwnedPID=active['child'].pid if active else None, pendingDeferredSignal=pending, scratchRemoved=scratch is None or not scratch.exists(), generatedFilesRetained=scratch is not None and scratch.exists(), expectedCommands=len(COMMAND_LOGS), expectedStoredCases=5, expectedSDKProducerCases=10, expectedLegacyProducerCases=7, expectedSDKPositiveDecodes=3, expectedLegacyPositiveDecodes=2, generatedOnly=True, hostArchitecture=platform.machine(), deviceActions=False, productionNetworkActions=False, dependencyAcquisitionPossible=True, physicalDeviceQualified=False, presentationQualified=False, recoveryQualified=False, aggregateTwoSecondQualified=False, releaseQualified=False, scope='Fresh current-host SDK and generated native encoder/ingress/SDK+legacy recorder matrix. Legacy default unchanged. Empty stopped session is not camera capture, paired programme H/T/presentation/recovery/sustained2s/performance/release proof. Native clock/duration diagnostics are not repaired or substituted.')
        mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
        try:
            result['terminalPublication'] = dict(protocol='MASKED_PENDING_INSPECTION_AND_ATOMIC_PUBLICATION_V1', committed=True)
            temporary = evidence / 'receipt.precommit.json'
            with temporary.open('x') as output: output.write(json.dumps(result, indent=2) + '\n')
            observed = pending or next(iter(sorted(signal.sigpending() & {signal.SIGINT, signal.SIGTERM})), None)
            if observed is not None:
                result.update(passed=False, pendingDeferredSignal=observed, failure=result['failure'] or 'Deferred signal ' + str(observed))
                temporary.write_text(json.dumps(result, indent=2) + '\n')
            os.replace(temporary, evidence / 'receipt.local.json')
        except BaseException as publication_error:
            primary = result['failure'] or type(publication_error).__name__ + ': ' + str(publication_error)
            result.update(passed=False, failure=primary)
            result['terminalPublication'].update(committed=False, publicationError=type(publication_error).__name__ + ': ' + str(publication_error))
            try:
                failed = evidence / 'receipt.failed-precommit.json'
                with failed.open('x') as output: output.write(json.dumps(result, indent=2) + '\n')
                os.replace(failed, evidence / 'receipt.local.json')
            except BaseException as secondary: print(json.dumps(dict(failure=primary, secondaryPublicationError=str(secondary))), file=sys.stderr)
            raise RuntimeError(primary) from publication_error
        finally:
            for number, handler in previous.items(): signal.signal(number, handler)
            signal.pthread_sigmask(signal.SIG_SETMASK, mask)
        print(json.dumps(dict(passed=result['passed'], failure=result['failure'], receipt=str(evidence / 'receipt.local.json'))), flush=True)
    if not result['passed']: raise SystemExit(1)

if __name__ == '__main__': main()
