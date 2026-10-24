#!/usr/bin/env python3
"""Explicit real SDK/host-filesystem owner controls; default preflight never builds."""
from pathlib import Path
from zipfile import ZipFile
import argparse,datetime,hashlib,json,os,platform,re,shutil,signal,subprocess,sys,tarfile,tempfile,time,uuid

root=Path(__file__).resolve().parents[2]
assert Path(__file__).resolve()==root/'mobile/android/test-recording-checkpoint-owner.py'
sha=lambda p:hashlib.sha256(Path(p).read_bytes()).hexdigest()
now=lambda:datetime.datetime.now(datetime.timezone.utc).isoformat()
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--proof',type=Path,required=True);parser.add_argument('--proof-sha256',required=True)
parser.add_argument('--execute',action='store_true');parser.add_argument('--compiler-released',action='store_true')
args=parser.parse_args()
proof_path=args.proof.resolve();assert proof_path.is_relative_to(root) and not proof_path.is_symlink()
assert sha(proof_path)==args.proof_sha256
proof=json.loads(proof_path.read_text())
def git_state():
    head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True,timeout=30).strip()
    status=subprocess.check_output(['git','status','--porcelain=v1','--untracked-files=all'],cwd=root,text=True,timeout=30)
    assert head==proof['expectedHead'] and status==proof['expectedStatusPorcelain']
    return head,status
head,status=git_state()
assert platform.system()=='Darwin','This driver qualifies the current source-bound macOS dylib only'
assert os.environ.get('CARGO_INCREMENTAL')=='0' and not os.environ.get('CARGO_TARGET_DIR')
def checked(name):
    p=Path(proof[name]);p=p if p.is_absolute() else root/p
    assert '..' not in p.parts and p.is_file() and not p.is_symlink() and sha(p)==proof[name+'SHA256']
    return p
host_path=checked('hostWitness');host=json.loads(host_path.read_text())
assert host['passed'] and host['exit']==0 and host['sourceUnchanged'] and host['childrenStopped']
assert sha(host_path)=='3b83990466107558ae99c5b153f41e8d8e65401ed1c8b3956329152376d1cc49'
assert host['newBuildPerformedByRecorder'] is False
sdk_map=checked('sdkSourceManifest');sdk=json.loads(sdk_map.read_text())
assert len(sdk)==118 and sdk==host['inputs']
assert sha(sdk_map)==host['inputManifestSHA256']
sdk_paths=sorted(set(p for p in [*root.glob('crates/**/src/**/*.rs'),*root.glob('crates/*/Cargo.toml'),*root.glob('crates/*/uniffi.toml'),*root.glob('crates/**/build.rs'),*root.glob('.cargo/*'),root/'THIRD_PARTY_LICENSES.txt',root/'Cargo.toml',root/'Cargo.lock',root/'.cargo/config.toml',root/'scripts/build-native.mjs',root/'scripts/native-kotlin-status-leases.mjs',root/'scripts/third-party-licences.py',root/'rust-toolchain.toml',root/'LICENSE',*root.glob('tests/native/*')] if p.is_file()))
assert set(sdk)=={str(p.relative_to(root)) for p in sdk_paths}
assert all(sha(root/p)==h for p,h in sdk.items())
assert len(host['outputs'])==1 and all(sha(root/p)==h for p,h in host['outputs'].items())
library=root/next(iter(host['outputs']));assert library==root/'target/debug/libsapflow_ffi.dylib'
package_path=checked('androidPackageReceipt');package=json.loads(package_path.read_text())
assert package['passed'] and package['exit']==0 and package['failure'] is None and package['sourceUnchanged'] and package['childrenStopped'] and package['ownedScratchRemoved']
assert package['hostWitnessSHA256']==sha(host_path) and package['newTimingViewAPIChecksPassed'] and package['hostRuntimeUnchanged']
assert json.loads((package_path.parent/'source-hashes.local.json').read_text())==sdk
assert sha(package_path.parent/'build.local.log')==package['logSHA256']
assert all(sha(root/p)==h for p,h in package['outputHashes'].items())
audit_path=checked('androidPackageAudit');audit=json.loads(audit_path.read_text())
assert audit['status']=='clear' and audit['actualReceiptSHA256']==sha(package_path) and audit['sourceInputsVerified']==118
assert audit['hostWitnessSHA256']==sha(host_path) and audit['sourceAndHostCurrentStable']
aar=checked('aar');assert sha(aar)=='649f4c037863210c216c9218e3bae4ba93b03cbda3a231bd8a8cb0572eca423d'
assert package['outputHashes'][str(aar.relative_to(root))]==sha(aar)
tools={name:checked(name) for name in ('jna','serializationCore','serializationJson','androidJar')}
assert sha(tools['jna'])=='4fb141dd8ef6b0585ffceea4bc49602fbc6312fa977e2c488794ea3e6aafecae'
production='mobile/android/app/src/main/java/dev/forgesworn/sapflow/broadcaster/'
sources=[production+n for n in ('CaptureProfile.kt','CaptureIntent.kt','RecordingSourceGate.kt','RecordingCatalogue.kt','RecordingCheckpointStorage.kt','AndroidRecordingStorageIO.kt','RecordingCheckpointOwner.kt')]
sources+=['mobile/android/host-tests/RecordingCheckpointOwnerHostTest.kt']
assert set(proof['compiledSources'])==set(sources)
assert all((root/p).is_file() and not (root/p).is_symlink() and sha(root/p)==h for p,h in proof['compiledSources'].items())
assert proof['driverSHA256']==sha(Path(__file__))
test_names=re.findall(r'::([A-Za-z][A-Za-z0-9_]*)',(root/sources[-1]).read_text().split('val tests = listOf(',1)[1].split(')\n',1)[0])
assert len(test_names)==len(set(test_names))==17
tool_paths={p:sha(p) for p in tools.values()}
context={**sdk,**proof['compiledSources'],str(Path(__file__).relative_to(root)):sha(Path(__file__))}
for p in [proof_path,host_path,sdk_map,package_path,package_path.parent/'source-hashes.local.json',package_path.parent/'build.local.log',audit_path,aar,library]:context[str(p.relative_to(root))]=sha(p)
def unchanged():
    try:return git_state()==(head,status) and all(sha(root/p)==h for p,h in context.items()) and all(sha(p)==h for p,h in tool_paths.items()) and all(sha(root/p)==h for p,h in package['outputHashes'].items())
    except (AssertionError,OSError,ValueError,subprocess.SubprocessError):return False
if not args.execute:
    print(json.dumps({'preflightOnly':True,'gateChildrenStarted':0,'buildStarted':False,'readOnlyGitQueries':True,
        'head':head,'sourceInputCount':len(context),'compiledSources':sources,'hostLibrarySHA256':sha(library),
        'aarSHA256':sha(aar),'controlsDeclaredNotExecuted':test_names,'deviceActions':False}))
    sys.exit(0)
assert args.compiler_released,'Root must release the exclusive serial compiler slot'
base=root/'target/android-sdk-recording-owner-host';base.mkdir(exist_ok=True)
out=base/('run-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ-')+uuid.uuid4().hex[:8]);out.mkdir()
working=None;commands=[]
active=None;launch_in_progress=False;cleanup_in_progress=False;pending_signal=None
results=[];groups=[];cleanup_errors=[];failure=None
def group_alive(pid):
    try:os.killpg(pid,0);return True
    except ProcessLookupError:return False
def stop_group(child):
    if group_alive(child.pid):
        try:os.killpg(child.pid,signal.SIGTERM)
        except ProcessLookupError:pass
        limit=time.monotonic()+2
        while group_alive(child.pid) and time.monotonic()<limit:child.poll();time.sleep(.05)
        if group_alive(child.pid):
            try:os.killpg(child.pid,signal.SIGKILL)
            except ProcessLookupError:pass
        child.wait(timeout=10)
        limit=time.monotonic()+2
        while group_alive(child.pid) and time.monotonic()<limit:time.sleep(.05)
    else:child.wait(timeout=10)
    return not group_alive(child.pid)
def interrupted(number,frame):
    global pending_signal
    if launch_in_progress or cleanup_in_progress:
        if pending_signal is None:pending_signal=number
        return
    raise RuntimeError('Interrupted by signal '+str(number))
for number in (signal.SIGTERM,signal.SIGINT):signal.signal(number,interrupted)
def retire():
    global active,cleanup_in_progress
    if active is None:return True
    owned=active
    if owned['attempted']:return owned['stopped']
    cleanup_in_progress=True;owned['attempted']=True
    try:
        if not stop_group(owned['child']):raise RuntimeError('Owned group remains')
        owned['stopped']=True
    except BaseException as error:cleanup_errors.append(str(error))
    finally:
        groups.append({'name':owned['name'],'pid':owned['child'].pid,'cleanupAttempted':True,'stopped':owned['stopped']})
        if owned['stopped']:active=None
        cleanup_in_progress=False
    return owned['stopped']
def run(name,command,timeout):
    global active,launch_in_progress,pending_signal
    assert active is None and unchanged()
    owned=None;problem=None;log=out/(name+'.local.log');began=time.monotonic()
    try:
        with log.open('xb') as stream:
            launch_in_progress=True
            try:
                child=subprocess.Popen(command,cwd=root,stdout=stream,stderr=subprocess.STDOUT,start_new_session=True,
                    env={**os.environ,'TMPDIR':str(working/'tmp'),'PYTHONDONTWRITEBYTECODE':'1'})
                owned={'child':child,'name':name,'attempted':False,'stopped':False};active=owned
            finally:launch_in_progress=False
            if pending_signal is not None:
                number=pending_signal;pending_signal=None;raise RuntimeError('Deferred launch signal '+str(number))
            code=child.wait(timeout=timeout)
            if code:raise RuntimeError(name+' exited '+str(code))
    except BaseException as error:problem=error
    finally:
        if owned is not None:
            retire();results.append({'name':name,'command':command,'exit':owned['child'].returncode,
                'elapsedSeconds':time.monotonic()-began,'logSHA256':sha(log),'groupStopped':owned['stopped']})
    if problem is not None:raise problem
    if pending_signal is not None:
        number=pending_signal;pending_signal=None;raise RuntimeError('Deferred cleanup signal '+str(number))
    assert owned is not None and owned['stopped'] and unchanged()
    return log.read_text(errors='replace')
began=now();observed=[]
try:
    (out/'source-hashes.local.json').write_text(json.dumps(context,indent=2)+'\n')
    shutil.copyfile(proof_path,out/'approved-proof.local.json')
    with tarfile.open(out/'source-snapshot.tar.gz','w:gz') as archive:
        for name in sources+['mobile/android/test-recording-checkpoint-owner.py']:archive.add(root/name,arcname=name,recursive=False)
    working=Path(tempfile.mkdtemp(prefix='working-',dir=out));(working/'tmp').mkdir();(working/'data').mkdir()
    # Use the ACTUAL current AAR classes; never load its Android .so on macOS.
    with ZipFile(aar) as archive:
        assert len(archive.namelist())==len(set(archive.namelist()))
        item=archive.getinfo('classes.jar');assert 0<item.file_size<=16*1024*1024
        (working/'classes.jar').write_bytes(archive.read(item))
    classpath=os.pathsep.join(str(p) for p in [working/'classes.jar',*tools.values()])
    commands=[('compile',['kotlinc','-language-version','2.2','-api-version','2.2','-jvm-target','17',
        '-J-Djava.io.tmpdir='+str(working/'tmp'),*[str(root/p) for p in sources],'-cp',classpath,
        '-include-runtime','-d',str(working/'controls.jar')],180),
        ('controls',['java','-Djava.io.tmpdir='+str(working/'tmp'),'-Djna.library.path='+str(library.parent),
        '-Duniffi.component.sapflow_ffi.libraryOverride='+str(library),
        '-cp',str(working/'controls.jar')+os.pathsep+classpath,
        'dev.forgesworn.sapflow.broadcaster.RecordingCheckpointOwnerHostTestKt',str(working/'data')],180)]
    for name,command,timeout in commands:
        text=run(name,command,timeout)
        if name=='controls':
            observed=re.findall(r'^SDK recording owner host control passed: (\w+)$',text,re.M)
            assert observed==test_names and text.splitlines().count('SDK recording owner host controls passed: 17')==1
except BaseException as error:failure=type(error).__name__+': '+str(error)
finally:
    if active is not None and not active['attempted']:retire()
    stopped=active is None and all(g['stopped'] for g in groups)
    if stopped and working is not None:
        try:shutil.rmtree(working)
        except BaseException as error:cleanup_errors.append(str(error))
    elif not stopped:cleanup_errors.append('Preserved generated synthetic data/scratch while owned group cleanup is unconfirmed')
    same=unchanged()
    receipt={'passed':failure is None and same and len(results)==2 and stopped and not cleanup_errors and (working is None or not working.exists()) and pending_signal is None,
        'startedUTC':began,'endedUTC':now(),'headBefore':head,'expectedStatusPorcelain':status,'proofSHA256':args.proof_sha256,
        'sourceUnchanged':same,'commands':results,'actualPassedControls':observed,'terminalProcessGroups':groups,
        'childrenStopped':stopped,'ownedWorkingAndGeneratedMediaRemoved':working is None or not working.exists(),'cleanupErrors':cleanup_errors,
        'failure':failure,'pendingDeferredSignal':pending_signal,'unconfirmedOwnedGroupPID':active['child'].pid if active is not None else None,
        'hostLibrarySHA256':sha(library),'aarSHA256':sha(aar),'sourceInputCount':len(context),
        'sourceSnapshotSHA256':sha(out/'source-snapshot.tar.gz') if (out/'source-snapshot.tar.gz').is_file() else None,'deviceActions':False,'packagesRebuilt':False,
        'scope':'Actual current SDK/AAR Kotlin classes + macOS host dylib and real host filesystem storage policy controls with synthetic compressed bytes and explicitly supplied scalar receipts. Not Android Os/proc/SELinux, native AAC facts, decoder, capture authority, recorder/default adoption, recovery, aggregate loss or performance qualification.'}
    (out/'receipt.local.json').write_text(json.dumps(receipt,indent=2)+'\n')
    print(json.dumps({'receipt':str((out/'receipt.local.json').relative_to(root)),'sha256':sha(out/'receipt.local.json'),'passed':receipt['passed']}),flush=True)
sys.exit(0 if receipt['passed'] else 1)
