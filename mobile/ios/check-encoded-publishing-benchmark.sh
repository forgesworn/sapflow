#!/bin/bash
# AAC-only offline host paired benchmark; no capture, sockets or app builds.
set -euo pipefail
if [ "$#" -ne 0 ]; then echo 'Usage: check-encoded-publishing-benchmark.sh' >&2; exit 2; fi
root="$(cd "$(dirname "$0")/../.." && pwd)"
umask 077
python3 -u - "$root" <<'PYCODE'
from pathlib import Path
import datetime,hashlib,json,os,platform,shutil,signal,subprocess,sys,tarfile,tempfile,time,uuid
root=Path(sys.argv[1]);run=root/'target/encoded-publishing-benchmark'/(datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ-')+uuid.uuid4().hex[:8]);run.mkdir(parents=True,mode=0o700)
scratch=Path(tempfile.mkdtemp(prefix='sapflow-publishing-benchmark-',dir=root/'target'))
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
names=['mobile/ios/Sources/EncodedPipeline.swift','mobile/ios/Sources/PublishingTelemetry.swift','mobile/ios/Sources/EncodedWorkBudget.swift','mobile/ios/Tools/EncodedPublishingBenchmark.swift','mobile/ios/check-encoded-publishing-benchmark.sh']
context=['docs/native-performance-contract.md','docs/mobile-sdk-contract.md','crates/sapflow-ffi/src/media.rs','crates/sapflow-client/src/queue.rs']
package=root/'target/native-packages/apple';pkg=sorted(p for p in package.rglob('*') if p.is_file());assert pkg and len(pkg)<=64 and not any(p.is_symlink() for p in pkg)
inputs={str(p.relative_to(root)):sha(p) for p in [root/n for n in names+context]+pkg};(run/'input-hashes.local.json').write_text(json.dumps(inputs,indent=2)+'\n')
plan={'scope':'AAC-only generated host Release paired microbenchmark, no wholeapp/physical/energy claim','compilerJobs':2,'compileTimeoutSeconds':180,'nativeTimeoutSeconds':20,'innerRuntimeDeadlineSeconds':12,'poolAUs':128,'warmupPairsPerScope':32,'measuredPairsPerScope':400,'order':'alternating AB/BA','clocks':['monotonic wall','CLOCK_THREAD_CPUTIME_ID'],'timingUnit':'per128-AU-batch means perAU; not event latency quantiles','stability':'Four100-pair CPU-delta means max-min <=max(500ns/AU,25% absolute pairedmedian). No tuning/rerun to pass.','allocations':'Uninstrumented; source-derived labels only','bounds':'128 prepared AUs/<=4MiB; actual publishing queue atmost1; common budget128/4MiB/1s correctness outside timing'}
(run/'predeclared-plan.local.json').write_text(json.dumps(plan,indent=2)+'\n');print(str(run.relative_to(root)),flush=True)
children=[];commands=[];failure=None;result=None;passed=False;timingStart=None;timingEnd=None
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

def stop(signum,frame):raise RuntimeError('Benchmark interrupted '+str(signum))
for sig in [signal.SIGTERM,signal.SIGINT]:signal.signal(sig,stop)
try:
    dest=scratch/'Sources/EncodedPublishingBenchmark';dest.mkdir(parents=True)
    for name in names[:-1]:shutil.copy2(root/name,dest/Path(name).name)
    with tarfile.open(run/'source-inputs.tar.gz','w:gz') as archive:
        for p in sorted(dest.iterdir()):archive.add(p,arcname='Sources/EncodedPublishingBenchmark/'+p.name,recursive=False)
    assert inputs=={n:sha(root/n) for n in inputs},'Inputs changed during copy'
    (scratch/'Package.swift').write_text('''// swift-tools-version: 5.9
import PackageDescription
let package = Package(name: "EncodedPublishingBenchmark", platforms: [.macOS(.v12)], dependencies: [.package(name: "Sapflow", path: PACKAGE)], targets: [.executableTarget(name: "EncodedPublishingBenchmark", dependencies: [.product(name: "Sapflow", package: "Sapflow")], swiftSettings: [.unsafeFlags(["-warnings-as-errors"])])])
'''.replace('PACKAGE',json.dumps(str(package))))
    invoke(['swift','build','-c','release','--package-path',str(scratch),'--scratch-path',str(scratch/'build'),'--jobs','2','--disable-sandbox','--disable-automatic-resolution'],'build.local.log',180)
    generated=scratch/'generated';generated.mkdir();(generated/'.sapflow-publishing-benchmark').write_text('generated-publishing-benchmark-v1\n')
    media=generated/'generated.m4a'
    invoke(['ffmpeg','-nostdin','-v','error','-xerror','-threads','1','-filter_threads','1','-f','lavfi','-i','aevalsrc=0.25*sin(2*PI*(113*t+173*t*t))+0.18*sin(2*PI*(997*t+71*t*t)):s=48000:d=3','-ac','1','-c:a','aac','-b:a','96k',str(media)],'generation.local.log',15)
    facts=json.loads(invoke(['ffprobe','-v','error','-show_streams','-count_packets','-of','json',str(media)],'generated-probe.local.json',10).read_text())
    track=facts['streams'][0];assert len(facts['streams'])==1 and track['codec_name']=='aac' and track['profile']=='LC' and track['sample_rate']=='48000' and track['channels']==1 and int(track['nb_read_packets'])>=128
    (run/'generated-source.local.json').write_text(json.dumps({'sha256':sha(media),'bytes':media.stat().st_size,'framesRequested':144000,'source':'Explicit lavfi nonperiodic chirp; AAC-LC48k mono96kbps. Pool timings start at0 independently on both paths; no original storedpayload is modified.'},indent=2)+'\n')
    for args,name in [(['swift','--version'],'swift-version.local.log'),(['ffmpeg','-version'],'ffmpeg-version.local.log'),(['sysctl','-n','machdep.cpu.brand_string'],'hardware.local.log'),(['sw_vers'],'os.local.log')]:invoke(args,name,10)
    (run/'timing-ready.local.json').write_text(json.dumps({'readyUTC':datetime.datetime.now(datetime.timezone.utc).isoformat(),'coordinated':os.environ.get('SAPFLOW_BENCHMARK_COORDINATED')=='1'})+'\n')
    print('TIMING_READY '+str(run.relative_to(root)),flush=True)
    if os.environ.get('SAPFLOW_BENCHMARK_COORDINATED')=='1':
        deadline=time.monotonic()+60
        while not (run/'timing-go.local').exists():
            if time.monotonic()>=deadline:raise RuntimeError('Exclusive timing coordination deadline')
            time.sleep(0.1)
    timingStart=datetime.datetime.now(datetime.timezone.utc).isoformat();print('TIMING_START '+timingStart,flush=True)
    result=json.loads(invoke([str(scratch/'build/release/EncodedPublishingBenchmark'),str(generated)],'runtime.local.json',20,accepted=(0,1)).read_text())
    timingEnd=datetime.datetime.now(datetime.timezone.utc).isoformat()
    for p in generated.glob('*-measurements.local.json'):shutil.copy2(p,run/p.name)
    assert result['passed'] and result['correctnessPassed'] and result['predeclaredStabilityPassed'],result
    assert result['poolAUs']==128 and result['nativeCompressedPoolBytes']<=4*1024*1024
    assert len(result['measurements'])==2 and all(row['pairs']==400 and row['batchAUs']==128 and row['warmupPairs']==32 for row in result['measurements'])
    passed=True
except BaseException as error:failure={'type':type(error).__name__,'reason':str(error)[:1200]}
finally:
    for child in children:
        if child.poll() is None:
            os.killpg(child.pid,signal.SIGTERM)
            try:child.wait(timeout=3)
            except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
    for p in scratch.glob('generated/*-measurements.local.json'):shutil.copy2(p,run/p.name)
    same=inputs=={n:sha(root/n) for n in inputs}
    shutil.rmtree(scratch)
    receipt={'passed':passed and same,'candidateAssertionsPassed':passed,'sourceUnchanged':same,'planSHA256':sha(run/'predeclared-plan.local.json'),'inputManifestSHA256':sha(run/'input-hashes.local.json'),
        'sourceArchiveSHA256':sha(run/'source-inputs.tar.gz') if (run/'source-inputs.tar.gz').exists() else None,'commands':commands,'result':result,'failure':failure,
        'timingStartUTC':timingStart,'timingEndUTC':timingEnd,'childrenStopped':all(c.poll() is not None for c in children),'scratchAndGeneratedMediaRemoved':not scratch.exists(),
        'scope':'Paired Release host-only generated AAC Swift/installedFFI receipt overhead. No physical, camera/video/recorder/network/energy/binding10percentwholeSDK acceptance. Allocation counts uninstrumented; source labels only.'}
    (run/'receipt.local.json').write_text(json.dumps(receipt,indent=2)+'\n');print('TERMINAL '+json.dumps({'passed':receipt['passed'],'sourceUnchanged':same,'failure':failure,'scratchRemoved':not scratch.exists()}),flush=True)
if not receipt['passed']:raise SystemExit(1)
PYCODE
