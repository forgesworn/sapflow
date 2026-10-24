#!/bin/bash
# Generated native AAC/queue/recording proof. No capture, socket or input media.
set -euo pipefail
if [ "$#" -ne 0 ]; then echo 'Usage: check-output-ownership.sh (generated input only)' >&2; exit 2; fi
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
umask 077
mkdir -p "$sapflow_root/target/ios-output-ownership-fixture"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-output-ownership-XXXXXX")"
trap 'rm -rf -- "$sapflow_scratch"' EXIT
printf 'generated-output-ownership-v1\n' > "$sapflow_scratch/.sapflow-output-ownership-fixture"
python3 - "$sapflow_root" "$sapflow_scratch" <<'PY'
from pathlib import Path
import array,datetime,hashlib,json,math,os,shutil,signal,subprocess,sys,tarfile,uuid
root=Path(sys.argv[1]);scratch=Path(sys.argv[2]);base=root/'target/ios-output-ownership-fixture'
run=base/(datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ-')+uuid.uuid4().hex[:8]);run.mkdir(mode=0o700)
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
names=['CaptureRecordingProducer','HardwareVideoEncoder','VideoEncodingPolicy','PreparedVideoSlate','AudioRecordingTiming','AudioShowLifecycle','NativeAudioEncoder','CompressedRecorder','QuickTimeAudioPriming','QuickTimeAudioPrimingFile','QuickTimeMovieLayout','RecordingCatalogue','RecordingStorage','RecordingCaptureAdmission','AudioInputPolicy','CameraControlPolicy','CaptureAdmission','CaptureProfile','CaptureOrientation','PublishingTelemetry','EncodedWorkBudget','EncodedPipeline','EncodedIngressWorker','CaptureRecording','RecordingCheckpointOwner']
sources=[root/('mobile/ios/Sources/'+n+'.swift') for n in names]
sources += [root/'mobile/ios/Tools/StandbyOutputOwnershipCheck.swift',root/'mobile/ios/Tools/AudioOnlyFixtureCheck.swift',root/'mobile/ios/check-output-ownership.sh']
package=root/'target/native-packages/apple';packages=sorted(p for p in package.rglob('*') if p.is_file())
assert packages and len(packages)<=64 and not any(p.is_symlink() for p in packages)
inputs={str(p.relative_to(root)):sha(p) for p in sources+packages}
(run/'input-hashes.local.json').write_text(json.dumps(inputs,indent=2)+'\n')
children=[];commands=[];passed=False;failure=None;proofs=[];old=None;summary=None;reference_pcm=None;old_publish=None;old_publish_overlay=None

def stopped(signum,frame):raise RuntimeError('Generated ownership proof interrupted '+str(signum))
for sig in [signal.SIGTERM,signal.SIGINT]:signal.signal(sig,stopped)
def invoke(args,name,timeout=30,accepted=(0,),extra=None):
    log=run/name
    with log.open('wb') as output:
        child=subprocess.Popen(args,stdout=output,stderr=subprocess.STDOUT,start_new_session=True,
            env=dict(os.environ,CLANG_MODULE_CACHE_PATH=str(root/'target/ios-module-cache'),**(extra or {})))
        children.append(child)
        try:code=child.wait(timeout=timeout)
        except BaseException:
            if child.poll() is None:
                os.killpg(child.pid,signal.SIGTERM)
                try:child.wait(timeout=3)
                except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
            raise
    commands.append({'command':args,'exit':code,'log':name,'logSHA256':sha(log)})
    if code not in accepted:raise RuntimeError(name+' failed with exit '+str(code))
    return log
try:
    target=scratch/'Sources/OutputOwnershipCheck';target.mkdir(parents=True)
    for source in sources[:-1]:
        contents=source.read_bytes()
        if source.name=='AudioOnlyFixtureCheck.swift':
            assert contents.count(b'\n@main\n')==1
            (target/'FixtureReaders.swift').write_bytes(contents.replace(b'\n@main\n',b'\n',1))
        else:(target/source.name).write_bytes(contents)
    assert inputs=={str(p.relative_to(root)):sha(p) for p in sources+packages},'Input changed during source copy'
    with tarfile.open(run/'source-inputs.tar.gz','w:gz') as archive:
        for source in sorted(target.iterdir()):archive.add(source,arcname='Sources/OutputOwnershipCheck/'+source.name,recursive=False)
    (scratch/'Package.swift').write_text('''// swift-tools-version: 5.9
import PackageDescription
let package = Package(name: "OutputOwnershipCheck", platforms: [.macOS(.v12)], dependencies: [.package(name: "Sapflow", path: PACKAGE)], targets: [.executableTarget(name: "OutputOwnershipCheck", dependencies: [.product(name: "Sapflow", package: "Sapflow")], swiftSettings: [.unsafeFlags(["-warnings-as-errors"])])])
'''.replace('PACKAGE',json.dumps(str(package))))
    invoke(['swift','build','--package-path',str(scratch),'--scratch-path',str(scratch/'build'),'--jobs','2','--disable-sandbox','--disable-automatic-resolution'],'build.local.log',150)
    invoke(['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-f','lavfi','-i','testsrc2=size=640x480:rate=30',
        '-frames:v','15','-an','-c:v','libx264','-threads','1','-profile:v','main','-bf','0','-g','30','-pix_fmt','yuv420p',str(scratch/'generated-video.mov')],
        'generated-video.local.log',20)
    # Reproduce precisely the old Stop caller ordering in its own empty tree.
    old_root=scratch/'old-order';old_root.mkdir()
    shutil.copy2(scratch/'.sapflow-output-ownership-fixture',old_root/'.sapflow-output-ownership-fixture')
    shutil.copy2(scratch/'generated-video.mov',old_root/'generated-video.mov')
    old=json.loads(invoke([str(scratch/'build/debug/OutputOwnershipCheck'),str(old_root)],'old-order-runtime.local.json',30,accepted=(1,),extra={'SAPFLOW_PROVE_OLD_STOP_ORDER':'1'}).read_text())
    assert old['generatedOnly'] and old['reconstructedOldStopOrder'] and len(old['cases'])==1
    assert old['cases'][0]['failed'] and old['cases'][0]['error']=='CaptureFailure(message: "Recording did not preserve admitted native AAC/EOF prefix")',old
    actual=scratch/'actual';actual.mkdir()
    shutil.copy2(scratch/'.sapflow-output-ownership-fixture',actual/'.sapflow-output-ownership-fixture')
    shutil.copy2(scratch/'generated-video.mov',actual/'generated-video.mov')
    summary=json.loads(invoke([str(scratch/'build/debug/OutputOwnershipCheck'),str(actual)],'runtime.local.json',45).read_text())
    assert summary['generatedOnly'] and not summary['reconstructedOldStopOrder'] and summary['codedIdrMismatchRejected']
    assert [r['name'] for r in summary['cases']]==['open-fanout','held-stop','held-auto','end-recording','end-invalid-aac','publishing-refused','publishing-pre-idr']
    assert summary['cases'][5]['actualPublishingRefusalVerified']
    assert summary['cases'][6]['preIdrRefusalVerified'] and summary['cases'][6]['postIdrLocalConfirmationVerified']
    for row in summary['cases']:
        assert not row.get('failed') and row['validatedExactlyOnce'] and row['exactNativeStoredAAC'] and row['recorderClosedBeforeTransport'] and row['lateJoinDidNotFinishSuccessor']
        saved=actual/row['recording'];native=actual/row['nativePCM']
        assert saved.is_file() and not saved.is_symlink() and saved.resolve().is_relative_to(actual.resolve())
        facts=json.loads(invoke(['ffprobe','-v','error','-ignore_editlist','1','-show_packets','-show_data_hash','sha256',
            '-show_entries','packet=codec_type,size,data_hash,pts,duration','-of','json',str(saved)],row['name']+'-stored-packets.local.json',15).read_text())
        for kind,key in [('audio','audioPackets'),('video','videoPackets')]:
            packets=[{'size':int(p['size']),'sha256':p['data_hash'].removeprefix('SHA256:').lower()}
                for p in facts['packets'] if p['codec_type']==kind and int(p['size'])>0]
            assert packets==row[key],(row['name'],kind,'Original stored packet sequence changed')
        pcm=actual/(row['name']+'-independent.pcm')
        invoke(['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-i',str(saved),'-map','0:a:0','-c:a','pcm_s16le','-f','s16le',str(pcm)],row['name']+'-decode.local.log',15)
        samples=array.array('h',pcm.read_bytes());native_samples=array.array('h',native.read_bytes())
        (run/(row['name']+'-presentation.local.json')).write_text(json.dumps({
            'nativeFrames':len(native_samples),'independentFrames':len(samples),
            'expectedNativeFrames':row['nativePCMFrames'],'validInputFrames':row['validInputFrames'],
            'nativePCMSHA256':sha(native),'independentPCMSHA256':sha(pcm),
            'nativeLeadingFrames':row['nativeLeadingFrames'],'nativeTrailingFrames':row['nativeTrailingFrames'],
            'nativeCodedFrames':row['nativeCodedFrames']},indent=2)+'\n')
        # Native presentation is exact. An independent AAC decoder may retain
        # precisely the native-declared padding, as in the existing recording
        # fixtures. Neither view may lose/shift the original valid prefix.
        permitted_frames={row['nativePCMFrames']}
        if row['durableState']=='finalised':
            permitted_frames.add(row['nativeCodedFrames']-row['nativeLeadingFrames'])
        assert len(samples) in permitted_frames,(row['name'],'Independent presentation extent changed',len(samples),sorted(permitted_frames))
        valid=row['validInputFrames'];prefix=samples[:valid]
        assert len(prefix)==valid
        source_correlation=1.0
        if row['zeroOnly']:
            assert all(n==0 for n in samples) and all(n==0 for n in native_samples)
        else:
            source=[round((0.28*math.sin(2*math.pi*(113*(n/48000)+173*(n/48000)**2))+0.18*math.sin(2*math.pi*(997*(n/48000)+71*(n/48000)**2)))*32767) for n in range(valid)]
            source_correlation=sum(a*b for a,b in zip(source,prefix))/math.sqrt(sum(a*a for a in source)*sum(b*b for b in prefix))
            assert source_correlation>0.995,(row['name'],'Independent valid source waveform changed',source_correlation)
            if reference_pcm is None:reference_pcm=prefix
            else:assert prefix==reference_pcm,(row['name'],'Stop/Auto changed independently decoded valid source presentation')
        proofs.append({'name':row['name'],'recordingSHA256':sha(saved),'recordingBytes':saved.stat().st_size,
            'originalAudioPackets':len(row['audioPackets']),'originalVideoPackets':len(row['videoPackets']),
            'exactFFprobeStoredPayloads':True,'nativePCMFrames':row['nativePCMFrames'],'independentPCMFrames':len(samples),
            'independentPresentationQualifiedHere':True,'independentSourceCorrelation':source_correlation,
            'permittedIndependentFrames':sorted(permitted_frames),'nativePCMSHA256':sha(native),'independentPCMSHA256':sha(pcm),
            'nativeEOFEvents':row['actualNativeEOFEvents'],'nativeCodedFrames':row['nativeCodedFrames'],
            'nativeLeadingFrames':row['nativeLeadingFrames'],'nativeTrailingFrames':row['nativeTrailingFrames'],
            'nativeSourceCorrelation':row['nativeSourceCorrelation'],'transport':row['transport'],'durableState':row['durableState'],
            'actualPublishingRefusalVerified':row['actualPublishingRefusalVerified'],'preIdrRefusalVerified':row['preIdrRefusalVerified'],
            'postIdrLocalConfirmationVerified':row['postIdrLocalConfirmationVerified'],'nativePreIdrAudioUnits':row['nativePreIdrAudioUnits']})
    # Exact pre-repair Worker from the retained source-only accepted V2 run.
    # A single UNUSED constructor parameter adapts today's fixture call. The
    # historical implementation and void acceptance logic remain byte-exact.
    old_archive=root/'target/ios-output-ownership-fixture/20261022T205933Z-cc75b961/source-inputs.tar.gz'
    old_receipt=json.loads((old_archive.parent/'receipt.local.json').read_text())
    assert sha(old_archive)==old_receipt['sourceCopyArchiveSHA256']
    with tarfile.open(old_archive) as archive:
        original=archive.extractfile('Sources/OutputOwnershipCheck/EncodedIngressWorker.swift').read()
    assert hashlib.sha256(original).hexdigest()=='1c9ebaf3d3939c03a812a87c41c11e0408435e1c6db7173e4aaea3cb4f09cee4'
    needle=b'         queue: DispatchQueue = DispatchQueue(label: "sapflow.encoded-ingress", qos: .userInitiated)) throws {'
    assert original.count(needle)==1
    compatibility=original.replace(needle,b'         publishingQueueBytes: UInt64 = 4 * 1024 * 1024,\n'+needle,1)
    (run/'historical-worker.original.swift').write_bytes(original)
    (run/'historical-worker.compat.swift').write_bytes(compatibility)
    old_publish_overlay={'archive':str(old_archive.relative_to(root)),'archiveSHA256':sha(old_archive),
        'originalSHA256':hashlib.sha256(original).hexdigest(),'compatibilitySHA256':hashlib.sha256(compatibility).hexdigest(),
        'onlyDelta':'One unused constructor parameter publishingQueueBytes; historical void receipt implementation unchanged.'}
    (target/'EncodedIngressWorker.swift').write_bytes(compatibility)
    invoke(['swift','build','--package-path',str(scratch),'--scratch-path',str(scratch/'build'),'--jobs','2','--disable-sandbox','--disable-automatic-resolution'],'historical-publish-build.local.log',100)
    bad=scratch/'historical-publish';bad.mkdir()
    shutil.copy2(scratch/'.sapflow-output-ownership-fixture',bad/'.sapflow-output-ownership-fixture')
    shutil.copy2(scratch/'generated-video.mov',bad/'generated-video.mov')
    old_publish=json.loads(invoke([str(scratch/'build/debug/OutputOwnershipCheck'),str(bad)],'historical-publish-runtime.local.json',30,accepted=(1,),extra={'SAPFLOW_PROVE_OLD_PUBLISH_RECEIPT':'1'}).read_text())
    assert old_publish['reconstructedOldPublishReceipt'] and len(old_publish['cases'])==1
    assert old_publish['cases'][0].get('failed') and old_publish['cases'][0]['error']=='CaptureFailure(message: "Publishing branch claimed admission despite 1-byte SDK publishing limit")',old_publish
    passed=True
except BaseException as error:failure={'type':type(error).__name__,'reason':str(error)[:1200]}
finally:
    for child in children:
        if child.poll() is None:
            os.killpg(child.pid,signal.SIGTERM)
            try:child.wait(timeout=3)
            except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
    same=inputs=={str(p.relative_to(root)):sha(p) for p in sources+packages}
    diagnostics={str(p.relative_to(scratch)):json.loads(p.read_text()) for p in scratch.rglob('producer-diagnostics.local.json')}
    shutil.rmtree(scratch)
    receipt={'passed':passed and same,'candidateAssertionsPassed':passed,'sourceUnchanged':same,
        'inputManifestSHA256':sha(run/'input-hashes.local.json'),'sourceCopyArchiveSHA256':sha(run/'source-inputs.tar.gz') if (run/'source-inputs.tar.gz').exists() else None,
        'commands':commands,'proofs':proofs,'failure':failure,'producerDiagnostics':diagnostics,
        'reconstructedOldCallerOrder':old,'reconstructedOldPublishingReceipt':old_publish,'historicalPublishingOverlay':old_publish_overlay,'childrenStopped':all(c.poll() is not None for c in children),
        'scratchAndAllGeneratedMediaRemoved':not scratch.exists(),
        'scope':'Generated actual native AAC, real Admission/worker/Rust/Recorder/catalogue. Transport spy only. Source copies exact but root runtime may be mutable; no final integration acceptance if input drift. Native presentation is exact; independent presentation permits only native-declared padding and requires original valid source at zero lag. No capture, device, network or physical privacy/real-time pacing acceptance.'}
    (run/'receipt.local.json').write_text(json.dumps(receipt,indent=2)+'\n')
    print(str(run.relative_to(root)));print(json.dumps({'passed':receipt['passed'],'candidateAssertionsPassed':passed,'sourceUnchanged':same,'failure':failure,'scratchRemoved':not scratch.exists()}))
if not passed or not same:raise SystemExit(1)
PY
