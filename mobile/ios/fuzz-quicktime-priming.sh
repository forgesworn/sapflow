#!/bin/bash
# Finite local Swift libFuzzer/ASan run, generated header seeds only.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
python3 - "$root" "$@" <<'PY'
from pathlib import Path
import datetime,hashlib,json,os,platform,re,signal,struct,subprocess,sys,tempfile,uuid,shutil
root=Path(sys.argv[1]);seconds=int(os.environ.get('SAPFLOW_HEADER_FUZZ_SECONDS','20'))
assert 1<=seconds<=60
run=root/'target/ios-quicktime-priming-fuzz'/(datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ-')+uuid.uuid4().hex[:8]);run.mkdir(parents=True,mode=0o700)
names=['mobile/ios/Sources/QuickTimeAudioPriming.swift','mobile/ios/Tests/QuickTimeAudioPrimingTests.swift','mobile/ios/test-quicktime-priming.sh','mobile/ios/Fuzz/QuickTimeAudioPrimingFuzz.swift','mobile/ios/fuzz-quicktime-priming.sh','mobile/ios/Sources/QuickTimeMovieLayout.swift','mobile/ios/Tests/QuickTimeMovieLayoutTests.swift','mobile/ios/test-quicktime-layout.sh']
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
inputs={name:sha(root/name) for name in names};children=[];commands=[];passed=False;failure=None
scratch=Path(tempfile.mkdtemp(prefix='sapflow-priming-fuzz-',dir=root/'target'))
def invoke(args,name,timeout,accepted=(0,)):
    with (run/name).open('wb') as log:
        child=subprocess.Popen(args,cwd=root,stdout=log,stderr=subprocess.STDOUT,start_new_session=True);children.append(child)
        try:code=child.wait(timeout=timeout)
        except BaseException:
            if child.poll() is None:
                os.killpg(child.pid,signal.SIGTERM)
                try:child.wait(timeout=3)
                except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
            raise
    commands.append({'command':args,'exit':code,'log':name,'logSHA256':sha(run/name)})
    if code not in accepted:raise RuntimeError(name+' failed '+str(code))
    return code
try:
    corpus=scratch/'corpus';corpus.mkdir();artifacts=run/'artifacts';artifacts.mkdir()
    invoke(['bash',str(root/'mobile/ios/test-quicktime-priming.sh'),'--write-fuzz-seeds',str(corpus)],'seed-tests.local.log',120)
    invoke(['bash',str(root/'mobile/ios/test-quicktime-layout.sh'),'--write-fuzz-seeds',str(corpus)],'layout-seed-tests.local.log',120)
    # Optional payload-free native-generated headers supplement portable seeds.
    for index,arg in enumerate(sys.argv[2:]):
        p=Path(arg);assert p.stat().st_size<=16*1024*1024
        data=p.read_bytes();at=0;moov=None
        while at<len(data):
            assert len(data)-at>=8
            size,kind=struct.unpack_from('>I4s',data,at)
            assert 8<=size<=len(data)-at and kind in [b'ftyp',b'moov']
            if kind==b'moov':assert moov is None;moov=data[at:at+size]
            at+=size
        assert moov is not None
        valid=97216 if 'native-failure' in p.name else 96356
        prefix=b'\1'+struct.pack('>IQIQ',2,97,2112,valid)
        (corpus/('native-generated-'+str(index))).write_bytes(prefix+moov)
        inputs[str(p)]=sha(p)
    seeds=[{'name':p.name,'bytes':p.stat().st_size,'sha256':sha(p)} for p in sorted(corpus.iterdir())]
    binary=scratch/'header-fuzz'
    compiler=['swiftc','-swift-version','5','-warnings-as-errors','-O','-g','-parse-as-library','-module-cache-path',str(root/'target/ios-module-cache')]
    sources=[str(root/names[index]) for index in [0,5,3]]
    code=invoke(compiler+['-sanitize=fuzzer,address']+sources+['-o',str(binary)],'compile.local.log',120,accepted=(0,1))
    coverage={'mode':'Swift fuzzer sanitizer'}
    if code!=0:
        # Some Apple Swift drivers reject fuzzer while supporting SanitizerCoverage.
        # Reuse an installed Clang runtime or an already-built worktree runtime;
        # never install a toolchain or invoke Cargo for this launcher.
        if "unsupported option '-sanitize=fuzzer'" not in (run/'compile.local.log').read_text():
            raise RuntimeError('Swift compiler failed for a reason other than unsupported fuzzer sanitizer')
        declared=os.environ.get('SAPFLOW_LIBFUZZER_ARCHIVE')
        candidate=Path(subprocess.check_output(['clang','-print-file-name=libclang_rt.fuzzer_osx.a'],text=True).strip())
        candidates=[Path(declared)] if declared else ([candidate] if candidate.is_absolute() and candidate.is_file() else sorted((root/'target/debug/build').glob('libfuzzer-sys-*/out/libfuzzer.a')))
        if not candidates or not candidates[0].is_file():raise RuntimeError('No installed or already-built libFuzzer archive; fuzz gate unavailable')
        runtime=candidates[0].resolve();inputs[str(runtime)]=sha(runtime)
        obj=scratch/'header-fuzz.o'
        hostTarget=platform.machine()+'-apple-macosx'+platform.mac_ver()[0].split('.')[0]+'.0'
        invoke(compiler+['-target',hostTarget,'-whole-module-optimization','-emit-object','-sanitize=address','-sanitize-coverage=edge,inline-8bit-counters,pc-table,trace-cmp']+sources+['-o',str(obj)],'coverage-compile.local.log',120)
        invoke(['nm','-g',str(obj)],'coverage-symbols.local.log',15)
        symbols=(run/'coverage-symbols.local.log').read_text()
        if not all(name in symbols for name in ['__sanitizer_cov_8bit_counters_init','__sanitizer_cov_pcs_init','__sanitizer_cov_trace_cmp']):raise RuntimeError('No actual Swift counter/PC/compare coverage references; fuzz gate unavailable')
        invoke(['swiftc','-target',hostTarget,'-sanitize=address',str(obj),str(runtime),'-lc++','-o',str(binary)],'mixed-link.local.log',60)
        coverage={'mode':'Swift edge/inline-8bit-counters/pc-table/trace-cmp + AddressSanitizer; existing libFuzzer mixed link','runtime':str(runtime),'runtimeSHA256':sha(runtime),'objectSHA256':sha(obj),'counterPCAndCompareReferencesVerified':True,'symbolsSHA256':sha(run/'coverage-symbols.local.log'),'hostOnlyTarget':hostTarget}
    invoke([str(binary),str(corpus),'-max_total_time='+str(seconds),'-timeout=2','-max_len=65536','-rss_limit_mb=768','-artifact_prefix='+str(artifacts)+'/'],'fuzz.local.log',seconds+20)
    log=(run/'fuzz.local.log').read_text()
    counters=re.search(r'(\d+) inline 8-bit counters',log);pcs=re.search(r'(\d+) PCs',log)
    progress=re.findall(r'#(\d+)\s+(?:INITED|DONE|NEW|REDUCE).*?cov:\s*(\d+)\s+ft:\s*(\d+)\s+corp:\s*(\d+)',log)
    completed=re.search(r'Done (\d+) runs in (\d+) second',log)
    if not counters or not pcs or not progress or not completed:raise RuntimeError('No actual counter/PC/corpus/completion statistics; fuzz gate unavailable')
    if min(int(counters[1]),int(pcs[1]),int(progress[-1][1]))<=0 or int(completed[1])<=len(seeds):raise RuntimeError('Fuzz engine did not exercise actual coverage/mutations')
    coverage['statistics']={'inlineCounters':int(counters[1]),'PCs':int(pcs[1]),'initial':list(map(int,progress[0])),'final':list(map(int,progress[-1])),'runs':int(completed[1]),'seconds':int(completed[2])}
    passed=True
except Exception as error:failure=str(error)
finally:
    for child in children:
        if child.poll() is None:
            os.killpg(child.pid,signal.SIGTERM)
            try:child.wait(timeout=3)
            except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
    shutil.rmtree(scratch)
    unchanged=all(sha(Path(name) if Path(name).is_absolute() else root/name)==value for name,value in inputs.items())
    receipt={'passed':passed and unchanged,'sourceUnchanged':unchanged,'sourceHashes':inputs,'commands':commands,'failure':failure,'seeds':locals().get('seeds',[]),'coverage':locals().get('coverage'),'childrenStopped':all(p.poll() is not None for p in children),'scratchRemoved':not scratch.exists(),'scope':'Swift libFuzzer+AddressSanitizer finite metadata-only run; no media/native codec/package/device acceptance'}
    (run/'receipt.local.json').write_text(json.dumps(receipt,indent=2)+'\n')
    print(str(run.relative_to(root)));print(json.dumps({'passed':receipt['passed'],'failure':failure}))
if not receipt['passed']:sys.exit(1)
PY
