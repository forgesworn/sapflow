#!/usr/bin/env python3
"""Inspect extracted generated-only native AAC observations. Default runs no tool.
Decoder output is file-specific evidence, never native AU cardinality or trim proof.
"""
from pathlib import Path
import argparse, hashlib, json, os, re, resource, signal, subprocess, sys, time

sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--input', type=Path, required=True)
parser.add_argument('--receipt-sha256', required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--execute', action='store_true')
args = parser.parse_args()
source = args.input.resolve()
assert source.is_dir() and not args.input.is_symlink()
receipt = source/'receipt.json'
assert sha(receipt) == args.receipt_sha256
root_receipt = json.loads(receipt.read_text())
assert root_receipt['schema'] == 'generated-native-aac-observation-v1'
assert root_receipt['generatedOnly'] and root_receipt['emulatorOnly']
assert root_receipt['hardware'] in ('ranchu', 'goldfish')
assert root_receipt['nativeAacQualified'] is False and root_receipt['sdkReceiptIssued'] is False
assert root_receipt['cleanupConfirmed'] and root_receipt['allCasesVisited'] and not root_receipt['cancelled']
names = ['raw-one-second', 'mixed-short', 'generated-zero', 'signed-origin', 'missing-input-eos']
assert [row['name'] for row in root_receipt['cases']] == names
expected_frames = [(48000,48000,0), (2765,1517,1248), (9600,0,9600), (4096,4096,0), (4096,4096,0)]
inputs = {}
entries = 0
for p in source.rglob('*'):
    entries += 1
    assert entries <= 96 and len(p.relative_to(source).parts) <= 2
    assert not p.is_symlink(), 'Symlink in extracted evidence'
    if p.is_dir(): continue
    assert p.is_file() and p.stat().st_size <= 4*1024*1024
    inputs[str(p.relative_to(source))] = sha(p)
    assert len(inputs) <= 64
assert sum((source/name).stat().st_size for name in inputs) <= 16*1024*1024

cases = []
for index, (name, expected) in enumerate(zip(names, expected_frames)):
    directory = source/name
    record = json.loads((directory/'receipt.json').read_text())
    assert record == root_receipt['cases'][index]
    assert record['cleanupConfirmed'] and record['nativeReleaseReturned'] and record['nativeStopReturned']
    assert record['callbackBarrier'] and record['callbackOwnerDrained'] and record['streamsClosed']
    assert record['failure'] is None and record['auCardinality'] == 'UNKNOWN'
    assert (record['inputFrames'],record['rawFrames'],record['zeroFrames']) == expected
    pcm = directory/'source.s16le'
    assert pcm.stat().st_size == 2*record['inputFrames']
    submitted = [json.loads(line) for line in (directory/'inputs.jsonl').read_text().splitlines()]
    assert len(submitted) <= 65 and [row['sequence'] for row in submitted] == list(range(1,len(submitted)+1))
    blocks = [row for row in submitted if row['type']=='SOURCE']
    ends = [row for row in submitted if row['type']=='EOS']
    assert len(blocks)==record['inputBuffers'] and len(ends)==int(record['inputEosQueued'])
    assert sum(row['rawFrames'] for row in blocks)==record['rawFrames']
    assert sum(row['zeroFrames'] for row in blocks)==record['zeroFrames']
    cursor = 0; source_bytes = pcm.read_bytes()
    for row in blocks:
        assert row['queueReturned'] and row['sourceByteOffset']==cursor and row['frames'] in range(1,1025)
        assert row['rawFrames']+row['zeroFrames']==row['frames']
        end = cursor+row['frames']*2
        assert hashlib.sha256(source_bytes[cursor:end]).hexdigest()==row['sourceSha256']
        cursor = end
    assert cursor==len(source_bytes)
    rows = [json.loads(line) for line in (directory/'outputs.jsonl').read_text().splitlines()]
    assert len(rows) <= 256 and len(rows) == record['outputCallbacks']
    assert record['outputCallbacks'] == record['outputReleaseAttempts'] == record['outputReleaseReturned']
    assert [r['sequence'] for r in rows] == list(range(1,len(rows)+1))
    payload = (directory/'buffers.bin').read_bytes()
    position = 0
    for row in rows:
        assert row['releaseReturned'] and 'failure' not in row
        assert row['payloadOffset'] == position and row['auCardinality'] == 'UNKNOWN'
        if row['size'] > 0:
            end = position + row['size']
            assert end <= len(payload)
            assert hashlib.sha256(payload[position:end]).hexdigest() == row['payloadSha256']
            position = end
    assert position == len(payload)
    if index == 4:
        assert record['deliberateMissingInputEos'] and not record['inputEosQueued'] and not record['outputEosObserved']
        assert record['classification'] == 'INCOMPLETE_NO_INPUT_EOS'
    else:
        assert record['inputEosQueued'] and record['outputEosObserved'] and record['classification'] == 'OBSERVED_NATIVE_EOS'
    # Metadata remains reported/unknown. Never promote optional trim keys to facts.
    cases.append((name, record, rows))

if not args.execute:
    print(json.dumps({'preflightOnly':True,'toolChildrenStarted':0,'generatedInputsVerified':len(inputs),
        'cases':names,'nativeAacQualified':False,'auCardinality':'UNKNOWN'}))
    sys.exit(0)
out = args.output.resolve()
assert not out.exists(), 'Use a fresh evidence directory; preserve prior results'
out.mkdir(parents=True)
(out/'input-hashes.local.json').write_text(json.dumps(inputs,indent=2)+'\n')
active = None
launching = False
cleaning = False
pending_signal = None
commands = []
cleanup_errors = []
terminal_groups = []
deadline = time.monotonic()+600

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

def run(name, command, allow_failure=False):
    global active,launching,pending_signal
    assert active is None, 'Earlier owned decoder group cleanup unconfirmed'
    owned = None; problem = None
    stdout = out/(name+'.stdout'); stderr = out/(name+'.stderr')
    try:
        with stdout.open('xb') as output, stderr.open('xb') as errors:
            launching = True
            try:
                child = subprocess.Popen(command,stdout=output,stderr=errors,start_new_session=True,preexec_fn=limit_child_files)
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

results = []
failure = None
try:
    for name, record, rows in cases:
        if name == 'missing-input-eos':
            results.append({'name':name,'classification':'INCOMPLETE_NO_INPUT_EOS','decoded':False})
            continue
        if record['containerControlFailure'] is not None:
            results.append({'name':name,'classification':'NATIVE_CONTAINER_CONTROL_UNSUPPORTED',
                'reason':record['containerControlFailure'],'auCardinality':'UNKNOWN','decoded':False})
            continue
        path = source/name/'native.mp4'
        _,packet_output,_ = run(name+'-packets',['ffprobe','-v','error','-show_streams','-show_packets',
            '-show_data_hash','sha256','-of','json',str(path)])
        facts = json.loads(packet_output.read_text())
        streams = facts['streams']; assert len(streams)==1
        stream = streams[0]
        assert stream['codec_name']=='aac' and stream['sample_rate']=='48000' and stream['channels']==1
        packets = facts.get('packets',[])
        expected = [row for row in rows if row['muxed']]
        assert len(packets)==len(expected), 'Native container dropped/merged original buffers'
        for packet, original in zip(packets,expected):
            assert int(packet['size'])==original['size']
            assert packet['data_hash'].lower()=='sha256:'+original['payloadSha256']
        _,frame_output,_ = run(name+'-frames',['ffprobe','-v','error','-show_frames',
            '-show_entries','frame=media_type,nb_samples,pkt_pos,pts,best_effort_timestamp','-of','json',str(path)])
        frames = json.loads(frame_output.read_text()).get('frames',[])
        assert all(frame['media_type']=='audio' and int(frame['nb_samples'])>0 for frame in frames)
        decoded = out/(name+'.s16le')
        run(name+'-decode',['ffmpeg','-nostdin','-v','error','-xerror','-i',str(path),'-map','0:a:0',
            '-c:a','pcm_s16le','-f','s16le','-fs','2097152',str(decoded)])
        assert decoded.stat().st_size < 2*1024*1024 and decoded.stat().st_size%2==0
        results.append({'name':name,'exactOriginalMuxedPayloads':len(expected),
            'ffprobeDecodedFrameSamples':[int(frame['nb_samples']) for frame in frames],
            'ffprobeDecodedExtent':sum(int(frame['nb_samples']) for frame in frames),
            'ffmpegPresentedFrames':decoded.stat().st_size//2,'pcmSHA256':sha(decoded),
            'classification':'FILE_SPECIFIC_DECODER_OBSERVATION','auCardinality':'UNKNOWN',
            'effectiveLeading':'UNKNOWN','effectiveTrailing':'UNKNOWN','decoded':True})
    assert any(row.get('name')=='raw-one-second' and row.get('decoded') for row in results), 'Healthy original native control was not independently decoded'
    # Raw mutations never invent ADTS/native-container boundaries. The separate
    # exact-consumption C witness must inspect these before AU qualification.
    raw_rows = cases[0][2]
    data = [row for row in raw_rows if row['size']>0 and not row['config']]
    if len(data) >= 2:
        raw = (source/'raw-one-second/buffers.bin').read_bytes()
        first = raw[data[0]['payloadOffset']:data[0]['payloadOffset']+data[0]['size']]
        second = raw[data[1]['payloadOffset']:data[1]['payloadOffset']+data[1]['size']]
        mutants = [('two-original-buffers',first+second),('truncated-original-buffer',first[:-1]),
            ('original-buffer-plus-garbage',first+bytes([0x7f,0x31,0x55,0,0x42,0x19,0x6e])),
            ('original-buffer-plus-zero',first+bytes([0]))]
        mutation_output = out/'raw-packing-controls'; mutation_output.mkdir()
        mutation_rows = []
        for name,payload in mutants:
            path = mutation_output/(name+'.bin'); path.write_bytes(payload)
            mutation_rows.append({'name':name,'file':str(path.relative_to(out)),'bytes':len(payload),'sha256':sha(path),
                'sourceSequences':[data[0]['sequence'],data[1]['sequence']] if name=='two-original-buffers' else [data[0]['sequence']],
                'classification':'RAW_EXACT_CONSUMPTION_WITNESS_REQUIRED','expected':'MUST_NOT_CERTIFY_ONE_AU'})
        (out/'raw-packing-controls.local.json').write_text(json.dumps(mutation_rows,indent=2)+'\n')
        results.extend(mutation_rows)
    else:
        results.append({'classification':'SECOND_ORIGINAL_BUFFER_UNAVAILABLE','auCardinality':'UNKNOWN'})
except BaseException as error: failure = type(error).__name__+': '+str(error)
finally:
    if active is not None and not active['attempted']: retire()
    unchanged = all((source/name).is_file() and sha(source/name)==digest for name,digest in inputs.items())
    result = {'inspectionCompleted':failure is None and unchanged and active is None and not cleanup_errors and pending_signal is None,
        'failure':failure,'inputsUnchanged':unchanged,'pendingDeferredSignal':pending_signal,'globalDeadlineSeconds':600,'perToolTimeoutSeconds':45,'commands':commands,'observations':results,
        'decodedControlCount':sum(row.get('decoded') is True for row in results),
        'childrenStopped':active is None and all(row['stopped'] for row in terminal_groups),
        'terminalProcessGroups':terminal_groups,'cleanupErrors':cleanup_errors,
        'unconfirmedOwnedGroupPID':active['child'].pid if active is not None else None,
        'sourceReceiptSHA256':args.receipt_sha256,'scriptSHA256':sha(Path(__file__)),
        'nativeAacQualified':False,'oneAuPerBufferProven':False,'trimQualified':False,'deviceActions':False,
        'scope':'Read-only extracted generated emulator evidence plus independent decoder tools. Exact stored payloads and file-specific decoded extents only; black-box decoding does not certify complete byte/bit consumption or one native AU.'}
    (out/'receipt.local.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({'receipt':str(out/'receipt.local.json'),'sha256':sha(out/'receipt.local.json'),
        'inspectionCompleted':result['inspectionCompleted'],'nativeAacQualified':False}))
sys.exit(0 if result['inspectionCompleted'] else 1)
