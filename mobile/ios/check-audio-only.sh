#!/bin/bash
# Generated macOS native AAC/recording proof only. No caller-supplied media.
set -euo pipefail
if [ "$#" -ne 0 ]; then echo 'Usage: check-audio-only.sh (generated input only)' >&2; exit 2; fi
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
mkdir -p "$sapflow_root/target/ios-audio-only-fixture"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-ios-audio-only-XXXXXX")"
trap 'rm -rf -- "$sapflow_scratch"' EXIT
umask 077
printf 'generated-lavfi-audio-only-v1\n' > "$sapflow_scratch/.sapflow-audio-only-fixture"
python3 - "$sapflow_root" "$sapflow_scratch" <<'PY'
from pathlib import Path
import datetime,hashlib,json,os,shutil,signal,subprocess,sys,tempfile,uuid
root=Path(sys.argv[1]);scratch=Path(sys.argv[2]);base=root/'target/ios-audio-only-fixture'
run=base/(datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ-')+uuid.uuid4().hex[:8]);run.mkdir(mode=0o700)
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
names=['NativeAudioEncoder','CompressedRecorder','RecordingCatalogue','RecordingStorage','RecordingCaptureAdmission','CaptureAdmission','CaptureProfile','CaptureOrientation','PublishingTelemetry','EncodedWorkBudget','EncodedPipeline']
sources=[root/('mobile/ios/Sources/'+name+'.swift') for name in names]
sources += [root/'mobile/ios/Tools/AudioOnlyFixtureCheck.swift',root/'mobile/ios/check-audio-only.sh']
package=root/'target/native-packages/apple';package_files=sorted(p for p in package.rglob('*') if p.is_file())
assert package_files and len(package_files)<=64 and not any(p.is_symlink() for p in package_files)
inputs={str(p.relative_to(root)):sha(p) for p in sources+package_files}
(run/'input-hashes.local.json').write_text(json.dumps(inputs,indent=2)+'\n')
commands=[];children=[];media=[];passed=False;failure=None

def stopped(signum,frame):raise RuntimeError('Generated gate interrupted by signal '+str(signum))
for signum in [signal.SIGTERM,signal.SIGINT]:signal.signal(signum,stopped)

def invoke(command,name,timeout=60):
    log=run/name
    with log.open('w') as out:
        process=subprocess.Popen(command,stdout=out,stderr=subprocess.STDOUT,start_new_session=True,env=dict(os.environ,CLANG_MODULE_CACHE_PATH=str(root/'target/ios-app/DerivedData/ModuleCache.noindex')))
        children.append(process)
        try:code=process.wait(timeout=timeout)
        except BaseException:
            if process.poll() is None:
                os.killpg(process.pid,signal.SIGTERM)
                try:process.wait(timeout=3)
                except subprocess.TimeoutExpired:os.killpg(process.pid,signal.SIGKILL);process.wait()
            raise
    commands.append({'command':command,'exit':code,'log':name,'logSHA256':sha(log)})
    if code:raise RuntimeError(name+' failed with exit '+str(code))
    return log

try:
    target=scratch/'Sources/AudioOnlyCheck';target.mkdir(parents=True)
    for source in sources[:-1]:shutil.copy2(source,target/source.name)
    (scratch/'Package.swift').write_text('''// swift-tools-version: 5.9
import PackageDescription
let package = Package(name: "AudioOnlyCheck", platforms: [.macOS(.v12)], dependencies: [.package(name: "Sapflow", path: PACKAGE)], targets: [.executableTarget(name: "AudioOnlyCheck", dependencies: [.product(name: "Sapflow", package: "Sapflow")], swiftSettings: [.unsafeFlags(["-warnings-as-errors"])])])
'''.replace('PACKAGE',json.dumps(str(package))))
    fixture=scratch/'generated.wav'
    invoke(['ffmpeg','-nostdin','-v','error','-filter_threads','1','-f','lavfi','-i','sine=frequency=440:sample_rate=48000','-t','3','-ac','1','-c:a','pcm_s16le',str(fixture)],'generation.local.log',15)
    assert fixture.stat().st_size<512*1024
    info=json.loads(invoke(['ffprobe','-v','error','-show_streams','-show_format','-of','json',str(fixture)],'generated-probe.local.json',10).read_text())
    assert len(info['streams'])==1 and info['streams'][0]['codec_name']=='pcm_s16le' and info['streams'][0]['sample_rate']=='48000' and info['streams'][0]['channels']==1 and float(info['format']['duration'])==3
    generated_sha=sha(fixture)
    invoke(['swift','build','--package-path',str(scratch),'--scratch-path',str(scratch/'build'),'--jobs','2','--disable-sandbox','--disable-automatic-resolution'],'build.local.log',150)
    runtime=invoke([str(scratch/'build/debug/AudioOnlyCheck'),str(scratch)],'runtime.local.json',40)
    summary=json.loads(runtime.read_text());assert summary['actualCatalogueClosed'] and summary['encodePasses']==1 and summary['videoTracks']==0 and summary['generatedPCMFrames']==144000
    assert summary['rustValidatedSameAACUnits']==summary['nativeAACUnits'] and 125<=summary['nativeAACUnits']<=160
    for row in summary['cases']:
        if row['case']=='empty':assert row['bytes']==0;continue
        original=scratch/'recordings'/row['original'];exported=scratch/'exports'/row['export']
        assert original.is_file() and exported.is_file() and sha(original)==sha(exported) and original.stat().st_size<=128*1024
        facts=json.loads(invoke(['ffprobe','-v','error','-count_packets','-show_streams','-show_format','-of','json',str(exported)],row['case']+'-probe.local.json',10).read_text())
        tracks=facts['streams'];assert len(tracks)==1
        track=tracks[0];assert track['codec_type']=='audio' and track['codec_name']=='aac' and track['profile']=='LC' and track['sample_rate']=='48000' and track['channels']==1 and int(track['nb_read_packets'])>0
        assert 0<float(facts['format']['duration'])<3.5
        packet_views={}
        for view,options in [('default',[]),('ignore_editlist',['-ignore_editlist','1']),('simple_editlist',['-advanced_editlist','0'])]:
            packets=json.loads(invoke(['ffprobe','-v','error',*options,'-show_packets','-show_data_hash','sha256','-show_entries','packet=size,data_hash,pts,duration,side_data_list','-of','json',str(exported)],row['case']+'-'+view+'-packet-hashes.local.json',10).read_text())['packets']
            assert len(packets)<=176
            positive=[packet for packet in packets if int(packet['size'])>0]
            assert len(packets)-len(positive)<=16
            exact=[{'size':int(packet['size']),'sha256':packet['data_hash'].removeprefix('SHA256:').lower()} for packet in positive]==row['encodedPayloadHashes']
            packet_views[view]={'containerPackets':len(packets),'emptyPackets':len(packets)-len(positive),'positivePackets':len(positive),'exactNativePayloadSequence':exact}
        # Ignore-editlist inspects stored media instead of the default demuxer's
        # presentation edits. Never relax exact encoded payload retention.
        exact_stored=packet_views['ignore_editlist']['exactNativePayloadSequence']
        invoke(['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-i',str(exported),'-map','0:a:0','-f','null','-'],row['case']+'-decode.local.log',15)
        pcm=scratch/(row['case']+'.pcm')
        invoke(['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-i',str(exported),'-map','0:a:0','-c:a','pcm_s16le','-f','s16le',str(pcm)],row['case']+'-pcm.local.log',15)
        assert 4096<=pcm.stat().st_size<=512*1024 and pcm.stat().st_size%2==0
        media.append({'case':row['case'],'state':row['state'],'bytes':original.stat().st_size,'originalSHA256':sha(original),'exportSHA256':sha(exported),'maxExportChunk':row['maxExportChunk'],'containerPackets':int(track['nb_read_packets']),'packetViews':packet_views,'exactStoredNativePayloadSequence':exact_stored,'decodedPCMFrames':pcm.stat().st_size//2,'decodedPCMSHA256':sha(pcm),'duration':facts['format']['duration'],'independentDecodeExit':0})
    assert len(media)==3 and all(c['maxExportChunk']<=65536 for c in media)
    assert all(c['exactStoredNativePayloadSequence'] for c in media), 'Stored AAC payload hashes differ from native encoded output'
    passed=True
except BaseException as error:
    failure={'type':type(error).__name__,'reason':str(error)[:300]}
finally:
    for process in children:
        if process.poll() is None:
            os.killpg(process.pid,signal.SIGTERM)
            try:process.wait(timeout=3)
            except subprocess.TimeoutExpired:os.killpg(process.pid,signal.SIGKILL);process.wait()
    same=inputs=={str(p.relative_to(root)):sha(p) for p in sources+package_files}
    shutil.rmtree(scratch)
    receipt={'createdUtc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'passed':passed and same,'sourceUnchanged':same,'inputManifestSHA256':sha(run/'input-hashes.local.json'),'generatedInputSHA256':locals().get('generated_sha'),'commands':commands,'media':media,'failure':failure,'childrenStopped':all(p.poll() is not None for p in children),'scratchAndAllGeneratedMediaRemoved':not scratch.exists(),'scope':'macOS generated PCM -> actual NativeAudioEncoder once -> sameAAC actual RustEncodedIngress(hasVideo:false) and actual CompressedRecorder/Catalogue. No mic/camera/socket/device/phone/private existing media. No iPhone/background/crash/powerloss qualification.','packageScope':'Existing pinned Apple package; the input manifest identifies its actual bytes. This check does not rebuild packages.','incompleteScope':'Prior-loss and explicit incomplete-finish seams; no actual disk-pressure or crash injection'}
    (run/'receipt.local.json').write_text(json.dumps(receipt,indent=2)+'\n')
    print(str(run.relative_to(root)));print(json.dumps({'passed':receipt['passed'],'childrenStopped':receipt['childrenStopped'],'scratchRemoved':receipt['scratchAndAllGeneratedMediaRemoved'],'failure':failure}))
if not passed or not same:raise SystemExit(1)
PY
