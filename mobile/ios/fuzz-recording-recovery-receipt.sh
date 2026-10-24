#!/bin/bash
# SPDX-License-Identifier: MIT
# Finite pure receipt-parser replay and Swift libFuzzer/ASan coverage; synthetic inputs only.
set -euo pipefail
if [ "$#" -ne 0 ]; then echo 'Usage: fuzz-recording-recovery-receipt.sh' >&2; exit 2; fi
root="$(cd "$(dirname "$0")/../.." && pwd)"
python3 -B -u - "$root" <<'PY'
from pathlib import Path
import datetime,hashlib,json,os,platform,re,shutil,signal,subprocess,sys,tempfile,uuid

root=Path(sys.argv[1]).resolve()
if sys.platform!='darwin':raise RuntimeError('Requires installed macOS Swift/Clang tools')
seconds=int(os.environ.get('SAPFLOW_RECOVERY_RECEIPT_FUZZ_SECONDS','20'))
if not 1<=seconds<=60:raise ValueError('Fuzz duration must be between 1 and 60 seconds')
sys.path.insert(0,str(root/'mobile/ios'))
from checkpoint_test_processes import stop_owned_group

run=root/'target/ios-recovery-receipt-fuzz'/(datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ-')+uuid.uuid4().hex[:8])
run.mkdir(parents=True,mode=0o700)
scratch=Path(tempfile.mkdtemp(prefix='sapflow-recovery-receipt-fuzz-',dir=root/'target'))
source_names=['CaptureOrientation','CaptureProfile','RecordingStorage','RecordingCatalogue']
production=[root/'mobile/ios/Sources'/(name+'.swift') for name in source_names]
entry=root/'mobile/ios/Fuzz/RecordingRecoveryReceiptFuzz.swift'
seed=root/'mobile/ios/Fuzz/RecordingRecoveryReceiptSeeds/recovered-v2.json'
invalid_utf8_seed=root/'mobile/ios/Fuzz/RecordingRecoveryReceiptSeeds/invalid-utf8-key-f181ea0c.raw'
invalid_utf8_nested_seed=root/'mobile/ios/Fuzz/RecordingRecoveryReceiptSeeds/invalid-utf8-key-b98f41bc.raw'
paths=production+[entry,seed,invalid_utf8_seed,invalid_utf8_nested_seed,root/'mobile/ios/fuzz-recording-recovery-receipt.sh',root/'mobile/ios/checkpoint_test_processes.py']
sha=lambda path:hashlib.sha256(path.read_bytes()).hexdigest()
inputs={}
active=None;launching=retiring=finalising=False;pending=None;unknown=False
commands=[];cleanup_errors=[];failure=None;passed=False;controls_passed=False;replayed=False;seeds=[];coverage=None

def caught(number,frame):
    global pending
    pending=pending or number
    if not launching and not retiring and not finalising:raise InterruptedError('Signal '+str(number))
previous={number:signal.signal(number,caught) for number in (signal.SIGINT,signal.SIGTERM)}

def invoke(label,argv,timeout,accepted=(0,)):
    global active,launching,retiring,unknown
    if active is not None or unknown or pending is not None:raise RuntimeError('Previous owned group or cancellation not resolved')
    argv=list(map(str,argv));owned=None;attempted=False;primary=None;code=None
    record={'label':label,'command':argv,'exit':None,'groupEvidence':None,'launchAttempted':False}
    commands.append(record)
    try:
        with (run/(label+'.stdout')).open('xb') as output,(run/(label+'.stderr')).open('xb') as diagnostics:
            launching=True
            try:
                attempted=True;record['launchAttempted']=True
                child=subprocess.Popen(argv,cwd=root,stdin=subprocess.DEVNULL,stdout=output,stderr=diagnostics,start_new_session=True)
                owned={'child':child,'cleanupAttempted':False};active=owned
            finally:launching=False
            if pending is not None:raise InterruptedError('Signal during child launch')
            code=child.wait(timeout=timeout)
    except BaseException as error:primary=error
    finally:
        if owned is not None:
            retiring=True;owned['cleanupAttempted']=True
            try:
                if stop_owned_group(owned,run,label):active=None
                else:unknown=True
            except BaseException as error:
                unknown=True;cleanup_errors.append({'stage':label,'type':type(error).__name__,'reason':str(error)})
            finally:retiring=False
            record['groupEvidence']=owned.get('groupEvidence');record['exit']=owned['child'].returncode
        elif attempted:unknown=True
        if primary is not None:record['primaryError']={'type':type(primary).__name__,'reason':str(primary)}
        for suffix in ('stdout','stderr'):
            path=run/(label+'.'+suffix)
            try:record[suffix+'SHA256']=sha(path) if path.is_file() else None
            except BaseException as error:
                record[suffix+'SHA256']=None
                cleanup_errors.append({'stage':label+'-log-hash','type':type(error).__name__,'reason':str(error)})
    if primary is not None:raise primary
    if unknown or active is not None:raise RuntimeError('Current owned process group closure unconfirmed: '+label)
    if pending is not None:raise InterruptedError('Signal during child retirement')
    if code not in accepted:raise RuntimeError(label+' failed '+str(code))
    return code

def diagnostics(label):
    return (run/(label+'.stdout')).read_text(errors='replace')+(run/(label+'.stderr')).read_text(errors='replace')

try:
    inputs={str(path.relative_to(root)):sha(path) for path in paths}
    corpus=scratch/'corpus';corpus.mkdir();artifacts=run/'artifacts';artifacts.mkdir()
    canonical=seed.read_bytes()
    if not 0<len(canonical)<=4096 or canonical.endswith(b'\n'):raise RuntimeError('Canonical seed must retain its exact bounded no-newline bytes')
    original=json.loads(canonical)
    cases=[('recovered-v2.json',canonical,True),('trailing-whitespace',canonical+b' ',False),
           ('truncated',canonical[:len(canonical)//2],False),('nul',b'\0',False)]
    for name,modify in [('unsupported-version',lambda value:value.update(version=3)),
                        ('forbidden-duration',lambda value:value.update(durationMs=0)),
                        ('short-binding',lambda value:value['recovery'].update(sourceFileBinding='AQ==')),
                        ('foreign-output-id',lambda value:value['identity'].update(segmentID='00000000-0000-0000-0000-000000000004'))]:
        value=json.loads(canonical);modify(value)
        cases.append((name,json.dumps(value,sort_keys=True,separators=(',',':')).encode(),False))
    # Exact generated synthetic fuzz crashes. Preserve old nine controls/order.
    invalid_utf8=invalid_utf8_seed.read_bytes()
    if len(invalid_utf8)!=984 or invalid_utf8[38]!=0xae or sha(invalid_utf8_seed)!='f181ea0caf04ce17f0cd3f224a13d88c6b1970623c53c0cded53a5021b1aa779':
        raise RuntimeError('Exact generated invalid-UTF8 regression seed changed')
    cases.append(('invalid-utf8-key-f181ea0c.raw',invalid_utf8,False))
    invalid_utf8_nested=invalid_utf8_nested_seed.read_bytes()
    if len(invalid_utf8_nested)!=781 or invalid_utf8_nested[550]!=0xa9 or sha(invalid_utf8_nested_seed)!='b98f41bccffb1076e5bb7dd49fee76b8e6ed17ae100fa30a16e5fbbe1da05926':
        raise RuntimeError('Exact generated invalid-UTF8 regression seed changed')
    cases.append(('invalid-utf8-key-b98f41bc.raw',invalid_utf8_nested,False))
    for name,data,expected in cases:
        if not 0<len(data)<=4096:raise RuntimeError('Synthetic seed exceeds parser bound')
        path=corpus/name;path.write_bytes(data)
        seeds.append({'name':name,'bytes':len(data),'sha256':sha(path),'expectedParserAcceptance':expected})
    # Positive/negative fixed controls prove the actual production parser is
    # reached; they do not manufacture any file/SDK/EOF/playback capability.
    checks=', '.join(json.dumps(name)+': '+('true' if expected else 'false') for name,_,expected in cases)
    main=scratch/'main.swift'
    main.write_text('import Foundation\nlet corpus = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)\n'+
        'let expected: [String: Bool] = ['+checks+']\n'+
        'for name in expected.keys.sorted() {\n'+
        '    let bytes = try Data(contentsOf: corpus.appendingPathComponent(name))\n'+
        '    guard RecordingCatalogue.checkRecoveryReceiptBytes(bytes) == expected[name]! else { fatalError("Receipt parser control failed: \\(name)") }\n'+
        '}\nprint("All \\(expected.count) synthetic receipt controls matched the actual parser")\n')
    host=platform.machine()+'-apple-macosx'+platform.mac_ver()[0].split('.')[0]+'.0'
    compiler=['/usr/bin/xcrun','--sdk','macosx','swiftc','-swift-version','5','-warnings-as-errors','-O','-g',
              '-target',host,'-module-cache-path',root/'target/ios-module-cache']
    check=scratch/'receipt-seed-check';binary=scratch/'receipt-fuzz';obj=scratch/'receipt-fuzz.o'
    invoke('seed-check-compile',compiler+['-sanitize=address',*production,main,'-o',check],180)
    invoke('seed-check',[check,corpus],30)
    controls_passed=True
    declared=os.environ.get('SAPFLOW_LIBFUZZER_ARCHIVE')
    sources=production+[entry];mixed=bool(declared)
    if not mixed:
        result=invoke('native-fuzzer-compile',compiler+['-parse-as-library','-sanitize=fuzzer,address',*sources,'-o',binary],180,accepted=(0,1))
        if result!=0:
            text=diagnostics('native-fuzzer-compile')
            if not re.search(r"unsupported (?:option|argument)[^\n]*fuzzer",text):
                raise RuntimeError('Swift fuzzer compilation failed for a reason other than unsupported fuzzer sanitizer')
            mixed=True
    if mixed:
        if declared:runtime=Path(declared).expanduser().resolve()
        else:
            invoke('clang-runtime',['/usr/bin/xcrun','--sdk','macosx','clang','-print-file-name=libclang_rt.fuzzer_osx.a'],15)
            candidate=Path((run/'clang-runtime.stdout').read_text().strip())
            candidates=[candidate] if candidate.is_absolute() and candidate.is_file() else sorted((root/'target/debug/build').glob('libfuzzer-sys-*/out/libfuzzer.a'))
            if not candidates:raise RuntimeError('No installed or already-built libFuzzer archive; set SAPFLOW_LIBFUZZER_ARCHIVE (no Cargo/install fallback)')
            runtime=candidates[0].resolve()
        if not runtime.is_file():raise RuntimeError('Declared existing libFuzzer archive is not a regular file')
        inputs[str(runtime)]=sha(runtime)
        invoke('coverage-compile',compiler+['-parse-as-library','-whole-module-optimization','-emit-object','-sanitize=address',
            '-sanitize-coverage=edge,inline-8bit-counters,pc-table,trace-cmp',*sources,'-o',obj],180)
        invoke('coverage-symbols',['/usr/bin/nm','-g',obj],15)
        symbols=diagnostics('coverage-symbols')
        if not all(name in symbols for name in ('__sanitizer_cov_8bit_counters_init','__sanitizer_cov_pcs_init','__sanitizer_cov_trace_cmp')):
            raise RuntimeError('Missing actual Swift counter/PC/compare coverage references')
        invoke('mixed-link',['/usr/bin/xcrun','--sdk','macosx','swiftc','-target',host,'-sanitize=address',obj,runtime,'-lc++','-o',binary],60)
        coverage={'mode':'Swift ASan + edge/inline-8bit-counters/pc-table/trace-cmp; existing libFuzzer mixed link',
                  'runtimeSHA256':sha(runtime),'objectSHA256':sha(obj),'symbolsSHA256':sha(run/'coverage-symbols.stdout')}
    else:coverage={'mode':'Swift fuzzer + AddressSanitizer'}
    flags=['-timeout=2','-max_len=4096','-rss_limit_mb=768','-artifact_prefix='+str(artifacts)+'/']
    invoke('replay',[binary,*[corpus/name for name,_,_ in cases],'-runs=1',*flags],30)
    replayed=True
    invoke('fuzz',[binary,corpus,'-seed=20261024','-max_total_time='+str(seconds),*flags],seconds+20)
    log=diagnostics('fuzz')
    counters=re.search(r'(\d+) inline 8-bit counters',log);pcs=re.search(r'(\d+) PCs',log)
    progress=re.findall(r'#(\d+)\s+(?:INITED|DONE|NEW|REDUCE).*?cov:\s*(\d+)\s+ft:\s*(\d+)\s+corp:\s*(\d+)',log)
    completed=re.search(r'Done (\d+) runs in (\d+) second',log)
    if not counters or not pcs or not progress or not completed:raise RuntimeError('Missing actual counter/PC/corpus/completion statistics')
    if min(int(counters[1]),int(pcs[1]),int(progress[-1][1]))<=0 or int(completed[1])<=len(seeds):
        raise RuntimeError('Fuzz engine did not demonstrate actual coverage/mutations')
    if list(artifacts.iterdir()) or re.search(r'(?:ERROR|SUMMARY): AddressSanitizer',diagnostics('replay')+log):
        raise RuntimeError('Replay/fuzz reported sanitizer failure or created an artifact')
    coverage['statistics']={'inlineCounters':int(counters[1]),'PCs':int(pcs[1]),'initial':list(map(int,progress[0])),
                            'final':list(map(int,progress[-1])),'runs':int(completed[1]),'seconds':int(completed[2])}
    passed=True
except BaseException as error:failure={'type':type(error).__name__,'reason':str(error)}
finally:
    finalising=True
    # invoke retires its exact current group ONCE. No retry of historical PIDs
    # or deletion of scratch while any group's outcome is unknown.
    unchanged=False
    try:unchanged=all(sha(Path(name) if Path(name).is_absolute() else root/name)==digest for name,digest in inputs.items())
    except BaseException as error:cleanup_errors.append({'stage':'source-readback','type':type(error).__name__,'reason':str(error)})
    if passed and unchanged and active is None and not unknown and pending is None and not cleanup_errors:
        try:shutil.rmtree(scratch)
        except BaseException as error:cleanup_errors.append({'stage':'scratch-removal','type':type(error).__name__,'reason':str(error)})
    groups_stopped=active is None and not unknown and all(not record['launchAttempted'] or (record.get('groupEvidence') or {}).get('stopped') is True for record in commands)
    receipt={'passed':passed and unchanged and groups_stopped and not scratch.exists() and pending is None and not cleanup_errors,
             'sourceUnchanged':unchanged,'sourceHashes':inputs,'commands':commands,'failure':failure,'cleanupErrors':cleanup_errors,
             'seeds':seeds,'coverage':coverage,'syntheticParserControlsPassed':controls_passed,'fixedSeedReplayPassed':replayed,'childrenAndGroupsStopped':groups_stopped,
             'scratchRemoved':not scratch.exists(),'retainedScratch':str(scratch) if scratch.exists() else None,'pendingSignal':pending,
             'scope':'Finite pure canonical receipt parser replay/libFuzzer/ASan; synthetic metadata only. No file ownership, recovery, media, SDK, native codec, capture, device or release acceptance.'}
    # The completion cut is the last masked pending inspection and atomic
    # rename below. It does not claim a signal-free process lifetime.
    signal.pthread_sigmask(signal.SIG_BLOCK,{signal.SIGINT,signal.SIGTERM})
    receipt['terminalPublication']={'protocol':'MASKED_PENDING_INSPECTION_AND_ATOMIC_PUBLICATION_V1',
        'signalFreeProcessLifetimeClaim':False,'committed':True}
    draft=run/'receipt.precommit.json'
    with draft.open('x') as stream:json.dump(receipt,stream,indent=2);stream.write('\n')
    observed=pending or next(iter(sorted(signal.sigpending() & {signal.SIGINT,signal.SIGTERM})),None)
    if observed is not None:
        receipt['passed']=False;receipt['pendingSignal']=observed
        draft.write_text(json.dumps(receipt,indent=2)+'\n')
    os.replace(draft,run/'receipt.local.json')
    # This heredoc process exits immediately below. Keep the mask through exit.
    for number,handler in previous.items():signal.signal(number,handler)
    print(str(run.relative_to(root)));print(json.dumps({'passed':receipt['passed'],'failure':failure,'cleanupErrors':cleanup_errors}),flush=True)
if not receipt['passed']:sys.exit(1)
PY
