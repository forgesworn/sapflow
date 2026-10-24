#!/bin/bash
# Generated host-native strict-token VT proof. No capture/device/input media.
set -euo pipefail
if [ "$#" -ne 0 ]; then echo 'Usage: check-video-ownership.sh (generated only)' >&2; exit 2; fi
root="$(cd "$(dirname "$0")/../.." && pwd)"
umask 077
python3 - "$root" <<'PYCODE'
from pathlib import Path
import datetime,hashlib,json,os,shutil,signal,subprocess,sys,tarfile,tempfile,uuid
root=Path(sys.argv[1]);run=root/'target/ios-video-ownership-fixture'/(datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ-')+uuid.uuid4().hex[:8]);run.mkdir(parents=True,mode=0o700)
scratch=Path(tempfile.mkdtemp(prefix='sapflow-video-ownership-',dir=root/'target'))
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
names=['HardwareVideoEncoder','VideoEncodingPolicy','PreparedVideoSlate','CaptureAdmission','CaptureProfile','CaptureOrientation','AudioInputPolicy','CameraControlPolicy','AudioShowLifecycle','EncodedPipeline','PublishingTelemetry']
sources=[root/('mobile/ios/Sources/'+n+'.swift') for n in names]+[root/'mobile/ios/Tools/StandbyVideoOwnershipCheck.swift',root/'mobile/ios/check-video-ownership.sh']
package=root/'target/native-packages/apple';packages=sorted(p for p in package.rglob('*') if p.is_file())
assert packages and len(packages)<=64 and not any(p.is_symlink() for p in packages)
inputs={str(p.relative_to(root)):sha(p) for p in sources+packages};(run/'input-hashes.local.json').write_text(json.dumps(inputs,indent=2)+'\n')
children=[];commands=[];passed=False;failure=None;native=None;frames=[]
def invoke(args,name,timeout=30,accepted=(0,)):
    with (run/name).open('wb') as log:
        child=subprocess.Popen(args,stdout=log,stderr=subprocess.STDOUT,start_new_session=True,env=dict(os.environ,CLANG_MODULE_CACHE_PATH=str(root/'target/ios-module-cache')));children.append(child)
        try:code=child.wait(timeout=timeout)
        except BaseException:
            if child.poll() is None:
                os.killpg(child.pid,signal.SIGTERM)
                try:child.wait(timeout=3)
                except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
            raise
    commands.append({'command':args,'exit':code,'log':name,'logSHA256':sha(run/name)})
    if code not in accepted:raise RuntimeError(name+' exit '+str(code))
    return run/name
try:
    dest=scratch/'Sources/VideoOwnershipCheck';dest.mkdir(parents=True)
    for source in sources[:-1]:shutil.copy2(source,dest/source.name)
    assert inputs=={str(p.relative_to(root)):sha(p) for p in sources+packages},'Inputs changed during copy'
    with tarfile.open(run/'source-inputs.tar.gz','w:gz') as archive:
        for p in sorted(dest.iterdir()):archive.add(p,arcname='Sources/VideoOwnershipCheck/'+p.name,recursive=False)
    (scratch/'Package.swift').write_text('''// swift-tools-version: 5.9
import PackageDescription
let package = Package(name: "VideoOwnershipCheck", platforms: [.macOS(.v12)], dependencies: [.package(name: "Sapflow", path: PACKAGE)], targets: [.executableTarget(name: "VideoOwnershipCheck", dependencies: [.product(name: "Sapflow", package: "Sapflow")], swiftSettings: [.unsafeFlags(["-warnings-as-errors"])])])
'''.replace('PACKAGE',json.dumps(str(package))))
    invoke(['swift','build','--package-path',str(scratch),'--scratch-path',str(scratch/'build'),'--jobs','2','--disable-sandbox','--disable-automatic-resolution'],'build.local.log',150)
    generated=scratch/'generated';generated.mkdir();(generated/'.sapflow-video-ownership').write_text('generated-video-ownership-v1\n')
    native=json.loads(invoke([str(scratch/'build/debug/VideoOwnershipCheck'),str(generated)],'runtime.local.json',40,accepted=(0,1)).read_text())
    assert native['passed'],native
    assert native['generatedOnly'] and native['sessionsInPositiveShow']==1 and len(native['nativeOutput'])==6
    assert all(native[k] for k in ['sameSPSPPS','sameCanvasAndOriginalPTS','oldRawKeptOriginalTokenWhileCallbackHeld','staleRawNotSubmitted','actualSealWaitedHeldOutput','actualFinishCheckedQuiescent','rawSampleCannotClaimSlate'])
    coded=generated/'generated.h264';coded_fact={'bytes':coded.stat().st_size,'sha256':sha(coded)}
    facts=json.loads(invoke(['ffprobe','-v','error','-show_streams','-count_frames','-of','json',str(coded)],'probe.local.json',15).read_text())
    stream=facts['streams'][0];width=native['width'];height=native['height']
    assert stream['codec_name']=='h264' and stream['width']==width and stream['height']==height and int(stream['nb_read_frames'])==6
    raw=generated/'decoded.yuv'
    invoke(['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-i',str(coded),'-pix_fmt','yuv420p','-f','rawvideo',str(raw)],'decode.local.log',15)
    decoded=raw.read_bytes();size=width*height*3//2;assert len(decoded)==6*size
    for n in range(6):
        y=decoded[n*size:n*size+width*height]
        background=sum(y[row*width+col] for row in range(20,40) for col in range(20,40))/400
        barwidth=max(1,width//24);barheight=max(2,height//8);barx=width//2-2*barwidth
        mark=sum(y[row*width+col] for row in range(height//2-barheight//4,height//2+barheight//4) for col in range(barx+barwidth//4,barx+3*barwidth//4))/((2*(barheight//4))*(3*barwidth//4-barwidth//4))
        if n<3:assert abs(background-80)<=3 and abs(mark-80)<=3,(n,background,mark)
        else:assert background<=24 and mark>=128 and mark-background>100,(n,background,mark)
        frames.append({'index':n,'kind':'raw' if n<3 else 'slate','backgroundLuma':background,'markLuma':mark})
    (run/'independent-picture.local.json').write_text(json.dumps({'frames':frames,'coded':coded_fact,'decodedSHA256':sha(raw),'decodedBytes':len(decoded)},indent=2)+'\n')
    passed=True
except BaseException as error:failure={'type':type(error).__name__,'reason':str(error)[:2000]}
finally:
    for child in children:
        if child.poll() is None:
            os.killpg(child.pid,signal.SIGTERM)
            try:child.wait(timeout=3)
            except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
    same=inputs=={str(p.relative_to(root)):sha(p) for p in sources+packages}
    shutil.rmtree(scratch)
    result={'passed':passed and same,'candidateAssertionsPassed':passed,'sourceUnchanged':same,'inputManifestSHA256':sha(run/'input-hashes.local.json'),
        'sourceCopyArchiveSHA256':sha(run/'source-inputs.tar.gz') if (run/'source-inputs.tar.gz').exists() else None,'commands':commands,'native':native,'independentFrames':frames,'failure':failure,
        'childrenStopped':all(c.poll() is not None for c in children),'scratchAndGeneratedMediaRemoved':not scratch.exists(),
        'scope':'Generated host-native selected hardware VT, strict real Admission tokens and actual packaged Rust validation; one session per positive show. Real callback held externally, native prefix/finish barriers. No source stubs, fallback encoder, camera, phone, runtime pacing, recording/network or physical privacy qualification.'}
    (run/'receipt.local.json').write_text(json.dumps(result,indent=2)+'\n');print(str(run.relative_to(root)));print(json.dumps({'passed':result['passed'],'candidateAssertionsPassed':passed,'sourceUnchanged':same,'failure':failure,'scratchRemoved':not scratch.exists()}))
if not result['passed']:raise SystemExit(1)
PYCODE
