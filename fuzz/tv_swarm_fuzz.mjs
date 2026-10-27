// Finite deterministic hostile JSON gate for the single-channel rendezvous.
import assert from 'node:assert/strict';
import {validFilter,validEvent} from '../target/tv-relayswarm/server.mjs';
import {controlMessage,storedControl} from '../tools/tv-relayswarm/control.mjs';
import {heartbeat} from '../tools/tv-relayswarm/audience.mjs';
const count=Number(process.argv[2]??20000);let seed=0x12ef34;
function random(n){seed=(Math.imul(seed,1664525)+1013904223)>>>0;return seed%n;}
const corpus=[{action:'start',session:'a'.repeat(32),startedAt:1},{id:'a'.repeat(32),active:true,peers:false,version:'1'},null,[],{},'channel',0,{kinds:[24170],'#x':['channel']},{kind:24170,tags:[['x','channel']],content:'{}',created_at:0},{kind:24171,tags:[null],content:'x',created_at:0}];
for(let i=0;i<count;i++){
 let text=JSON.stringify(corpus[random(corpus.length)]);const at=random(text.length+1);text=text.slice(0,at)+String.fromCharCode(random(128))+text.slice(at+random(3));
 let input;try{input=JSON.parse(text);}catch{continue;}
 assert.doesNotThrow(()=>{validFilter(input,'channel');validEvent(input,'channel');try{controlMessage(input);}catch(error){assert.equal(error.status,400);}try{storedControl(input);}catch{}try{heartbeat(input);}catch(error){assert.equal(error.status,400);}});
}
console.log(`${count} bounded rendezvous JSON mutations passed`);
