#!/usr/bin/env python3
"""Generated native drain matrix: source conservation, exact raw parser, native container.
Default is read-only preflight. No leading/tail or producer receipt is fabricated.
"""
from pathlib import Path
import argparse, contextlib, hashlib, json, os, resource, runpy, signal, subprocess, sys, time
sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--input',type=Path,required=True)
parser.add_argument('--receipt-sha256',required=True)
parser.add_argument('--output',type=Path,required=True)
parser.add_argument('--raw-build-receipt',type=Path,required=True)
parser.add_argument('--raw-build-sha256',required=True)
parser.add_argument('--raw-controls-receipt',type=Path,required=True)
parser.add_argument('--raw-controls-sha256',required=True)
parser.add_argument('--raw-validator',type=Path,required=True)
parser.add_argument('--raw-validator-sha256',required=True)
parser.add_argument('--execute',action='store_true')
args=parser.parse_args()
repo=Path(__file__).resolve().parents[3]
source=args.input.resolve()
assert source.is_dir() and not args.input.is_symlink()
assert sha(source/'receipt.json')==args.receipt_sha256
root=json.loads((source/'receipt.json').read_text())
assert root['schema']=='generated-native-aac-drain-experiment-v1' and root['mode']=='DRAIN_EXPERIMENTS'
assert root['generatedOnly'] and root['emulatorOnly'] and root['hardware'] in ('ranchu','goldfish')
assert root['cleanupConfirmed'] and root['allCasesVisited'] and not root['cancelled']
assert root['nativeAacQualified'] is False and root['sdkReceiptIssued'] is False
matrix=[]
for short in (False,True):
    for policy in range(5):
        for last in (False,True):
            programme='mixed-short' if short else 'raw-one-second'
            treatment=['no-pad','block-pad','drain-1024','drain-2048','drain-4096'][policy]
            name=programme+'-'+treatment+('-last-nonempty-eos' if last else '-empty-eos')
            frames,raw,zero=(2765,1517,1248) if short else (48000,48000,0)
            padding=(1024-frames%1024)%1024 if policy else 0
            drain=[0,0,1024,2048,4096][policy]
            matrix.append((name,programme,frames,raw,zero,padding,drain,last))
assert [row['name'] for row in root['cases']]==[row[0] for row in matrix]
inputs={};count=0
for p in source.rglob('*'):
    count+=1;assert count<=200 and len(p.relative_to(source).parts)<=2 and not p.is_symlink()
    if p.is_dir():continue
    assert p.is_file() and p.stat().st_size<=4*1024*1024
    inputs[str(p.relative_to(source))]=sha(p)
assert sum((source/name).stat().st_size for name in inputs)<=64*1024*1024
programme_hashes={};cases=[]
for index,(name,programme,frames,raw,zero,padding,drain,last) in enumerate(matrix):
    directory=source/name;record=json.loads((directory/'receipt.json').read_text())
    assert record==root['cases'][index] and record['failure'] is None
    assert record['cleanupConfirmed'] and record['nativeStopReturned'] and record['nativeReleaseReturned']
    assert record['callbackBarrier'] and record['callbackOwnerDrained'] and record['streamsClosed']
    assert record['inputEosQueued'] and record['outputEosObserved'] and record['classification']=='OBSERVED_NATIVE_EOS'
    assert record['auCardinality']=='UNKNOWN' and record['nativeAacQualified'] is False
    assert record['inputOriginUs']==0 and record['nativeQueuedFramesConserved']
    assert (record['programmeFrames'],record['rawFrames'],record['zeroFrames'],record['blockPaddingFrames'],record['codecDrainFrames'])==(frames,raw,zero,padding,drain)
    assert record['inputFrames']==frames+padding+drain<64*1024
    assert record['plannedProgrammeFrames']==frames and record['plannedBlockPaddingFrames']==padding and record['plannedCodecDrainFrames']==drain
    assert record['inputEosPlacement']==('LAST_NONEMPTY' if last else 'SEPARATE_EMPTY')
    native=(directory/'source.s16le').read_bytes();show=(directory/'programme.s16le').read_bytes()
    assert len(native)==record['inputFrames']*2 and len(show)==frames*2
    assert native[:len(show)]==show and not any(native[len(show):]),'Pad/drain are not exact generated zero suffixes'
    digest=sha(directory/'programme.s16le')
    assert programme_hashes.setdefault(programme,digest)==digest,'Programme bytes changed across treatment or EOS placement'
    queued=[json.loads(line) for line in (directory/'inputs.jsonl').read_text().splitlines()]
    assert len(queued)<=65 and [row['sequence'] for row in queued]==list(range(1,len(queued)+1))
    blocks=[row for row in queued if row['type']=='SOURCE'];ends=[row for row in queued if row['type']=='EOS']
    assert len(blocks)==record['inputBuffers'] and 0<len(blocks)<=64
    assert len(ends)==int(not last)
    assert all(row['flags']==0 for row in blocks[:-1])
    assert blocks[-1]['flags']==(4 if last else 0)
    if ends:assert ends[0]['flags']==4 and ends[0]['frames']==0 and ends[0]['queueReturned']
    cursor=0;programme_cursor=0;totals=[0,0,0,0]
    for row in blocks:
        assert row['queueReturned'] and row['sourceByteOffset']==cursor and 1<=row['frames']<=1024
        components=[row['rawFrames'],row['zeroFrames'],row['blockPaddingFrames'],row['codecDrainFrames']]
        assert all(x>=0 for x in components) and sum(components)==row['frames']
        assert row['programmeFrames']==sum(components[:2]) and row['programmeByteOffset']==programme_cursor
        end=cursor+row['frames']*2;blob=native[cursor:end]
        assert hashlib.sha256(blob).hexdigest()==row['sourceSha256']
        programme_end=row['programmeFrames']*2;padding_end=programme_end+row['blockPaddingFrames']*2
        assert hashlib.sha256(blob[:programme_end]).hexdigest()==row['programmeSha256']
        assert hashlib.sha256(blob[programme_end:padding_end]).hexdigest()==row['blockPaddingSha256']
        assert hashlib.sha256(blob[padding_end:]).hexdigest()==row['codecDrainSha256']
        programme_cursor+=programme_end;cursor=end;totals=[a+b for a,b in zip(totals,components)]
    assert cursor==len(native) and programme_cursor==len(show) and totals==[raw,zero,padding,drain]
    rows=[json.loads(line) for line in (directory/'outputs.jsonl').read_text().splitlines()]
    assert len(rows)==record['outputCallbacks']<=256
    assert record['outputCallbacks']==record['outputReleaseAttempts']==record['outputReleaseReturned']
    assert [row['sequence'] for row in rows]==list(range(1,len(rows)+1))
    payload=(directory/'buffers.bin').read_bytes();position=0
    for row in rows:
        assert row['releaseReturned'] and 'failure' not in row and row['payloadOffset']==position
        if row['size']>0:
            end=position+row['size'];assert end<=len(payload)
            assert hashlib.sha256(payload[position:end]).hexdigest()==row['payloadSha256'];position=end
    assert position==len(payload)
    assert record['containerControlFailure'] is None and record['nativeMuxerStopReturned'] and record['nativeMuxerReleaseReturned']
    cases.append((name,record,rows,frames,padding,drain,last))

# Bind the actual previously built and negative-controlled parser; never build it here.
for p,digest in [(args.raw_build_receipt,args.raw_build_sha256),(args.raw_controls_receipt,args.raw_controls_sha256),(args.raw_validator,args.raw_validator_sha256)]:
    assert p.is_file() and not p.is_symlink() and sha(p)==digest
build=json.loads(args.raw_build_receipt.read_text());controls=json.loads(args.raw_controls_receipt.read_text())
assert build['passed'] and build['childrenStopped'] and not build['cleanupErrors']
witness=repo/build['product'];assert sha(witness)==build['productSHA256']=='8a116784a652f0f2854f21215eb4b4080cf745057decfd3d9049b1b2cf1d0336'
# The .a inputs were audited at build time and intentionally cleaned up.
# The retained executable is statically linked; bind that immutable history,
# rather than requiring a decoder rebuild or deleted scratch to reappear.
proof_base=repo/'target/privacy-standby/android-native-aac-qualification-v1'
historical_specs={
    'core':('independent-oracle-v1/manifest.core-v1.local.json','102794dbc157f897e8ac76049f844ea2beab290be811471ae763a595a083b49d'),
    'source':('decoder-source-v1/receipt.local.json','f475d25d39421afb1ba5fede956e10d9a3746406e6ed35432124702cb4dd9f1f'),
    'audit':('raw-decoder-controls-v1/independent-outcome-review.local.json','5df2f85e1659234c5acecedc7249678a129991a6f88657fc8c9523dcc9987c01'),
    'cleanup':('decoder-source-v1/scratch-cleanup.local.json','6875b457a67639aecb1b111db578b2d39580ca8c73d202d51315e07e3c1eb231'),
}
historical={}
for key,(relative,digest) in historical_specs.items():
    p=proof_base/relative;assert p.is_file() and not p.is_symlink() and sha(p)==digest
    historical[key]=json.loads(p.read_text())
core=historical['core'];audit=historical['audit'];cleanup=historical['cleanup']
assert args.raw_build_sha256==audit['buildReceiptSHA256']=='8b1926be47f4ea4a2d19a61b49da349e3a8d36ac5a8f91d4c31ec3790cfe3f05'
assert args.raw_controls_sha256==audit['rawControlReceiptSHA256']=='771b14c7149967c3dfc78e63ec97c4d3999f6579ba25d1386a39f6082842fb30'
assert audit['status']=='CLEAR' and audit['exactPatchedAACAndAllCoreHashesMatch']
assert audit['coreManifestSHA256']==historical_specs['core'][1] and audit['pinnedSourceReceiptSHA256']==historical_specs['source'][1]
assert audit['driverBinarySHA256']==sha(witness) and audit['actualMachODependencies']==['/usr/lib/libSystem.B.dylib']
assert build['driverSourceSHA256']==core['paths']['aac_raw_witness.c']['sha256']
assert build['aacPatchedSourceSHA256']==core['paths']['candidate/aacdec.c']['sha256']
assert build['sourceCommit']==core['pinnedCommit']==historical['source']['commit']
assert args.raw_validator_sha256==core['paths']['validate_raw_witness.py']['sha256']
assert cleanup['retainedStaticOracleSHA256']==sha(witness) and cleanup['retainedStaticOracle']==build['product']
assert cleanup['coreBuildAndControlReceiptsRetained'] and not cleanup['sharedRustOrAndroidCachesTouched']
assert controls['passed'] and controls['driverSHA256']==sha(witness) and controls['validatorSHA256']==args.raw_validator_sha256
assert all(row['accepted']==row['expectedAccepted'] for row in controls['cases'])
assert {row['name'] for row in controls['cases'] if not row['accepted']} >= {'packed-two','truncated-one-byte','truncated-half','trailing-nonzero','trailing-zero'}
retained_proofs=[proof_base/relative for relative,_ in historical_specs.values()]
proof_inputs={str(p.resolve()):sha(p) for p in [args.raw_build_receipt,args.raw_controls_receipt,args.raw_validator,witness,*retained_proofs]}
if not args.execute:
    print(json.dumps({'preflightOnly':True,'toolChildrenStarted':0,'matrixCases':len(cases),'nativeSourceInputsVerified':len(inputs),'programmeHashes':programme_hashes,'oneAuPerBufferProven':False,'nativeAacQualified':False}))
    sys.exit(0)
out=args.output.resolve();assert not out.exists();out.mkdir(parents=True)
(out/'input-hashes.local.json').write_text(json.dumps(inputs,indent=2)+'\n')
(out/'parser-proof-hashes.local.json').write_text(json.dumps(proof_inputs,indent=2)+'\n')
validate=runpy.run_path(str(args.raw_validator))['inspect']
active = None
launching = False
cleaning = False
pending_signal = None
commands = []
cleanup_errors = []
terminal_groups = []
deadline = time.monotonic()+1200

def limit_child_files():
    resource.setrlimit(resource.RLIMIT_FSIZE,(4*1024*1024,4*1024*1024))

def alive(pid):
    try: os.killpg(pid,0); return True
    except ProcessLookupError: return False

def interrupted(number, frame):
    global pending_signal
    if launching or cleaning:
        if pending_signal is None: pending_signal = number
        return
    raise RuntimeError('Interrupted by signal '+str(number))
for number in (signal.SIGTERM,signal.SIGINT): signal.signal(number,interrupted)

def retire():
    global active,cleaning
    if active is None: return True
    owned = active
    if owned['attempted']: return owned['stopped']
    cleaning = True; owned['attempted'] = True
    child = owned['child']
    try:
        if alive(child.pid):
            try: os.killpg(child.pid,signal.SIGTERM)
            except ProcessLookupError: pass
            limit = time.monotonic()+2
            while alive(child.pid) and time.monotonic()<limit: child.poll(); time.sleep(.05)
            if alive(child.pid):
                try: os.killpg(child.pid,signal.SIGKILL)
                except ProcessLookupError: pass
        child.wait(timeout=10)
        limit = time.monotonic()+2
        while alive(child.pid) and time.monotonic()<limit: time.sleep(.05)
        owned['stopped'] = not alive(child.pid)
        if not owned['stopped']: raise RuntimeError('Owned decoder tool group remains')
    except BaseException as error: cleanup_errors.append(str(error))
    finally:
        terminal_groups.append({'name':owned['name'],'pid':child.pid,'stopped':owned['stopped']})
        if owned['stopped']: active = None
        cleaning = False
    return owned['stopped']

def run(name, command, allow_failure=False, stdin_path=None):
    global active,launching,pending_signal
    assert active is None, 'Earlier owned decoder group cleanup unconfirmed'
    owned = None; problem = None
    stdout = out/(name+'.stdout'); stderr = out/(name+'.stderr')
    try:
        with stdout.open('xb') as output, stderr.open('xb') as errors, (stdin_path.open('rb') if stdin_path else contextlib.nullcontext(None)) as supplied:
            launching = True
            try:
                child = subprocess.Popen(command,stdin=supplied if supplied else subprocess.DEVNULL,stdout=output,stderr=errors,start_new_session=True,preexec_fn=limit_child_files)
                owned = {'child':child,'name':name,'attempted':False,'stopped':False}; active = owned
            finally: launching = False
            if pending_signal is not None: raise RuntimeError('Deferred signal '+str(pending_signal))
            remaining = min(45, deadline-time.monotonic())
            assert remaining > 0, "Global decoder inspection deadline"
            code = child.wait(timeout=remaining)
    except BaseException as error: problem = error
    finally:
        if owned is not None:
            retire()
            commands.append({'name':name,'command':command,'exit':owned['child'].returncode,
                'stdoutSHA256':sha(stdout),'stderrSHA256':sha(stderr),'groupStopped':owned['stopped']})
    if problem is not None: raise problem
    assert owned['stopped'] and not cleanup_errors and pending_signal is None
    assert stdout.stat().st_size <= 4*1024*1024 and stderr.stat().st_size <= 1024*1024
    if not allow_failure: assert code == 0, name+' failed; original logs retained'
    return code,stdout,stderr

results=[];failure=None
try:
    for name,record,rows,programme,padding,drain,last in cases:
        spans=[(row['sequence'],row['payloadOffset'],row['size']) for row in rows if row['size']>0 and not row['config']]
        assert spans,'No original native data output'
        manifest=out/(name+'.spans');manifest.write_text(''.join('%d %d %d\n'%span for span in spans))
        pcm=out/(name+'.raw.f32le')
        code,raw_stdout,raw_stderr=run(name+'-raw',[str(witness),str(source/name/'buffers.bin'),str(pcm)],stdin_path=manifest)
        exact=validate((source/name/'buffers.bin').read_bytes(),spans,raw_stdout.read_bytes(),raw_stderr.read_bytes(),pcm.read_bytes(),code)
        assert exact['selectedProfilePackingObserved'] and exact['exactParsedByteCoverage']
        path=source/name/'native.mp4'
        _,packet_output,_=run(name+'-packets',['ffprobe','-v','error','-show_streams','-show_packets',
            '-show_data_hash','sha256','-of','json',str(path)])
        facts=json.loads(packet_output.read_text());assert len(facts['streams'])==1
        stream=facts['streams'][0]
        assert stream['codec_name']=='aac' and stream['sample_rate']=='48000' and stream['channels']==1
        packets=facts.get('packets',[]);expected=[row for row in rows if row['muxed']]
        assert len(packets)==len(expected)==len(spans),'Native container omitted or merged original buffers'
        for packet,original in zip(packets,expected):
            assert int(packet['size'])==original['size'] and packet['data_hash'].lower()=='sha256:'+original['payloadSha256']
        _,frame_output,_=run(name+'-frames',['ffprobe','-v','error','-show_frames',
            '-show_entries','frame=media_type,nb_samples,pkt_pos,pts,best_effort_timestamp','-of','json',str(path)])
        decoded_frames=json.loads(frame_output.read_text()).get('frames',[])
        assert all(frame['media_type']=='audio' and int(frame['nb_samples'])>0 for frame in decoded_frames)
        presented=out/(name+'.native-container.s16le')
        run(name+'-decode',['ffmpeg','-nostdin','-v','error','-xerror','-i',str(path),'-map','0:a:0',
            '-c:a','pcm_s16le','-f','s16le','-fs','2097152',str(presented)])
        assert presented.stat().st_size<2097152 and presented.stat().st_size%2==0
        coded=exact['decodedCodedFrames']
        results.append({'name':name,'programmeFrames':programme,'blockPaddingFrames':padding,'codecDrainFrames':drain,
            'nativeQueuedFrames':record['inputFrames'],'eosPlacement':record['inputEosPlacement'],
            'exactRawPacking':exact,'decodedCodedFrames':coded,'codedMinusProgrammeFrames':coded-programme,
            'nativeQueuedMinusCodedFrames':record['inputFrames']-coded,
            'programmeSHA256':sha(source/name/'programme.s16le'),'nativeQueuedPCMSHA256':sha(source/name/'source.s16le'),
            'nativeContainerOriginalPackets':len(packets),'nativeContainerDecodedFrameSamples':[int(frame['nb_samples']) for frame in decoded_frames],
            'nativeContainerPresentedFrames':presented.stat().st_size//2,'nativeContainerPCMSHA256':sha(presented),
            'programmeWindowClassification':'DEFINITELY_SHORTER_THAN_PROGRAMME' if coded<programme else 'NEEDS_NATIVE_LEADING_AND_VALID_WINDOW_FACTS',
            'leadingFrames':'UNKNOWN','finalTailFrames':'UNKNOWN','drainModelQualified':False})
    pairs=[]
    for index in range(0,len(results),2):
        empty,last=results[index:index+2]
        assert empty['nativeQueuedPCMSHA256']==last['nativeQueuedPCMSHA256']
        assert empty['programmeSHA256']==last['programmeSHA256']
        pairs.append({'emptyEosCase':empty['name'],'lastNonemptyEosCase':last['name'],
            'identicalNativeQueuedPCM':True,'lastMinusEmptyDecodedCodedFrames':last['decodedCodedFrames']-empty['decodedCodedFrames']})
    (out/'eos-placement-pairs.local.json').write_text(json.dumps(pairs,indent=2)+'\n')
except BaseException as error:failure=type(error).__name__+': '+str(error)
finally:
    if active is not None and not active['attempted']:retire()
    same=all((source/name).is_file() and sha(source/name)==digest for name,digest in inputs.items())
    proofs_same=all(Path(name).is_file() and sha(Path(name))==digest for name,digest in proof_inputs.items())
    result={'inspectionCompleted':failure is None and len(results)==20 and same and proofs_same and active is None and not cleanup_errors and pending_signal is None,
        'failure':failure,'matrixResults':results,'commands':commands,'terminalProcessGroups':terminal_groups,
        'childrenStopped':active is None and all(row['stopped'] for row in terminal_groups),'cleanupErrors':cleanup_errors,
        'unconfirmedOwnedGroupPID':active['child'].pid if active is not None else None,'pendingDeferredSignal':pending_signal,
        'inputsUnchanged':same,'parserProofsUnchanged':proofs_same,'sourceReceiptSHA256':args.receipt_sha256,
        'scriptSHA256':sha(Path(__file__)),'rawWitnessSHA256':sha(witness),'globalDeadlineSeconds':1200,'perToolTimeoutSeconds':45,
        'nativeAacQualified':False,'drainModelQualified':False,'nativeGenerationReceiptIssued':False,'deviceActions':False,
        'scope':'Actual selected-emulator generated programme/padding/drain experiment with exact parsed raw AAC byte coverage, actual decoder extents and native-container packet/decode controls. Source conservation and EOS placement are measured separately. No inferred native first-leading/tail, programme-valid presentation, producer or app-finalisation authority.'}
    (out/'receipt.local.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({'receipt':str(out/'receipt.local.json'),'sha256':sha(out/'receipt.local.json'),
        'inspectionCompleted':result['inspectionCompleted'],'drainModelQualified':False}))
sys.exit(0 if result['inspectionCompleted'] else 1)
