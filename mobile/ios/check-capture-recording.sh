#!/bin/bash
# Root-only generated native AAC -> actual ingress/SDK recording storage gate.
# Legacy remains default; no capture/device/network/producer-completeness proof.
# Explicit reviewed runtime/binding witnesses; never Cargo/package regeneration.
set -euo pipefail
if [ "$#" -ne 0 ]; then echo 'Usage: check-capture-recording.sh (generated input only)' >&2; exit 2; fi
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
: "${SAPFLOW_CAPTURE_RECORDING_HOST_WITNESS:?Set actual root-owned host runtime receipt path}"
: "${SAPFLOW_CAPTURE_RECORDING_HOST_WITNESS_SHA256:?Set independently reviewed host receipt SHA256}"
: "${SAPFLOW_CAPTURE_RECORDING_APPLE_RECEIPT:?Set actual qualified Apple receipt path}"
: "${SAPFLOW_CAPTURE_RECORDING_APPLE_RECEIPT_SHA256:?Set independently reviewed Apple receipt SHA256}"
: "${SAPFLOW_CAPTURE_RECORDING_EVIDENCE:?Set NEW retained evidence directory under target}"
python3 - "$sapflow_root" <<'PY'
from pathlib import Path
import datetime,hashlib,json,os,platform,shutil,signal,subprocess,sys,tempfile,time
root=Path(sys.argv[1]).resolve()
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
evidence=Path(os.environ['SAPFLOW_CAPTURE_RECORDING_EVIDENCE']).absolute()
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
    host=Path(os.environ['SAPFLOW_CAPTURE_RECORDING_HOST_WITNESS']).resolve()
    apple=Path(os.environ['SAPFLOW_CAPTURE_RECORDING_APPLE_RECEIPT']).resolve()
    for path,key in [(host,'SAPFLOW_CAPTURE_RECORDING_HOST_WITNESS_SHA256'),(apple,'SAPFLOW_CAPTURE_RECORDING_APPLE_RECEIPT_SHA256')]:
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
    names=['CaptureRecordingProducer','HardwareVideoEncoder','VideoEncodingPolicy','PreparedVideoSlate','AudioRecordingTiming','AudioShowLifecycle','NativeAudioEncoder','CompressedRecorder',
      'QuickTimeAudioPriming','QuickTimeAudioPrimingFile','QuickTimeMovieLayout','RecordingCatalogue','RecordingStorage',
      'RecordingCaptureAdmission','AudioInputPolicy','CameraControlPolicy','CaptureAdmission','CaptureProfile','CaptureOrientation',
      'PublishingTelemetry','EncodedWorkBudget','EncodedPipeline','EncodedIngressWorker','CaptureRecording','RecordingCheckpointOwner']
    used=['mobile/ios/Sources/'+name+'.swift' for name in names]
    used+=['mobile/ios/Sources/CaptureEngine.swift','mobile/ios/Tools/CaptureRecordingFixtureCheck.swift','mobile/ios/check-capture-recording.sh']
    for name in used:inputs[name]=sha(root/name)
    head_before=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
    before={'head':head_before,'allInputs':inputs,'proofs':proofs,'hostRuntimeSHA256':inputs['target/debug/libsapflow_ffi.dylib'],
        'scope':'Actual native encoder, ingress, SDK adapter and FS; CaptureEngine defaultlegacy is source-bound context only, not actual capture proof.'}
    (evidence/'before.local.json').write_text(json.dumps(before,indent=2)+'\n')
    scratch=Path(tempfile.mkdtemp(prefix='sapflow-capture-recording-',dir=root/'target'))
    (scratch/'tmp').mkdir();(scratch/'modules').mkdir()
    fixture=scratch/'generated';fixture.mkdir()
    (fixture/'.sapflow-sdk-recorder-fixture').write_text('generated-sdk-recorder-v1\n')
    headers=root/'target/native-packages/apple/SapflowFFI.xcframework/macos-arm64/Headers'
    common=['xcrun','--sdk','macosx','swiftc','-swift-version','5','-warnings-as-errors','-parse-as-library',
       '-j','1','-module-cache-path',str(scratch/'modules'),'-I',str(headers),'-Xcc','-fmodule-map-file='+str(headers/'module.modulemap')]
    invoke(common+['-emit-module','-emit-object','-module-name','Sapflow',str(root/selected[0]),
       '-emit-module-path',str(scratch/'Sapflow.swiftmodule'),'-o',str(scratch/'Sapflow.o')],'sdk-compile.log',150)
    invoke(common+['-I',str(scratch),'-L',str(root/'target/debug'),'-lsapflow_ffi',
       '-Xlinker','-rpath','-Xlinker',str(root/'target/debug')]+[str(root/('mobile/ios/Sources/'+name+'.swift')) for name in names]+
       [str(root/'mobile/ios/Tools/CaptureRecordingFixtureCheck.swift'),str(scratch/'Sapflow.o'),
       '-o',str(scratch/'capture-recording')],'fixture-compile.log',150)
    result=invoke([str(scratch/'capture-recording'),str(fixture)],'native-run.local.json',45)
    assert result.stat().st_size<=2*1024*1024
    summary=json.loads(result.read_text());assert summary['passed'] and summary['generatedOnly'] and len(summary['cases'])==5
    expected=['healthy','mixed-zero-raw','publishing-independent-failure','missing-file-EOF','recording-queue-failure']
    assert [c['name'] for c in summary['cases']]==expected and all(c['passed'] is True for c in summary['cases'])
    assert all(c['sdkReceiptState']=='incomplete' and c['appAdmission']=='unknown' and c['actualCheckedCleanup'] and
       c['recordingClosedBeforeTransport'] and c['validatedExactlyOnce'] for c in summary['cases'])
    ffprobe=shutil.which('ffprobe');assert ffprobe,'Existing packet-hash oracle ffprobe is required'
    packet_proofs=[]
    for case in summary['cases']:
        if not case['exactPayloadProbeRequired']:continue
        media=fixture/case['name']/case['media']
        assert media.is_file() and not media.is_symlink() and media.resolve().parent==(fixture/case['name']).resolve()
        packet_log=invoke([ffprobe,'-v','error','-ignore_editlist','1','-show_packets','-show_data_hash','sha256',
          '-show_entries','packet=codec_type,size,data_hash','-of','json',str(media)],case['name']+'-packets.local.json',15)
        assert packet_log.stat().st_size<=2*1024*1024
        packets=json.loads(packet_log.read_text())['packets']
        assert all(p['codec_type']=='audio' for p in packets)
        actual=[{'size':int(p['size']),'sha256':p['data_hash'].removeprefix('SHA256:').lower()} for p in packets]
        assert actual==case['originalPackets'],case['name']+' original native AAC payload changed'
        packet_proofs.append({'name':case['name'],'actualOriginalNativePackets':len(actual),'mediaSHA256':sha(media)})
    assert len(packet_proofs)==4 and len(commands)==7,'Expected three native commands and four exact-payload probes'
    (evidence/'exact-payload-proofs.local.json').write_text(json.dumps(packet_proofs,indent=2)+'\n')
    expected_producers=['producer-audio','producer-av','producer-publishing-failure','producer-late-native-drop',
       'producer-recording-loss','producer-input-not-closed','producer-wrong-worker','producer-late-encoded-output','producer-late-timing','producer-wrong-ticket']
    producer_cases=summary['producerCases']
    assert [c['name'] for c in producer_cases]==expected_producers and all(c['passed'] is True for c in producer_cases)
    assert all(c['finalised']==(i<3) and c['normalStopCancellationPresent'] and c['producerCapabilitySingleUse'] and
       c['duplicateFinishPreservedBytes'] and c['recordingClosedBeforeTransport'] for i,c in enumerate(producer_cases))
    ffmpeg=shutil.which('ffmpeg');assert ffmpeg,'Existing native decoder oracle ffmpeg is required'
    producer_proofs=[]
    for case in producer_cases[:3]:
        media=fixture/case['name']/case['file']
        assert media.is_file() and not media.is_symlink() and media.resolve().parent==(fixture/case['name']).resolve()
        probe=invoke([ffprobe,'-v','error','-count_frames','-show_streams','-of','json',str(media)],case['name']+'-streams.local.json',15)
        streams=json.loads(probe.read_text())['streams'];audio=[x for x in streams if x['codec_type']=='audio'];video=[x for x in streams if x['codec_type']=='video']
        assert len(audio)==1 and audio[0]['codec_name']=='aac' and int(audio[0]['sample_rate'])==48000 and int(audio[0]['nb_read_frames'])>0
        assert len(video)==int(case['hasVideo'])
        if video:assert video[0]['codec_name']=='h264' and int(video[0]['nb_read_frames'])==10
        invoke([ffmpeg,'-nostdin','-v','error','-xerror','-threads','1','-i',str(media),'-map','0','-f','null','-'],case['name']+'-decode.local.log',20)
        producer_proofs.append({'name':case['name'],'mediaSHA256':sha(media),'bytes':media.stat().st_size,'streams':streams,'decodedWithoutErrors':True})
    assert len(commands)==13
    (evidence/'producer-finalisation-proofs.local.json').write_text(json.dumps(producer_proofs,indent=2)+'\n')
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
                assert scratch.parent==root/'target' and scratch.name.startswith('sapflow-capture-recording-') and not scratch.is_symlink()
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
       'scratchRemoved':scratch is None or not scratch.exists(),'expectedCommands':13,'expectedCases':5,'expectedProducerCases':10,'actualCases':len(summary['cases']) if summary else None,
       'mediaFilesCreated':True,'generatedOnly':True,'deviceActions':False,'networkActions':False,
       'scope':'Actual generated native AAC through original ingress validated sample to SDK adapter/FS; original5 stored-input controls incomplete. Separate10 concrete native producer cases include3 checked finalised files with actual decoder success; same native close method as CaptureEngine with empty stopped session. No new presentation equivalence, camera/device, default switch, crash durability/recovery, aggregate2s or physical acceptance.'}
    (evidence/'receipt.local.json').write_text(json.dumps(receipt,indent=2)+'\n');print(json.dumps(receipt),flush=True)
if not receipt['passed']:raise SystemExit(1)
PY
