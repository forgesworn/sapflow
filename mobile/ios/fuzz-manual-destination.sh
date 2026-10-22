#!/bin/bash
# Finite local Swift libFuzzer/ASan run, synthetic link and JSON seeds only.
set -euo pipefail
if [ "$#" -ne 0 ]; then echo 'Usage: fuzz-manual-destination.sh' >&2; exit 2; fi
root="$(cd "$(dirname "$0")/../.." && pwd)"
python3 -u - "$root" <<'PY'
from pathlib import Path
import datetime,hashlib,json,os,platform,re,shutil,signal,subprocess,sys,tempfile,uuid
root=Path(sys.argv[1]);seconds=int(os.environ.get('SAPFLOW_MANUAL_LINK_FUZZ_SECONDS','10'))
assert 1<=seconds<=60
run=root/'target/ios-manual-destination-fuzz'/(datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ-')+uuid.uuid4().hex[:8]);run.mkdir(parents=True,mode=0o700)
sources=['DestinationProfile','SrtDestination','V4vContracts','ManualDestinationImport','ManualDestinationEnvelope']
names=['mobile/ios/Sources/'+n+'.swift' for n in sources]+['mobile/ios/Fuzz/ManualDestinationEnvelopeFuzz.swift','mobile/ios/fuzz-manual-destination.sh','tests/fixtures/manual-destination-v1.json','tests/fixtures/manual-destination-link-v1.json']
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
inputs={name:sha(root/name) for name in names}
scratch=Path(tempfile.mkdtemp(prefix='sapflow-manual-fuzz-',dir=root/'target'));children=[];commands=[];passed=False;failure=None;coverage=None;seeds=[]
def invoke(args,name,limit):
    with (run/name).open('wb') as log:
        child=subprocess.Popen(args,cwd=root,stdout=log,stderr=subprocess.STDOUT,start_new_session=True);children.append(child)
        try:code=child.wait(timeout=limit)
        except BaseException:
            if child.poll() is None:
                os.killpg(child.pid,signal.SIGTERM)
                try:child.wait(timeout=3)
                except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
            raise
    commands.append({'command':args,'exit':code,'log':name,'logSHA256':sha(run/name)})
    if code!=0:raise RuntimeError(name+' failed '+str(code))
def interrupted(signum,frame):raise RuntimeError('Owned fuzz run interrupted '+str(signum))
for sig in [signal.SIGTERM,signal.SIGINT]:signal.signal(sig,interrupted)
try:
    corpus=scratch/'corpus';corpus.mkdir();artifacts=run/'artifacts';artifacts.mkdir()
    for filename,key,mode in [('manual-destination-link-v1.json','text',0),('manual-destination-v1.json','json',1)]:
        cases=json.loads((root/'tests/fixtures'/filename).read_text())['cases']
        for index,case in enumerate(cases):
            data=bytes([mode])+case[key].encode('utf-8')
            (corpus/(str(mode)+'-'+str(index))).write_bytes(data)
    seeds=[{'name':p.name,'bytes':p.stat().st_size,'sha256':sha(p)} for p in sorted(corpus.iterdir())]
    declared=os.environ.get('SAPFLOW_LIBFUZZER_ARCHIVE')
    candidate=Path(subprocess.check_output(['clang','-print-file-name=libclang_rt.fuzzer_osx.a'],text=True).strip())
    candidates=[Path(declared)] if declared else ([candidate] if candidate.is_absolute() and candidate.is_file() else sorted((root/'target/debug/build').glob('libfuzzer-sys-*/out/libfuzzer.a')))
    if not candidates or not candidates[0].is_file():raise RuntimeError('No installed or already-built libFuzzer archive; gate unavailable')
    runtime=candidates[0].resolve();inputs[str(runtime)]=sha(runtime)
    host=platform.machine()+'-apple-macosx'+platform.mac_ver()[0].split('.')[0]+'.0'
    obj=scratch/'manual-fuzz.o';binary=scratch/'manual-fuzz'
    invoke(['swiftc','-swift-version','5','-warnings-as-errors','-O','-g','-parse-as-library','-target',host,
        '-module-cache-path',str(root/'target/ios-module-cache'),'-whole-module-optimization','-emit-object',
        '-sanitize=address','-sanitize-coverage=edge,inline-8bit-counters,pc-table,trace-cmp']+
        [str(root/n) for n in names[:len(sources)+1]]+['-o',str(obj)],'compile.local.log',180)
    invoke(['nm','-g',str(obj)],'symbols.local.log',15)
    symbols=(run/'symbols.local.log').read_text()
    assert all(n in symbols for n in ['__sanitizer_cov_8bit_counters_init','__sanitizer_cov_pcs_init','__sanitizer_cov_trace_cmp'])
    invoke(['swiftc','-target',host,'-sanitize=address',str(obj),str(runtime),'-lc++','-o',str(binary)],'link.local.log',60)
    invoke([str(binary),str(corpus),'-seed=20261022','-max_total_time='+str(seconds),'-timeout=2','-max_len=24577','-rss_limit_mb=768','-artifact_prefix='+str(artifacts)+'/'],'fuzz.local.log',seconds+20)
    log=(run/'fuzz.local.log').read_text()
    counters=re.search(r'(\d+) inline 8-bit counters',log);pcs=re.search(r'(\d+) PCs',log)
    progress=re.findall(r'#(\d+)\s+(?:INITED|DONE|NEW|REDUCE).*?cov:\s*(\d+)\s+ft:\s*(\d+)\s+corp:\s*(\d+)',log)
    completed=re.search(r'Done (\d+) runs in (\d+) second',log)
    assert counters and pcs and progress and completed
    assert min(int(counters[1]),int(pcs[1]),int(progress[-1][1]))>0 and int(completed[1])>len(seeds)
    coverage={'mode':'Swift ASan + edge/inline-8bit-counters/pc-table/trace-cmp, existing libFuzzer mixed link',
        'runtimeSHA256':sha(runtime),'objectSHA256':sha(obj),'symbolsSHA256':sha(run/'symbols.local.log'),
        'inlineCounters':int(counters[1]),'PCs':int(pcs[1]),'initial':list(map(int,progress[0])),'final':list(map(int,progress[-1])),
        'runs':int(completed[1]),'seconds':int(completed[2])}
    passed=True
except BaseException as error:failure={'type':type(error).__name__,'reason':str(error)}
finally:
    for child in children:
        if child.poll() is None:
            os.killpg(child.pid,signal.SIGTERM)
            try:child.wait(timeout=3)
            except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
    shutil.rmtree(scratch)
    unchanged=all(sha(Path(n) if Path(n).is_absolute() else root/n)==h for n,h in inputs.items())
    receipt={'passed':passed and unchanged,'sourceUnchanged':unchanged,'sourceHashes':inputs,'commands':commands,'failure':failure,'seeds':seeds,'coverage':coverage,
        'childrenStopped':all(c.poll() is not None for c in children),'scratchRemoved':not scratch.exists(),
        'scope':'Finite pure Swift link/profile libFuzzer+ASan gate. No file, Keychain, socket, scanner, device or installed routing acceptance.'}
    (run/'receipt.local.json').write_text(json.dumps(receipt,indent=2)+'\n');print(str(run.relative_to(root)));print(json.dumps({'passed':receipt['passed'],'failure':failure}),flush=True)
if not receipt['passed']:sys.exit(1)
PY
