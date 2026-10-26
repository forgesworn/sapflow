#!/bin/bash
# Root-only actual generated native AAC receipt gate. No capture/device/network.
# Explicit reviewed runtime/binding witnesses; never Cargo/package regeneration.
set -euo pipefail
if [ "$#" -ne 0 ]; then echo 'Usage: check-native-audio-receipts.sh (generated input only)' >&2; exit 2; fi
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
: "${SAPFLOW_AUDIO_RECEIPT_HOST_WITNESS:?Set actual root-owned host runtime receipt path}"
: "${SAPFLOW_AUDIO_RECEIPT_HOST_WITNESS_SHA256:?Set independently reviewed host receipt SHA256}"
: "${SAPFLOW_AUDIO_RECEIPT_APPLE_RECEIPT:?Set actual qualified Apple receipt path}"
: "${SAPFLOW_AUDIO_RECEIPT_APPLE_RECEIPT_SHA256:?Set independently reviewed Apple receipt SHA256}"
: "${SAPFLOW_AUDIO_RECEIPT_EVIDENCE:?Set NEW retained evidence directory under target}"
python3 - "$sapflow_root" <<'PY'
from pathlib import Path
import datetime,hashlib,json,os,platform,shutil,signal,subprocess,sys,tempfile,time
root=Path(sys.argv[1]).resolve()
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
evidence=Path(os.environ['SAPFLOW_AUDIO_RECEIPT_EVIDENCE']).absolute()
assert evidence.is_relative_to(root/'target') and evidence.parent.is_dir() and not evidence.exists()
assert evidence.parent.resolve()==evidence.parent and not evidence.is_symlink()
evidence.mkdir(mode=0o700)
# Only the currently owned group may be signalled. Completed groups retain
# scalar proof only and are never queried/re-signalled through historical PIDs.
active=None;launch_in_progress=False;pending_signal=None;commands=[];cleanup_errors=[];scratch=None;inputs={};proofs={};head_before=None;failure=None;passed=False;summary=None
now=lambda:datetime.datetime.now(datetime.timezone.utc).isoformat()
started=now()
def stopped(number,frame):
    global pending_signal
    if launch_in_progress:
        # Popen may have created an actual child before returning its handle.
        # Defer raising until that exact handle is published to active ownership.
        if pending_signal is None:pending_signal=number
        return
    raise RuntimeError('Interrupted by signal '+str(number))
for number in [signal.SIGTERM,signal.SIGINT]:signal.signal(number,stopped)
def alive(pid):
    try:os.killpg(pid,0);return True
    except ProcessLookupError:return False
def stop_group(child):
    if alive(child.pid):
        os.killpg(child.pid,signal.SIGTERM);deadline=time.monotonic()+2
        while alive(child.pid) and time.monotonic()<deadline:
            child.poll();time.sleep(0.05)
        if alive(child.pid):os.killpg(child.pid,signal.SIGKILL)
    child.wait(timeout=5)
    deadline=time.monotonic()+2
    while alive(child.pid) and time.monotonic()<deadline:time.sleep(0.05)
    if alive(child.pid):raise RuntimeError('Owned child group still present: '+str(child.pid))
def retire_active():
    global active
    if active is None:return True
    owned=active
    # A failed/ambiguous cleanup is retained, never retried by numeric PGID.
    if owned['cleanupAttempted']:return owned['groupStopped']
    owned['cleanupAttempted']=True
    try:stop_group(owned['child'])
    except BaseException as error:
        owned['cleanupError']=type(error).__name__+': '+str(error)
        cleanup_errors.append(owned['cleanupError']);return False
    owned['groupStopped']=True
    active=None
    return True

def invoke(args,name,timeout):
    global active,launch_in_progress,pending_signal
    assert active is None,'Previous owned group cleanup remains unconfirmed'
    log=evidence/name;started=time.monotonic();owned=None;problem=None
    try:
        with log.open('xb') as output:
            launch_in_progress=True
            try:
                child=subprocess.Popen(args,cwd=root,stdout=output,stderr=subprocess.STDOUT,start_new_session=True,
                    env={**os.environ,'TMPDIR':str(scratch/'tmp'),'CLANG_MODULE_CACHE_PATH':str(scratch/'modules')})
                owned={'child':child,'cleanupAttempted':False,'groupStopped':False,'cleanupError':None}
                active=owned
            finally:
                # Success publishes ownership BEFORE re-enabling raising; a
                # launch exception also clears the guard without hiding failure.
                launch_in_progress=False
            if pending_signal is not None:
                number=pending_signal;pending_signal=None
                raise RuntimeError('Interrupted by deferred signal '+str(number))
            code=child.wait(timeout=timeout)
        if code!=0:raise RuntimeError(name+' failed with exit '+str(code))
    except BaseException as error:problem=error
    finally:
        launch_in_progress=False
        if owned is not None:
            retire_active() # Exactly one cleanup attempt while this group is owned.
            commands.append({'command':args,'exit':owned['child'].returncode,'elapsedSeconds':time.monotonic()-started,
                'log':name,'logSHA256':sha(log),'groupPID':owned['child'].pid,
                'groupStopped':owned['groupStopped'],'cleanupAttempted':owned['cleanupAttempted'],
                'cleanupError':owned['cleanupError']})
    if problem is not None:raise problem
    if owned is not None and not owned['groupStopped']:raise RuntimeError(name+' owned group cleanup unconfirmed')
    return log

try:
    assert sys.platform=='darwin' and platform.machine()=='arm64','Qualified ARM64 macOS host only'
    assert not os.environ.get('CARGO_TARGET_DIR') and os.environ.get('CARGO_INCREMENTAL')=='0'
    host=Path(os.environ['SAPFLOW_AUDIO_RECEIPT_HOST_WITNESS']).resolve()
    apple=Path(os.environ['SAPFLOW_AUDIO_RECEIPT_APPLE_RECEIPT']).resolve()
    for path,key in [(host,'SAPFLOW_AUDIO_RECEIPT_HOST_WITNESS_SHA256'),(apple,'SAPFLOW_AUDIO_RECEIPT_APPLE_RECEIPT_SHA256')]:
        assert path.is_file() and not path.is_symlink() and path.stat().st_size<=2*1024*1024 and sha(path)==os.environ[key]
        proofs[str(path)]=sha(path)
    witness=json.loads(host.read_text());package=json.loads(apple.read_text())
    assert witness['passed'] and witness['sourceUnchanged'] and witness['childrenStopped'] and witness['exit']==0
    assert package['passed'] and package['sourceUnchanged'] and package['childrenStopped']
    assert isinstance(witness['inputs'],dict) and 1<=len(witness['inputs'])<=512
    # New source files must not escape an older witness's known-file map.
    build_paths=sorted(set(x for x in [*root.glob('crates/**/src/**/*.rs'),*root.glob('crates/*/Cargo.toml'),
        *root.glob('crates/**/build.rs'),*root.glob('.cargo/*'),
        root/'THIRD_PARTY_LICENSES.txt',root/'Cargo.toml',root/'Cargo.lock',root/'.cargo/config.toml',
        root/'scripts/build-native.mjs',root/'scripts/third-party-licences.py',
        root/'rust-toolchain.toml',root/'LICENSE',*root.glob('tests/native/*')] if x.is_file()))
    assert {str(x.relative_to(root)) for x in build_paths}==set(witness['inputs'])
    inputs.update(witness['inputs']);inputs.update(witness['outputs'])
    assert 'target/debug/libsapflow_ffi.dylib' in witness['outputs']
    selected=['target/native-packages/apple/Sources/Sapflow/Sapflow.swift',
      'target/native-packages/apple/SapflowFFI.xcframework/macos-arm64/Headers/SapflowFFI.h',
      'target/native-packages/apple/SapflowFFI.xcframework/macos-arm64/Headers/module.modulemap']
    for name in selected:inputs[name]=package['outputHashes'][name]
    for name,value in inputs.items():
        path=root/name;assert path.is_file() and not path.is_symlink() and sha(path)==value,'Stale source/runtime: '+name
    used=['mobile/ios/Sources/NativeAudioEncoder.swift','mobile/ios/Sources/AudioRecordingTiming.swift',
      'mobile/ios/Sources/EncodedPipeline.swift','mobile/ios/Sources/CaptureEngine.swift','mobile/ios/Sources/CompressedRecorder.swift',
      'mobile/ios/Tools/NativeAudioReceiptFixtureCheck.swift','mobile/ios/check-native-audio-receipts.sh']
    for name in used:inputs[name]=sha(root/name)
    head_before=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
    before={'head':head_before,'allInputs':inputs,'proofs':proofs,'hostRuntimeSHA256':inputs['target/debug/libsapflow_ffi.dylib'],
        'scope':'Only exact native encoder/ledger and actual current SDK; CaptureEngine/Recorder remain unchanged default context.'}
    (evidence/'before.local.json').write_text(json.dumps(before,indent=2)+'\n')
    scratch=Path(tempfile.mkdtemp(prefix='sapflow-native-audio-receipts-',dir=root/'target'))
    (scratch/'tmp').mkdir();(scratch/'modules').mkdir()
    # Exact production declaration extraction only: avoids importing unrelated
    # capture/network implementations. No synthetic codec/encoder/FFI substitute.
    original=(root/'mobile/ios/Sources/EncodedPipeline.swift').read_text()
    start=original.index('struct CaptureFailure:');end=original.index('/// A destination must',start)
    declarations='import AVFoundation\n'+original[start:end]
    (scratch/'EncodedTypes.swift').write_text(declarations)
    (evidence/'declaration-extraction.local.json').write_text(json.dumps({'source':'mobile/ios/Sources/EncodedPipeline.swift',
       'sourceSHA256':inputs['mobile/ios/Sources/EncodedPipeline.swift'],'exactStart':'struct CaptureFailure:',
       'exactEnd':'/// A destination must','extractedSHA256':hashlib.sha256(declarations.encode()).hexdigest()},indent=2)+'\n')
    headers=root/'target/native-packages/apple/SapflowFFI.xcframework/macos-arm64/Headers'
    common=['xcrun','--sdk','macosx','swiftc','-swift-version','5','-warnings-as-errors','-parse-as-library',
       '-j','1','-module-cache-path',str(scratch/'modules'),'-I',str(headers),'-Xcc','-fmodule-map-file='+str(headers/'module.modulemap')]
    invoke(common+['-emit-module','-emit-object','-module-name','Sapflow',str(root/selected[0]),
       '-emit-module-path',str(scratch/'Sapflow.swiftmodule'),'-o',str(scratch/'Sapflow.o')],'sdk-compile.log',150)
    invoke(common+['-I',str(scratch),'-L',str(root/'target/debug'),'-lsapflow_ffi',
       '-Xlinker','-rpath','-Xlinker',str(root/'target/debug'),str(scratch/'EncodedTypes.swift'),
       str(root/'mobile/ios/Sources/AudioRecordingTiming.swift'),str(root/'mobile/ios/Sources/NativeAudioEncoder.swift'),
       str(root/'mobile/ios/Tools/NativeAudioReceiptFixtureCheck.swift'),str(scratch/'Sapflow.o'),
       '-o',str(scratch/'native-audio-receipts')],'fixture-compile.log',150)
    result=invoke([str(scratch/'native-audio-receipts')],'native-run.local.json',30)
    assert result.stat().st_size<=2*1024*1024
    summary=json.loads(result.read_text());assert summary['passed'] and summary['generatedOnly'] and len(summary['cases'])==14
    expected=['raw-48000','raw-44100','initial-zero-resume-same-generation','initial-zero-only','three-generations',
       'empty-uncreated','empty-initial-zero','empty-trailing-converter','sticky-time-failure-no-EOF','sticky-time-failure-keeps-prefix','invalid-target-no-EOF',
       'default-legacy-raw','default-legacy-initial-resume','default-legacy-three-generations']
    assert [c['name'] for c in summary['cases']]==expected and all(c['passed'] is True for c in summary['cases'])
    passed=True
except BaseException as error:failure=type(error).__name__+': '+str(error)
finally:
    # Handle only an active group which has not already consumed its cleanup
    # attempt (for example interruption outside invoke). Never signal old PIDs.
    if active is not None and not active['cleanupAttempted']:retire_active()
    groups_stopped=active is None and all(c['groupStopped'] for c in commands)
    if scratch is not None:
        if groups_stopped:
            try:
                assert scratch.parent==root/'target' and scratch.name.startswith('sapflow-native-audio-receipts-') and not scratch.is_symlink()
                shutil.rmtree(scratch)
            except BaseException as error:cleanup_errors.append(type(error).__name__+': '+str(error))
        else:cleanup_errors.append('Owned group cleanup unconfirmed; preserving task scratch: '+str(scratch))
    unchanged=bool(inputs) and all((root/n).is_file() and not(root/n).is_symlink() and sha(root/n)==v for n,v in inputs.items())
    proofs_same=bool(proofs) and all(Path(n).is_file() and sha(Path(n))==v for n,v in proofs.items())
    head_after=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
    clean=not cleanup_errors and groups_stopped and (scratch is None or not scratch.exists())
    receipt={'passed':passed and unchanged and proofs_same and head_before==head_after and clean,'startedUTC':started,'endedUTC':now(),
       'headBefore':head_before,'headAfter':head_after,'sourceUnchanged':unchanged,'proofsUnchanged':proofs_same,
       'commands':commands,'failure':failure,'cleanupErrors':cleanup_errors,'childrenStopped':groups_stopped,
       'unconfirmedOwnedGroupPID':active['child'].pid if active is not None else None,
       'pendingDeferredSignal':pending_signal,'launchGuardCleared':not launch_in_progress,
       'scratchPreservedForUnconfirmedCleanup':scratch is not None and not groups_stopped and scratch.exists(),
       'scratchRemoved':scratch is None or not scratch.exists(),'expectedCases':14,'actualCases':len(summary['cases']) if summary else None,
       'mediaFilesCreated':False,'deviceActions':False,'networkActions':False,
       'scope':'Actual generated PCM/native AAC full receipt callback and real SDK acceptance gate. No native decode/presentation, app recorder adoption, file durability/recovery, capture pacing or physical acceptance.'}
    (evidence/'receipt.local.json').write_text(json.dumps(receipt,indent=2)+'\n');print(json.dumps(receipt),flush=True)
if not receipt['passed']:raise SystemExit(1)
PY
