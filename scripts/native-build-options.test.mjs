import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,mkdirSync,writeFileSync,readFileSync,rmSync,realpathSync,lstatSync,symlinkSync,renameSync,existsSync,readdirSync} from 'node:fs';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {parseNativeBuildOptions,validateNativeOutputRoot,reserveNativeOutputRoot} from './native-build-options.mjs';

function fixture(t) {
  const root=realpathSync(mkdtempSync(join(tmpdir(),'sapflow-native-output-test-')));
  mkdirSync(join(root,'target'));
  t.after(()=>rmSync(root,{recursive:true,force:true}));
  return root;
}
const fresh=root=>join(root,'target/native-packages-test-run');

test('default check has no output mutation',t=>{
  const root=fixture(t),options=parseNativeBuildOptions(root,[]);
  assert.equal(options.mode,'check');assert.equal(options.customOutputRoot,false);
  assert.equal(options.outputRoot,join(root,'target/native-packages'));
  assert.deepEqual(readdirSync(join(root,'target')),[]);
});
test('default Apple and all four Android target mappings remain unchanged',t=>{
  const root=fixture(t),apple=parseNativeBuildOptions(root,['apple']),android=parseNativeBuildOptions(root,['android']);
  assert.equal(apple.packageLabel,'apple');assert.equal(android.packageLabel,'android');
  assert.deepEqual(android.selectedTargets,[['aarch64-linux-android','arm64-v8a','aarch64-linux-android23-clang'],['armv7-linux-androideabi','armeabi-v7a','armv7a-linux-androideabi23-clang'],['x86_64-linux-android','x86_64','x86_64-linux-android23-clang'],['i686-linux-android','x86','i686-linux-android23-clang']]);
});
test('ABI subset labels and full-list ordering preserve the old selection',t=>{
  const root=fixture(t),subset=parseNativeBuildOptions(root,['android','--abis=x86_64,arm64-v8a']);
  assert.equal(subset.packageLabel,'android-arm64-v8a-x86_64');
  const full=parseNativeBuildOptions(root,['android','--abis=x86,x86_64,armeabi-v7a,arm64-v8a']);
  assert.equal(full.packageLabel,'android');assert.equal(full.selectedTargets.length,4);
});
test('custom Apple and Android roots are validated without creating them',t=>{
  const root=fixture(t),destination=fresh(root);
  for(const args of [['apple','--output-root='+destination],['android','--output-root='+destination,'--abis=arm64-v8a'],['android','--abis=arm64-v8a','--output-root='+destination]]) {
    const options=parseNativeBuildOptions(root,args);assert.equal(options.outputRoot,destination);assert.equal(options.customOutputRoot,true);
  }
  assert.equal(existsSync(destination),false);
});
test('unsupported modes flags and duplicate options refuse before mutation',t=>{
  const root=fixture(t),flag='--output-root='+fresh(root);
  for(const args of [['bad'],['check',flag],['check','--abis=arm64-v8a'],['apple','--abis=arm64-v8a'],['apple',flag,flag],['android','--unknown'],['android','--output-root'],['android','--abis=arm64-v8a','--abis=arm64-v8a']])assert.throws(()=>parseNativeBuildOptions(root,args));
  assert.deepEqual(readdirSync(join(root,'target')),[]);
});
test('empty duplicate and unknown ABI lists refuse',t=>{
  const root=fixture(t);
  for(const value of ['', 'arm64-v8a,arm64-v8a','mips','arm64-v8a,'])assert.throws(()=>parseNativeBuildOptions(root,['android','--abis='+value]));
});
test('custom root must be absolute and lexically canonical',t=>{
  const root=fixture(t),destination=fresh(root);
  for(const value of ['', 'target/native-packages-run', destination+'/', join(root,'target')+'/./native-packages-run', join(root,'target')+'/../target/native-packages-run'])assert.throws(()=>parseNativeBuildOptions(root,['android','--output-root='+value]));
});
test('custom root cannot be nested outside target or use a reserved namespace',t=>{
  const root=fixture(t);mkdirSync(join(root,'target/native-tooling'));
  for(const value of [join(root,'native-packages-run'),join(root,'target'),join(root,'target/native-packages'),join(root,'target/native-tooling'),join(root,'target/debug'),join(root,'target/sapflow-native-build-run'),join(root,'target/native-tooling/native-packages-run'),join(root,'target/native-packages-run space'),join(root,'target/native-packages-'+('a'.repeat(81)))])assert.throws(()=>validateNativeOutputRoot(root,value));
});
test('existing custom directory remains untouched even with an owned marker',t=>{
  const root=fixture(t),destination=fresh(root);mkdirSync(destination);writeFileSync(join(destination,'.sapflow-owned'),'old proof');writeFileSync(join(destination,'sentinel'),'preserve');
  assert.throws(()=>parseNativeBuildOptions(root,['apple','--output-root='+destination]),/fresh/);
  assert.equal(readFileSync(join(destination,'sentinel'),'utf8'),'preserve');
});
test('existing file and dangling symlink both refuse fresh reservation',t=>{
  const root=fixture(t),file=fresh(root);writeFileSync(file,'preserve');
  assert.throws(()=>reserveNativeOutputRoot(root,file),/fresh/);assert.equal(readFileSync(file,'utf8'),'preserve');
  const link=join(root,'target/native-packages-link');symlinkSync(join(root,'not-present'),link);
  assert.throws(()=>reserveNativeOutputRoot(root,link),/fresh/);assert.equal(lstatSync(link).isSymbolicLink(),true);
});
test('symlinked checkout or target directories cannot redirect custom output',t=>{
  const root=fixture(t),target=join(root,'target'),realTarget=join(root,'real-target');renameSync(target,realTarget);symlinkSync(realTarget,target);
  assert.throws(()=>validateNativeOutputRoot(root,fresh(root)),/canonical/);
  const alias=join(root,'alias');symlinkSync(root,alias);
  assert.throws(()=>validateNativeOutputRoot(alias,join(alias,'target/native-packages-run')),/canonical/);
});
test('exclusive reservation creates only its private root and preserves old packages',t=>{
  const root=fixture(t),old=join(root,'target/native-packages'),destination=fresh(root);mkdirSync(old);writeFileSync(join(old,'sentinel'),'known old package');
  assert.equal(reserveNativeOutputRoot(root,destination),destination);assert.equal(lstatSync(destination).mode&0o777,0o700);
  assert.throws(()=>reserveNativeOutputRoot(root,destination),/fresh/);assert.equal(readFileSync(join(old,'sentinel'),'utf8'),'known old package');
});
test('path created after parsing is refused without deleting its contents',t=>{
  const root=fixture(t),options=parseNativeBuildOptions(root,['android','--output-root='+fresh(root)]);
  mkdirSync(options.outputRoot);writeFileSync(join(options.outputRoot,'sentinel'),'other owner');
  assert.throws(()=>reserveNativeOutputRoot(root,options.outputRoot),/fresh/);assert.equal(readFileSync(join(options.outputRoot,'sentinel'),'utf8'),'other owner');
});
