import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,rm,readFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createServer} from 'node:http';
import {once} from 'node:events';
import {completePlaylist,validFilter,validEvent,startReceiver} from '../target/tv-relayswarm/server.mjs';
import {finalizeEvent,generateSecretKey,verifyEvent} from '../tools/tv-relayswarm/node_modules/nostr-tools/lib/esm/pure.js';
import {WebSocket} from '../tools/tv-relayswarm/node_modules/ws/wrapper.mjs';
import {parseMedia} from '../web/tv/playlist.mjs';
const name='abcdef123456',media=`#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:10\n#EXT-X-MAP:URI="${name}_init.mp4"\n#EXTINF:1,\n${name}_seg10.mp4\n#EXT-X-DISCONTINUITY\n#EXTINF:1,\n${name}_seg11.mp4\n`;
const master='#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1500000,CODECS="avc1.640029,mp4a.40.2"\nstream.m3u8\n';
test('whole-fragment mirror preserves sequence, discontinuity and exact media identity',()=>{
  const parsed=parseMedia(media),roundtrip=parseMedia(completePlaylist(parsed));assert.deepEqual(roundtrip,parsed);
  assert(!completePlaylist(parsed).includes('#EXT-X-PART'));
});
test('rendezvous refuses broader filters, foreign channels and forged events',()=>{
  assert(validFilter({kinds:[24170,24171],'#x':['channel']},'channel'));
  for(const f of [{kinds:[1],'#x':['channel']},{kinds:[24170],'#x':['other']},{kinds:[24170],'#x':['channel'],authors:['any']}])assert(!validFilter(f,'channel'));
  const event=finalizeEvent({kind:24170,created_at:Math.floor(Date.now()/1000),tags:[['x','channel']],content:'{}'},generateSecretKey());
  assert(validEvent(event,'channel'));assert(!validEvent(event,'other'));assert(!validEvent(JSON.parse(JSON.stringify({...event,sig:'0'.repeat(128)})),'channel'));
});
test('receiver signs only mirrored playlist contexts and scopes its WebSocket',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'sapflow-swarm-test-'));
  let removed=false,expiredFragment=false;
  const upstream=createServer((req,res)=>{if(removed){res.statusCode=404;res.end();return;}const asset=req.url.split('/').at(-1);if(asset==='index.m3u8')res.end(master);else if(asset==='stream.m3u8')res.end(media+(expiredFragment?`#EXTINF:1,\n${name}_seg12.mp4\n`:''));else if([`${name}_init.mp4`,`${name}_seg10.mp4`,`${name}_seg11.mp4`].includes(asset))res.end('generated fixture bytes');else{res.statusCode=404;res.end();}});
  upstream.listen(0,'127.0.0.1');await once(upstream,'listening');let receiver,ws;
  try {
    receiver=await startReceiver({stateDir:directory,upstream:`http://127.0.0.1:${upstream.address().port}/live/event-tv/`,origin:'https://receiver.example',port:0});
    const base=`http://127.0.0.1:${receiver.server.address().port}`;let config;
    for(let i=0;i<30;i++){const response=await fetch(base+'/tv/swarm/config');if(response.ok){config=await response.json();break;}await new Promise(r=>setTimeout(r,100));}
    assert(config?.swarmId);assert(!('secretKey' in config));
    const url=new URL('/tv/swarm/commitment',base);url.search=new URLSearchParams({path:`/media/${name}_seg10.mp4`,sequence:'10',discontinuity:'0',level:'0',playlist:'/media/stream.m3u8',mediaType:'main'});
    const response=await fetch(url);assert.equal(response.status,200);const event=await response.json();assert(verifyEvent(event));assert.equal(JSON.parse(event.content).size,23);
    url.searchParams.set('path','/etc/passwd');assert.equal((await fetch(url)).status,404);
    assert.equal((await fetch(base+'/media/../../publisher.json')).status,404);
    const playlist=await (await fetch(base+'/media/stream.m3u8')).text();assert.deepEqual(parseMedia(playlist),parseMedia(media));
    const beat=async body=>fetch(base+'/tv/audience',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)});
    const visitor={id:'a'.repeat(32),active:true,peers:true,version:'1'};
    const counted=await (await beat(visitor)).json();assert.equal(counted.activePlayers,1);assert.equal(counted.peerEnabledPlayers,1);assert.equal(counted.channel.state,'live');assert.equal(typeof counted.playerVersion,'string');
    assert(!JSON.stringify(counted).includes(visitor.id));assert.equal((await beat({...visitor,ip:'198.51.100.1'})).status,400);
    assert.equal((await fetch(base+'/tv/audience',{headers:{'Sec-Fetch-Site':'cross-site'}})).status,403);
    assert.equal((await (await beat({...visitor,active:false})).json()).activePlayers,0);
    ws=new WebSocket(base.replace('http:','ws:')+'/tv/swarm/relay',{origin:'https://receiver.example'});await once(ws,'open');
    const received=once(ws,'message');ws.send(JSON.stringify(['REQ','test',{kinds:[24170],'#x':[config.swarmId]}]));assert.equal(JSON.parse(String((await received)[0]))[0],'EOSE');
    const wrong=new WebSocket(base.replace('http:','ws:')+'/tv/swarm/relay',{origin:'https://foreign.example'});const [error]=await once(wrong,'error');assert.match(error.message,/403/);
    assert((await readFile(join(directory,'identity','publisher.json'),'utf8')).includes('secretKey'));
    expiredFragment=true;await new Promise(r=>setTimeout(r,1200));
    assert.notEqual((await (await fetch(base+'/tv/audience')).json()).channel.state,'ended','an unavailable fragment cannot end the channel');
    removed=true;let ended=false;
    for(let i=0;i<35;i++){await new Promise(r=>setTimeout(r,100));if((await (await fetch(base+'/tv/audience')).json()).channel.state==='ended'){ended=true;break;}}
    assert(ended,'persistent removal of the channel master confirms the broadcast end');
  }finally{ws?.terminate();await receiver?.close();upstream.closeAllConnections();await new Promise(r=>upstream.close(r));await rm(directory,{recursive:true,force:true});}
});
