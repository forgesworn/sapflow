import test from 'node:test';
import assert from 'node:assert/strict';
import {parseMaster,parseMedia} from '../web/tv/playlist.mjs';
import {boundedBytes,channelUrl} from '../web/tv/cmaf-player.mjs';
const name='012345abcdef';
const master='#EXTM3U\n#EXT-X-VERSION:9\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-STREAM-INF:BANDWIDTH=1200000,AVERAGE-BANDWIDTH=1100000,CODECS="avc1.640029,mp4a.40.2"\nstream.m3u8\n';
test('master accepts the channel codecs and bounded bandwidth',()=>{
 assert.equal(parseMaster(master).codec,'avc1.640029,mp4a.40.2');
 for(const bad of [master.replace('stream.m3u8','https://evil.example/stream.m3u8'),master.replace('1200000','90000000000'),master.replace('avc1.640029','hvc1.bad'),master+'#EXT-X-KEY:URI="https://evil.example/"\n']) assert.throws(()=>parseMaster(bad));
});
test('media preserves gaps, complete segments and codec epochs but ignores parts',()=>{
 const text=`#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-DISCONTINUITY-SEQUENCE:2\n#EXT-X-MAP:URI="${name}_init.mp4"\n#EXT-X-GAP\n#EXTINF:1,\ngap.mp4\n#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI="${name}_init1.mp4"\n#EXT-X-PART:DURATION=0.2,URI="${name}_part4.mp4"\n#EXTINF:1,\n${name}_seg8.mp4\n`;
 const p=parseMedia(text);assert.equal(p.segments.length,2);assert(p.segments[0].gap);assert.equal(p.segments[1].cc,3);assert.equal(p.segments[1].init,name+'_init1.mp4');
});
test('external URLs, unsupported features and incomplete media are refused',()=>{
 for(const text of ['#EXTM3U\n#EXT-X-KEY:URI="x"\n','#EXTM3U\n#EXT-X-MAP:URI="file:///secret"\n','#EXTM3U\r\n','#EXTM3U\n#EXTINF:1,\n','#EXTM3U\n#EXT-X-MAP:URI="012345abcdef_init.mp4"\n#EXTINF:1,\n../escape.mp4\n']) assert.throws(()=>parseMedia(text));
});
test('response reader cancels an oversized body',async()=>{
 let cancelled=false;const response=new Response(new ReadableStream({start(c){c.enqueue(new Uint8Array(16));},cancel(){cancelled=true;}}));
 await assert.rejects(()=>boundedBytes(response,8));assert(cancelled);
});

test('channel source cannot escape the selected origin and known routes',()=>{
 assert.equal(channelUrl('/media/index.m3u8','https://receiver.example/tv/'),'https://receiver.example/media/index.m3u8');
 for(const value of ['https://elsewhere.example/media/index.m3u8','//elsewhere.example/media/index.m3u8','/admin','/media/index.m3u8?redirect=x'])assert.throws(()=>channelUrl(value,'https://receiver.example/tv/'));
});


test('fragment loader can start after the swarm pre-load abort without stale callbacks',async()=>{
  const {FetchLoader}=await import('../web/tv/cmaf-player.mjs');
  const original=globalThis.fetch;globalThis.fetch=async()=>new Response(new Uint8Array([1,2,3]));
  const loader=new FetchLoader();
  try {
    loader.abort();
    const data=await new Promise((resolve,reject)=>loader.load({url:'http://127.0.0.1/fixture'}, {timeout:1000},{onSuccess:resolve,onError:reject}));
    assert.equal(data.data.byteLength,3);assert.equal(loader.stats.aborted,false);
  }finally{loader.destroy();globalThis.fetch=original;}
});


test('Stop aborts all concurrent fragment transfers and releases their loaders',async()=>{
 const {default:Engine}=await import('../web/tv/cmaf-player.mjs');const loaders=[];
 class Loader {stats={};constructor(){loaders.push(this);}load(){}abort(){this.aborted=true;}destroy(){this.destroyed=true;this.abort();}}
 const engine=new Engine({loader:Loader});
 const pending=Array.from({length:4},(_,i)=>engine.download({name:`012345abcdef_seg${i}.mp4`,sn:i,cc:0,position:i,duration:1},'http://127.0.0.1/media/stream.m3u8'));
 assert.equal(loaders.length,4);engine.destroy();const results=await Promise.allSettled(pending);
 assert(results.every(r=>r.status==='rejected'));assert(loaders.every(l=>l.aborted&&l.destroyed));assert.equal(engine.loaders.size,0);
});


test('gap recovery keeps old buffered programme and only nudges a bounded stalled hole',async()=>{
 const {gapPosition}=await import('../web/tv/cmaf-player.mjs');
 const ranges={length:2,start:i=>[0,10.03][i],end:i=>[10,20][i]};
 assert.equal(gapPosition(ranges,5,2),null);assert.equal(gapPosition(ranges,9.99,4),null);
 assert.equal(gapPosition(ranges,10,2,true),null);assert(Math.abs(gapPosition(ranges,10,2)-10.031)<.00001);
 const splice={length:2,start:i=>[0,10.356][i],end:i=>[10,20][i]};assert(Math.abs(gapPosition(splice,10,2)-10.357)<.00001);
 const distant={length:2,start:i=>[0,15][i],end:i=>[10,20][i]};assert.equal(gapPosition(distant,10,2),null);
});


test('a transient playlist update retries within the existing request budget',async()=>{
 const {default:Engine}=await import('../web/tv/cmaf-player.mjs');const original=globalThis.fetch;let calls=0;const engine=new Engine();
 globalThis.fetch=async()=>++calls===1?new Response('Busy',{status:503}):new Response(master);
 try{assert.equal(await engine.text('http://127.0.0.1/media/index.m3u8'),master);assert.equal(calls,2);}finally{engine.destroy();globalThis.fetch=original;}
});
test('Stop during playlist backoff prevents another request',async()=>{
 const {default:Engine}=await import('../web/tv/cmaf-player.mjs');const original=globalThis.fetch;let calls=0;const engine=new Engine();
 globalThis.fetch=async()=>{calls++;queueMicrotask(()=>engine.destroy());return new Response('Busy',{status:503});};
 try{await assert.rejects(()=>engine.text('http://127.0.0.1/media/index.m3u8'));assert.equal(calls,1);}finally{engine.destroy();globalThis.fetch=original;}
});
