// Run after npm ci --prefix tools/tv-relayswarm --ignore-scripts. MIT.
import {build} from 'esbuild';
import {readFile,writeFile,mkdir} from 'node:fs/promises';
import {resolve,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
const here=dirname(fileURLToPath(import.meta.url)),root=resolve(here,'../..'),out=resolve(root,'target/tv-relayswarm');
await mkdir(out,{recursive:true});
const result=await build({entryPoints:[resolve(here,'server.mjs')],outfile:resolve(out,'server.mjs'),bundle:true,platform:'node',format:'esm',target:'node22',metafile:true,
  nodePaths:[resolve(here,'node_modules')],banner:{js:"import {createRequire as __createRequire} from 'node:module'; const require = __createRequire(import.meta.url);"},
  alias:{'relayswarm/hls-publisher':resolve(out,'release/package/src/hls-publisher.mjs'),'relayswarm/publisher-state':resolve(out,'release/package/src/publisher-state.mjs')}});
const packages=new Map();
for(const input of Object.keys(result.metafile.inputs)){
  if(!input.includes('node_modules/'))continue;
  let directory=dirname(resolve(input));
  while(directory.includes('node_modules')){
    try{const pkg=JSON.parse(await readFile(resolve(directory,'package.json'),'utf8'));if(pkg.name){packages.set(pkg.name,{directory,...pkg});break;}}catch{}
    directory=dirname(directory);
  }
}
// The released browser bundle also includes these MIT cryptographic helpers.
for(const name of ['@noble/ciphers','@scure/base']){const directory=resolve(here,'node_modules',name);const pkg=JSON.parse(await readFile(resolve(directory,'package.json'),'utf8'));packages.set(name,{directory,...pkg});}
let notices=await readFile(resolve(root,'web/tv/vendor/relayswarm-LICENSE'),'utf8');
for(const pkg of [...packages.values()].sort((a,b)=>a.name.localeCompare(b.name))){
  if(!['MIT','Unlicense','ISC'].includes(pkg.license))throw Error(`Review licence before shipping: ${pkg.name} ${pkg.license}`);
  let licence;for(const file of ['LICENSE','LICENSE.md','LICENSE.txt'])try{licence=await readFile(resolve(pkg.directory,file),'utf8');break;}catch{}
  if(!licence)throw Error(`Missing licence: ${pkg.name}`);
  notices+=`\n\n${pkg.name} ${pkg.version}\n${licence}`;
}
notices+='\n\n'+await readFile(resolve(here,'wasm-LICENCES.txt'),'utf8');
notices=notices.split('\n').map(line=>line.trimEnd()).join('\n').trimEnd()+'\n';
await writeFile(resolve(out,'THIRD-PARTY-LICENCES.txt'),notices);
await writeFile(resolve(root,'web/tv/vendor/THIRD-PARTY-LICENCES.txt'),notices);
await writeFile(resolve(out,'build.json'),JSON.stringify({packages:[...packages.values()].map(p=>({name:p.name,version:p.version,licence:p.license}))},null,2)+'\n');
console.log('Receiver bundle and included dependency licences prepared.');
