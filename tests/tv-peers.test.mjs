import test from 'node:test';
import assert from 'node:assert/strict';
import {attachPeers,privatePreview} from '../web/tv/peers.mjs';
test('private Mac preview cannot load a peer SDK or make a signalling request',async()=>{
  for(const hostname of ['127.0.0.1','localhost','[::1]']){
    assert(privatePreview({hostname}));let calls=0;
    await assert.rejects(attachPeers({}, {location:{hostname},signal:new AbortController().signal,fetchImpl:()=>{calls++;},loadSdk:()=>{calls++;}}));assert.equal(calls,0);
  }
});
test('viewer peer assistance is bounded and closes on Stop',async()=>{
  const signal=new AbortController();let options,attached=false,stopped=0;
  const result=await attachPeers({}, {location:new URL('https://receiver.example/tv/'),signal:signal.signal,
    fetchImpl:async()=>({ok:true,json:async()=>({swarmId:'test',streamEvent:{}})}),
    loadSdk:async()=>({createHlsSwarm:o=>{options=o;return {metrics:()=>({}),attach:()=>{attached=true;},stop:()=>{stopped++;}};}})});
  assert(attached);assert.equal(result.stream,'/media/index.m3u8');assert.equal(options.maxUploadBytesPerSession,128*1024*1024);assert.equal(options.maxPeerWaitMs,500);
  assert.deepEqual(options.relays,['wss://receiver.example/tv/swarm/relay']);signal.abort();assert.equal(stopped,1);
});
