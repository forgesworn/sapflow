#!/bin/bash
# Generated native audio only. No caller-supplied media, capture or sockets.
set -euo pipefail
if [ "$#" -ne 0 ]; then echo 'Usage: check-audio-standby.sh (generated input only)' >&2; exit 2; fi
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
umask 077
mkdir -p "$sapflow_root/target/ios-audio-standby-fixture"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-ios-audio-standby-XXXXXX")"
trap 'rm -rf -- "$sapflow_scratch"' EXIT
printf 'generated-native-audio-standby-v1\n' > "$sapflow_scratch/.sapflow-audio-standby-fixture"
python3 - "$sapflow_root" "$sapflow_scratch" <<'PY'
from pathlib import Path
import array,datetime,hashlib,json,math,os,shutil,signal,struct,subprocess,sys,uuid
# Declared before execution, never fitted to observed residuals. AAC-LC's
# 2048-point synthesis filterbank overlaps adjacent 1024-frame output blocks
# (ISO/IEC 14496-3:2009, 4.6.11.3.3). Preserve that entire transition block.
# This qualifies coded synthetic provenance and bounded decoded settling,
# not exact silence at the coded boundary or instantaneous remote privacy.
aac_decoder_overlap_frames=1024
root=Path(sys.argv[1]);scratch=Path(sys.argv[2]);base=root/'target/ios-audio-standby-fixture'
run=base/(datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ-')+uuid.uuid4().hex[:8]);run.mkdir(mode=0o700)
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
names=['AudioRecordingTiming','AudioShowLifecycle','NativeAudioEncoder','CompressedRecorder','RecordingCatalogue','RecordingStorage','RecordingCaptureAdmission','AudioInputPolicy','CameraControlPolicy','CaptureAdmission','CaptureProfile','CaptureOrientation','PublishingTelemetry','EncodedWorkBudget','EncodedPipeline','QuickTimeAudioPriming','QuickTimeAudioPrimingFile','QuickTimeMovieLayout']
sources=[root/('mobile/ios/Sources/'+n+'.swift') for n in names]
sources += [root/'mobile/ios/Tools/AudioStandbyFixtureCheck.swift',root/'mobile/ios/Tools/AudioOnlyFixtureCheck.swift',root/'mobile/ios/check-audio-standby.sh']
package=root/'target/native-packages/apple';packages=sorted(p for p in package.rglob('*') if p.is_file())
assert packages and len(packages)<=64 and not any(p.is_symlink() for p in packages)
inputs={str(p.relative_to(root)):sha(p) for p in sources+packages}
(run/'input-hashes.local.json').write_text(json.dumps(inputs,indent=2)+'\n')
children=[];commands=[];passed=False;failure=None;media=[]
def stopped(signum,frame):raise RuntimeError('Generated standby gate interrupted by signal '+str(signum))
for sig in [signal.SIGTERM,signal.SIGINT]:signal.signal(sig,stopped)
def invoke(args,name,timeout=30,accepted=(0,)):
    log=run/name
    with log.open('wb') as output:
        child=subprocess.Popen(args,stdout=output,stderr=subprocess.STDOUT,start_new_session=True,
            env=dict(os.environ,CLANG_MODULE_CACHE_PATH=str(root/'target/ios-module-cache')))
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
def timing_atoms(path):
    raw=path.read_bytes();assert len(raw)<=8*1024*1024
    result=[];visited=0
    containers={b'moov',b'trak',b'mdia',b'minf',b'stbl',b'edts',b'udta',b'meta',b'ilst',b'----'}
    def walk(a,b,parent,depth):
        nonlocal visited
        assert depth<=12
        index=0
        while a+8<=b:
            size,kind=struct.unpack_from('>I4s',raw,a);header=8
            if size==1:size=struct.unpack_from('>Q',raw,a+8)[0];header=16
            if size==0:size=b-a
            assert header<=size<=b-a
            visited+=1;assert visited<=2048
            data=raw[a+header:a+size];name=parent+'/'+kind.decode('latin1')+'['+str(index)+']'
            row={'path':name,'bytes':len(data)}
            if kind in [b'mvhd',b'mdhd']:
                version=data[0];offset=20 if version else 12
                row.update(version=version,timeScale=struct.unpack_from('>I',data,offset)[0],
                    duration=struct.unpack_from('>Q' if version else '>I',data,offset+4)[0]);result.append(row)
            elif kind==b'elst':
                version=data[0];count=struct.unpack_from('>I',data,4)[0];assert count<=32
                step=20 if version else 12;rows=[]
                for n in range(count):
                    value=struct.unpack_from('>Qqhh' if version else '>Iihh',data,8+n*step)
                    rows.append({'duration':value[0],'mediaTime':value[1],'rateInteger':value[2],'rateFraction':value[3]})
                row.update(version=version,entries=rows);result.append(row)
            elif kind in [b'stts',b'ctts',b'sbgp']:
                offset=8 if kind==b'sbgp' else 4
                if kind==b'sbgp' and data[0]==1:offset+=4
                count=struct.unpack_from('>I',data,offset)[0];assert count<=256
                row.update(version=data[0],entryCount=count,entries=[list(struct.unpack_from('>Ii' if kind==b'ctts' and data[0]==1 else '>II',data,offset+4+8*n)) for n in range(min(count,16))]);result.append(row)
            elif kind in [b'cslg',b'sgpd']:
                row['headerHex']=data[:96].hex();result.append(row)
            elif kind in [b'mean',b'name',b'data'] and b'----' in parent.encode('latin1'):
                row['text']=data[:512].decode('utf-8','replace');result.append(row)
            if kind in containers:walk(a+header+(4 if kind==b'meta' else 0),a+size,name,depth+1)
            a+=size;index+=1
    walk(0,len(raw),'',0)
    return result
try:
    target=scratch/'Sources/AudioStandbyCheck';target.mkdir(parents=True)
    for source in sources[:-1]:
        if source.name=='AudioOnlyFixtureCheck.swift':
            contents=source.read_text();assert contents.count('\n@main\n')==1
            (target/'FixtureReaders.swift').write_text(contents.replace('\n@main\n','\n',1))
        else:shutil.copy2(source,target/source.name)
    (scratch/'Package.swift').write_text('''// swift-tools-version: 5.9
import PackageDescription
let package = Package(name: "AudioStandbyCheck", platforms: [.macOS(.v12)], dependencies: [.package(name: "Sapflow", path: PACKAGE)], targets: [.executableTarget(name: "AudioStandbyCheck", dependencies: [.product(name: "Sapflow", package: "Sapflow")], swiftSettings: [.unsafeFlags(["-warnings-as-errors"])])])
'''.replace('PACKAGE',json.dumps(str(package))))
    invoke(['swift','build','--package-path',str(scratch),'--scratch-path',str(scratch/'build'),'--jobs','2','--disable-sandbox','--disable-automatic-resolution'],'build.local.log',150)
    invoke(['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-f','lavfi','-i','testsrc2=size=640x480:rate=30',
        '-frames:v','63','-an','-c:v','libx264','-threads','1','-profile:v','main','-bf','0','-g','30','-pix_fmt','yuv420p',
        str(scratch/'generated-video.mov')],'generated-video.local.log',20)
    summary=json.loads(invoke([str(scratch/'build/debug/AudioStandbyCheck'),str(scratch)],'runtime.local.json',45,accepted=(0,1)).read_text())
    assert summary['generatedOnly'] and [c['name'] for c in summary['cases']]==['mono48','stereo441']
    native_failures=[{'name':r['name'],'error':r['error']} for r in summary['cases'] if r.get('failed')]
    silence_failures=[]
    av_failures=[]
    for row in summary['cases']:
        if row.get('failed'):
            continue
        name=row['name'];case=scratch/name;recorded=case/row['recording']
        assert recorded.parent==case and recorded.suffix=='.m4a' and recorded.is_file() and not recorded.is_symlink()
        expected=[{'size':p['size'],'sha256':p['sha256']} for p in row['packets']]
        assert row['actualRustValidatedUnits']==len(expected) and 1<=len(expected)<=256
        assert all(p['pts48k']==i*1024 for i,p in enumerate(row['packets']))
        assert row['rawTokenStaleRefused'] and row['finishIdempotent'] and row['postEOFSilenceRefused']
        assert row['nativePCMFrames']==row['attainableFinalValidFrames']
        assert abs(row['quantizationOutputFrames'])<=1
        native=case/'native.pcm';assert sha(native)==row['nativePCMSHA256'] and native.stat().st_size==row['nativePCMFrames']*2
        # The same decoder remains alive over the complete concatenated AAC.
        joined=case/'joined.aac';pcm=case/'joined.pcm'
        invoke(['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-i',str(joined),'-map','0:a:0','-c:a','pcm_s16le','-f','s16le',str(pcm)],name+'-continuous-decode.local.log',15)
        assert pcm.stat().st_size==len(expected)*1024*2
        facts=json.loads(invoke(['ffprobe','-v','error','-ignore_editlist','1','-show_packets','-show_data_hash','sha256','-show_entries','packet=pos,size,data_hash,pts,duration','-of','json',str(recorded)],name+'-stored-packets.local.json',15).read_text())
        packets=[p for p in facts['packets'] if int(p['size'])>0]
        actual=[{'size':int(p['size']),'sha256':p['data_hash'].removeprefix('SHA256:').lower()} for p in packets]
        assert actual==expected,'Stored AAC packet sequence differs from actual encoder'
        presented=case/'presented.pcm'
        invoke(['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-i',str(recorded),'-map','0:a:0','-c:a','pcm_s16le','-f','s16le',str(presented)],name+'-recording-decode.local.log',15)
        assert row['nativeConfigurationChecks']==3 and row['sdkConfigurations']==1
        assert row['nativeIntervals']['contiguous'] and row['nativeIntervals']['firstPts48k']==0
        assert row['nativeIntervals']['finalEnd48k']==row['attainableFinalValidFrames']
        leading,tail=row['fileLeadingFrames'],row['fileTrailingFrames']
        assert presented.stat().st_size//2 in [row['nativePCMFrames'],row['nativePCMFrames']+tail]
        references={}
        views={'native':array.array('h',native.read_bytes()),'continuous':array.array('h',pcm.read_bytes()),'presented':array.array('h',presented.read_bytes())}
        controls={}
        assert [c['name'] for c in row['controls']]==['fresh','zero']
        for control in row['controls']:
            label=control['name'];control_native=case/(label+'-native.pcm');control_pcm=case/(label+'-continuous.pcm')
            assert sha(control_native)==control['nativePCMSHA256']
            assert control_native.stat().st_size//2==control['nativeFrames']==control['codedFrames']
            invoke(['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-i',str(case/(label+'.aac')),
                '-map','0:a:0','-c:a','pcm_s16le','-f','s16le',str(control_pcm)],name+'-'+label+'-control-decode.local.log',15)
            assert control_pcm.stat().st_size//2==control['codedFrames']
            controls[label]={'native':array.array('h',control_native.read_bytes()),
                'continuous':array.array('h',control_pcm.read_bytes())}
            for decoder,samples in controls[label].items():
                checked=samples if label=='zero' else samples[:aac_decoder_overlap_frames]
                assert max(map(abs,checked))==0,(name,label,decoder,'Fresh-only diagnostic contains signal')
        marker_proof=[]
        for tone in row['tones']:
            at=round(tone['startValue']*48000/tone['startScale']);count=round(tone['sourceFrames']*48000/tone['sourceRate'])
            def reference(n):
                t=n/48000
                if tone['kind']==0:return 32767*(.28*math.sin(2*math.pi*(113*t+173*t*t))+.18*math.sin(2*math.pi*(997*t+71*t*t)))
                return 32767*(.23*math.sin(2*math.pi*(317*t+127*t*t))+.21*math.sin(2*math.pi*(1297*t+53*t*t)))
            # Fixed, declared edge exclusion for lossy-codec transients. No
            # lag search, shifting, seeking or post-hoc marker placement.
            ref=[reference(n) for n in range(512,count-512)]
            correlations={}
            for view,samples in views.items():
                offset=at+(leading if view=='continuous' else 0)
                segment=samples[offset+512:offset+count-512]
                assert len(segment)==len(ref) and len(ref)>1024
                cross=sum(a*b for a,b in zip(ref,segment));den=math.sqrt(sum(a*a for a in ref)*sum(float(b)*b for b in segment))
                corr=cross/den if den else 0
                assert corr>=.98,(name,tone['name'],view,'Independent zero-lag raw marker',corr)
                correlations[view]=corr
            marker_proof.append({'name':tone['name'],'externalPresentationStart48k':at,'referenceFrames':len(ref),'zeroLagCorrelations':correlations})
        b=row['tones'][1];b_start=round(b['startValue']*48000/b['startScale'])
        silence_proof={}
        for view,samples in views.items():
            shift=0 if view=='continuous' else -leading
            first,second=[n+shift for n in row['freshCodedBoundaries']]
            end_first=b_start+(leading if view=='continuous' else 0)-1024
            assert 0<=first<end_first<second<len(samples)
            # Future admitted Resume may pre-echo within its final AAC window;
            # exclude that one predetermined window, never search for silence.
            windows=[(first,end_first),(second,len(samples))]
            peaks=[max(map(abs,samples[a:b])) for a,b in windows]
            settled_peaks=[max(map(abs,samples[a+aac_decoder_overlap_frames:b])) for a,b in windows]
            # Retain bounded scalar diagnostics for every decoder and case even
            # when the original exact-zero assertion fails. Never move the
            # declared cutover or search for a later passing window.
            diagnostics=[]
            for a,b in windows:
                nonzero=[i for i in range(a,b) if samples[i]!=0]
                diagnostics.append({'nonzeroCount':len(nonzero),
                    'firstNonzero':nonzero[0] if nonzero else None,
                    'lastNonzero':nonzero[-1] if nonzero else None,
                    'firstNonzeroAfterWindowStart':nonzero[0]-a if nonzero else None,
                    'lastNonzeroBeforeResume':b_start+(leading if view=='continuous' else 0)-nonzero[-1] if nonzero else None,
                    'firstTwoPacketPeaks':[max(map(abs,samples[x:min(x+1024,b)])) for x in range(a,min(a+2048,b),1024)],
                    'lastTwoPacketPeaks':[max(map(abs,samples[x:min(x+1024,b)])) for x in range(max(a,b-2048),b,1024)]})
            if settled_peaks!=[0,0]:silence_failures.append({'name':name,'view':view,'settledPeaks':settled_peaks})
            # A fresh decoder is a diagnostic control only. The actual complete
            # decoder above remains the acceptance path and retains all audio.
            control=controls['fresh']['native' if view=='native' else 'continuous']
            assert samples[first+aac_decoder_overlap_frames:end_first]==control[aac_decoder_overlap_frames:end_first-first],(name,view,'Fresh-only and continuous settled output differ')
            silence_proof[view]={'windows':windows,'codedBoundaryPeaks':peaks,'strictCodedBoundaryZero':peaks==[0,0],
                'declaredDecoderOverlapFrames':aac_decoder_overlap_frames,'settledPeaks':settled_peaks,
                'freshOnlySettledComparisonExact':True,'diagnostics':diagnostics}
        media.append({'name':name,'nativePCMFrames':row['nativePCMFrames'],'continuousPCMFrames':pcm.stat().st_size//2,
            'presentedPCMFrames':presented.stat().st_size//2,'exactStoredPayloadSequence':True,'packets':len(expected),
            'recordingSHA256':sha(recorded),'continuousPCMSHA256':sha(pcm),'presentedPCMSHA256':sha(presented),
            'freshCodedBoundaries':row['freshCodedBoundaries'],'tones':row['tones'],'markers':marker_proof,'silence':silence_proof,
            'containerTiming':timing_atoms(recorded)})
        av_results=[]
        assert len(row['avRecordings'])==(3 if name=='mono48' else 0)
        for av in row['avRecordings']:
            saved=case/av['recording']
            assert saved.parent==case and saved.suffix=='.mov' and saved.is_file() and not saved.is_symlink()
            assert av['finalised'] != av['injectedNativeTimingFailure']
            default_facts=json.loads(invoke(['ffprobe','-v','error','-show_packets','-show_streams','-show_entries',
                'stream=index,codec_type,time_base,start_pts,duration_ts,nb_frames:stream_tags:packet=codec_type,pts,duration,size:packet_side_data',
                '-of','json',str(saved)],saved.stem+'-default-timing.local.json',15).read_text())
            av_facts=json.loads(invoke(['ffprobe','-v','error','-ignore_editlist','1','-show_packets','-show_data_hash','sha256',
                '-show_entries','packet=codec_type,size,data_hash,pts,duration','-of','json',str(saved)],saved.stem+'-stored-packets.local.json',15).read_text())
            stored={}
            for kind,key in [('audio','audioPackets'),('video','videoPackets')]:
                actual=[{'size':int(p['size']),'sha256':p['data_hash'].removeprefix('SHA256:').lower()}
                    for p in av_facts['packets'] if p['codec_type']==kind and int(p['size'])>0]
                stored[kind]={'expectedCount':len(av[key]),'storedCount':len(actual),'exact':actual==av[key]}
                if actual!=av[key]:av_failures.append({'name':saved.stem,'view':'ffprobe','kind':kind})
            if not av['exactNativeStoredPayloads']:av_failures.append({'name':saved.stem,'view':'native'})
            av_pcm=case/(saved.stem+'-presented.pcm')
            invoke(['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-i',str(saved),'-map','0:a:0','-c:a','pcm_s16le','-f','s16le',str(av_pcm),'-map','0:v:0','-f','null','-'],saved.stem+'-decode.local.log',15)
            av_native=case/av['nativePCM'];assert av_native.parent==case and av_native.is_file()
            av_native_samples=array.array('h',av_native.read_bytes());av_presented_samples=array.array('h',av_pcm.read_bytes())
            intervals=av['nativeIntervals'];valid=row['nativePCMFrames']
            assert len(av_native_samples)==av['nativePCMFrames']
            expected_native=av['expectedNativePCMFrames']
            audio_offset=av['audioOffset48k'];assert audio_offset in [0,1440]
            # Apple's PCM reader materialises the original empty edit as
            # silence from zero. FFmpeg retains that gap as stream start_pts.
            # Require the precise declared gap, exact zeros and unchanged
            # following source; never infer an offset by correlation/search.
            exact_native_clock=intervals['contiguous'] and intervals['firstPts48k']==0 and intervals['finalEnd48k']==audio_offset+expected_native and len(av_native_samples)==audio_offset+expected_native
            exact_native_gap=all(value==0 for value in av_native_samples[:audio_offset])
            exact_native_source=av_native_samples[audio_offset:audio_offset+valid]==views['native']
            exact_independent_source=av_presented_samples[:valid]==views['presented'][:valid]
            exact_video_clock=av['nativeVideoReader']['firstPts48k']==0 and av['nativeVideoReader']['lastPts48k']==99_200
            bounded_independent_tail=len(av_presented_samples) in [valid,valid+tail]
            audio_stream=next(s for s in default_facts['streams'] if s['codec_type']=='audio')
            exact_independent_clock=int(audio_stream['start_pts'])==audio_offset and audio_stream['time_base']=='1/48000'
            if not all([exact_native_clock,exact_native_gap,exact_native_source,exact_independent_source,exact_video_clock,bounded_independent_tail,exact_independent_clock]):
                av_failures.append({'name':saved.stem,'view':'presentation','nativeClock':exact_native_clock,
                    'nativeGap':exact_native_gap,'nativeSource':exact_native_source,'independentSource':exact_independent_source,
                    'videoClock':exact_video_clock,'boundedIndependentTail':bounded_independent_tail,'independentClock':exact_independent_clock})
                invoke(['ffmpeg','-nostdin','-v','debug','-threads','1','-i',str(saved),'-map','0:a:0','-f','null','-'],saved.stem+'-timing-debug.local.log',15)
            av_results.append({'recordingSHA256':sha(saved),'finalised':av['finalised'],
                'injectedNativeTimingFailure':av['injectedNativeTimingFailure'],
                'exactNativeStoredPayloads':av['exactNativeStoredPayloads'],'stored':stored,
                'nativeAudioReader':av['nativeAudioReader'],'nativeVideoReader':av['nativeVideoReader'],
                'nativeIntervals':intervals,'nativePCMFrames':len(av_native_samples),'expectedNativePCMFrames':expected_native,
                'independentPCMFrames':len(av_presented_samples),'exactNativeSource':exact_native_source,
                'exactIndependentSource':exact_independent_source,'exactVideoClock':exact_video_clock,
                'audioOffset48k':audio_offset,'exactIndependentClock':exact_independent_clock,
                'exactNativeEmptyEditSilence':exact_native_gap,
                'containerTiming':timing_atoms(saved),'defaultTimingFacts':default_facts,
                'independentAudioVideoDecode':True})
        media[-1]['avRecordings']=av_results
        short_results=[]
        assert len(row['shortRecordings'])==(2*(leading//1024+2) if name=='mono48' else 0)
        for short in row['shortRecordings']:
            saved=case/short['recording'];assert saved.parent==case and saved.suffix=='.mov' and saved.is_file() and not saved.is_symlink()
            assert not short['finalised']
            facts=json.loads(invoke(['ffprobe','-v','error','-ignore_editlist','1','-show_packets','-show_data_hash','sha256',
                '-show_entries','packet=codec_type,size,data_hash,pts,duration','-of','json',str(saved)],saved.stem+'-stored-packets.local.json',15).read_text())
            for kind,key in [('audio','audioPackets'),('video','videoPackets')]:
                actual=[{'size':int(p['size']),'sha256':p['data_hash'].removeprefix('SHA256:').lower()}
                    for p in facts['packets'] if p['codec_type']==kind and int(p['size'])>0]
                expected_prefix=short[key]
                # An incomplete, insufficient AAC head may include writer-owned
                # preroll. Every original accepted packet must still occur in
                # order as one intact prefix sequence; it is never complete.
                retained=(actual==expected_prefix) if kind=='video' else any(actual[i:i+len(expected_prefix)]==expected_prefix for i in range(len(actual)-len(expected_prefix)+1))
                assert retained,(saved.stem,kind,'Accepted short prefix was lost')
            invoke(['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-i',str(saved),'-map','0:a:0?','-map','0:v:0','-f','null','-'],saved.stem+'-decode.local.log',15)
            short_results.append({'recordingSHA256':sha(saved),'acceptedAudioUnits':short['acceptedAudioUnits'],
                'injectedNativeTimingFailure':short['injectedNativeTimingFailure'],'originalAcceptedPrefixRetained':True,'finalised':False,'independentDecode':True})
        media[-1]['shortRecordings']=short_results
    assert not native_failures and not silence_failures and not av_failures,('Actual native gates failed',native_failures,silence_failures,av_failures)
    passed=True
except BaseException as error:failure={'type':type(error).__name__,'reason':str(error)[:400]}
finally:
    for child in children:
        if child.poll() is None:
            os.killpg(child.pid,signal.SIGTERM)
            try:child.wait(timeout=3)
            except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
    same=inputs=={str(p.relative_to(root)):sha(p) for p in sources+packages}
    diagnostics={str(p.relative_to(scratch)):json.loads(p.read_text()) for p in scratch.glob('*/diagnostics.local.json')}
    shutil.rmtree(scratch)
    receipt={'passed':passed and same,'sourceUnchanged':same,'inputManifestSHA256':sha(run/'input-hashes.local.json'),
        'commands':commands,'media':media,'failure':failure,'diagnostics':diagnostics,'nativeFailures':locals().get('native_failures',[]),'silenceFailures':locals().get('silence_failures',[]),'avFailures':locals().get('av_failures',[]),'childrenStopped':all(c.poll() is not None for c in children),
        'scratchAndAllGeneratedMediaRemoved':not scratch.exists(),
        'declaredAACLCDecoderOverlapFrames':aac_decoder_overlap_frames,
        'scope':'Generated native PCM -> actual NativeAudioEncoder -> same packaged Rust ingress and actual CompressedRecorder/Catalogue only. Synthetic coded provenance with declared AAC-LC decoded settling; no exact coded-boundary silence claim. No mic, camera, socket, phone, capture-driver pacing or physical acceptance.'}
    (run/'receipt.local.json').write_text(json.dumps(receipt,indent=2)+'\n')
    print(str(run.relative_to(root)));print(json.dumps({'passed':receipt['passed'],'failure':failure,'scratchRemoved':receipt['scratchAndAllGeneratedMediaRemoved']}))
if not passed or not same:raise SystemExit(1)
PY
