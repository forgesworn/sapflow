import {lstatSync,realpathSync,mkdirSync} from 'node:fs';
import {resolve,join,dirname,basename,isAbsolute} from 'node:path';

const androidTargets=[['aarch64-linux-android','arm64-v8a','aarch64-linux-android23-clang'],['armv7-linux-androideabi','armeabi-v7a','armv7a-linux-androideabi23-clang'],['x86_64-linux-android','x86_64','x86_64-linux-android23-clang'],['i686-linux-android','x86','i686-linux-android23-clang']];

/** Read-only validation. Custom outputs have one deliberately narrow namespace. */
export function validateNativeOutputRoot(root,value) {
  const checkout=resolve(root),target=join(checkout,'target');
  if(!isAbsolute(value)||resolve(value)!==value||dirname(value)!==target||!/^native-packages-[A-Za-z0-9][A-Za-z0-9._-]{0,79}$/.test(basename(value)))
    throw Error('Custom output root must be a canonical absolute direct target/native-packages-<safe suffix> child');
  if(realpathSync(checkout)!==checkout||realpathSync(target)!==target||!lstatSync(target).isDirectory())
    throw Error('Custom output root requires a canonical checkout target directory');
  try {lstatSync(value);} catch(error) {if(error.code==='ENOENT')return value;throw error;}
  throw Error('Custom output root must be fresh, even if an existing path is marked owned');
}

/** Atomic reservation happens before any build child, not just an exists check. */
export function reserveNativeOutputRoot(root,value) {
  validateNativeOutputRoot(root,value);
  mkdirSync(value,{mode:0o700}); // EEXIST is a refusal; never remove or retry it.
  return value;
}

export function parseNativeBuildOptions(root,args) {
  const mode=args[0]??'check';
  if(!['check','apple','android'].includes(mode))throw Error('Expected check, apple or android');
  let abis,customOutputRoot;
  for(const arg of args.slice(1)) {
    if(arg.startsWith('--abis=')&&mode==='android'&&abis===undefined)abis=arg.slice('--abis='.length).split(',');
    else if(arg.startsWith('--output-root=')&&mode!=='check'&&customOutputRoot===undefined)customOutputRoot=arg.slice('--output-root='.length);
    else throw Error('Unknown, duplicate or unsupported native build option');
  }
  const requestedAbis=abis??androidTargets.map(x=>x[1]);
  if(!requestedAbis.length||new Set(requestedAbis).size!==requestedAbis.length||requestedAbis.some(x=>!androidTargets.some(t=>t[1]===x)))
    throw Error('Unknown, empty or duplicate Android ABI');
  const selectedTargets=androidTargets.filter(x=>requestedAbis.includes(x[1]));
  const packageLabel=mode==='android'&&selectedTargets.length!==androidTargets.length?`android-${selectedTargets.map(x=>x[1]).join('-')}`:mode;
  const outputRoot=customOutputRoot===undefined?join(root,'target/native-packages'):validateNativeOutputRoot(root,customOutputRoot);
  return {mode,selectedTargets,packageLabel,outputRoot,customOutputRoot:customOutputRoot!==undefined};
}
