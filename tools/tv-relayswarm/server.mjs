// Receiver-side, single-channel RelaySwarm adapter. MIT.
import {createServer} from 'node:http';
import {mkdir,writeFile,rename,unlink,readFile,readdir,stat} from 'node:fs/promises';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {WebSocketServer} from 'ws';
import {verifyEvent} from 'nostr-tools/pure';
import {createHlsPublisher} from 'relayswarm/hls-publisher';
import {loadPublisherState} from 'relayswarm/publisher-state';
import {ChannelControl} from './control.mjs';
import {Audience,ChannelState} from './audience.mjs';
import {PLAYER_VERSION} from '../../web/tv/version.mjs';
import {parseMaster,parseMedia} from '../../web/tv/playlist.mjs';

const ASSET=/^[a-f0-9]{12}_(?:init[0-9]{0,20}|seg[0-9]{1,20})\.mp4$/;
export function completePlaylist(parsed) {
  const segments=parsed.segments;
  const rows=['#EXTM3U','#EXT-X-VERSION:7','#EXT-X-INDEPENDENT-SEGMENTS',
    `#EXT-X-TARGETDURATION:${Math.ceil(Math.max(...segments.map(s=>s.duration)))}`,
    `#EXT-X-MEDIA-SEQUENCE:${segments[0].sn}`,`#EXT-X-DISCONTINUITY-SEQUENCE:${segments[0].cc}`];
  let init=null,cc=segments[0].cc;
  for(const s of segments) {
    while(cc<s.cc){rows.push('#EXT-X-DISCONTINUITY');cc++;}
    if(s.init!==init){rows.push(`#EXT-X-MAP:URI="${s.init}"`);init=s.init;}
    rows.push(`#EXTINF:${s.duration},`);if(s.gap)rows.push('#EXT-X-GAP');rows.push(s.name);
  }
  if(parsed.end)rows.push('#EXT-X-ENDLIST');return rows.join('\n')+'\n';
}
async function bytes(response,limit) {
  if(!response.ok)throw Object.assign(Error('Upstream unavailable'),{status:response.status});
  const reader=response.body.getReader(),chunks=[];let length=0;
  try{while(true){const {done,value}=await reader.read();if(done)break;length+=value.length;if(length>limit)throw Error('Upstream budget');chunks.push(value);}}
  catch(error){await reader.cancel().catch(()=>{});throw error;}
  finally{reader.releaseLock();}
  return Buffer.concat(chunks,length);
}
export function validEvent(event,swarmId,now=Date.now()) {
  return event&&[24170,24171].includes(event.kind)&&Number.isSafeInteger(event.created_at)
    &&Math.abs(event.created_at-Math.floor(now/1000))<=90&&typeof event.content==='string'
    &&event.content.length<=20000&&Array.isArray(event.tags)&&event.tags.length<=16
    &&event.tags.every(t=>Array.isArray(t)&&t.length<=4&&t.every(v=>typeof v==='string'&&v.length<=256))
    &&event.tags.filter(t=>t[0]==='x').length===1&&event.tags.some(t=>t[0]==='x'&&t[1]===swarmId)
    &&(event.kind!==24171||event.tags.some(t=>t[0]==='p'&&/^[0-9a-f]{64}$/.test(t[1])))&&verifyEvent(event);
}
export function validFilter(filter,swarmId) {
  return filter&&typeof filter==='object'&&!Array.isArray(filter)
    &&Object.keys(filter).every(k=>['kinds','#x','#p','since','limit'].includes(k))
    &&Array.isArray(filter.kinds)&&filter.kinds.length>0&&filter.kinds.length<=2&&filter.kinds.every(k=>[24170,24171].includes(k))
    &&Array.isArray(filter['#x'])&&filter['#x'].length===1&&filter['#x'][0]===swarmId
    &&(!filter['#p']||(Array.isArray(filter['#p'])&&filter['#p'].length===1&&/^[0-9a-f]{64}$/.test(filter['#p'][0])))
    &&(filter.since===undefined||Number.isSafeInteger(filter.since))&&(filter.limit===undefined||Number.isSafeInteger(filter.limit)&&filter.limit>=0&&filter.limit<=128);
}
function matches(event,f){return f.kinds.includes(event.kind)&&(!f['#p']||event.tags.some(t=>t[0]==='p'&&t[1]===f['#p'][0]))&&(f.since===undefined||event.created_at>=f.since);}
function bucket(capacity,rate){let tokens=capacity,at=Date.now();return()=>{const now=Date.now();tokens=Math.min(capacity,tokens+(now-at)*rate/1000);at=now;if(tokens<1)return false;tokens--;return true;};}
export async function startReceiver({stateDir,upstream='http://127.0.0.1:28888/live/event-tv/',origin,port=28777,host='127.0.0.1',controlToken}={}) {
  const source=new URL(upstream);
  if(source.protocol!=='http:'||source.hostname!=='127.0.0.1'||source.username||source.password||source.search||source.hash||source.pathname!=='/live/event-tv/')throw Error('Upstream must be the loopback TV channel');
  if(!origin||new URL(origin).origin!==origin)throw Error('An exact public origin is required');
  stateDir=resolve(stateDir);const mediaDir=resolve(stateDir,'media');await mkdir(mediaDir,{recursive:true,mode:0o700});
  for(const file of await readdir(mediaDir))if(ASSET.test(file)||['index.m3u8','stream.m3u8'].includes(file)||/^\.[a-z0-9_.]+\.tmp$/.test(file))await unlink(resolve(mediaDir,file));
  const identityDir=resolve(stateDir,'identity');await mkdir(identityDir,{mode:0o700,recursive:true});
  const identity=await loadPublisherState(resolve(identityDir,'publisher.json'),{manifest:'index.m3u8'});
  const audience=new Audience(),channel=new ChannelState(),control=new ChannelControl(controlToken,channel);
  const lifecycleFile=resolve(stateDir,'channel-session.json');
  if(control.token){try{if((await stat(lifecycleFile)).size>1024)throw Error('Lifecycle state budget');control.restore(JSON.parse(await readFile(lifecycleFile,'utf8')));}catch(error){if(error.code!=='ENOENT')throw Error('Private lifecycle state unavailable');}}
  let lifecycleUpdate=Promise.resolve();
  function updateLifecycle(value){const update=lifecycleUpdate.catch(()=>{}).then(async()=>{
    control.update(value);const temporary=lifecycleFile+'.tmp';await writeFile(temporary,JSON.stringify(control.snapshot())+'\n',{mode:0o600});await rename(temporary,lifecycleFile);
  });lifecycleUpdate=update;return update;}
  const controller=new AbortController(),cache=new Map(),presence=new Map();let publisher=null,fresh=0,closed=false,cacheBytes=0,active=0;
  async function get(name,limit){return bytes(await fetch(new URL(name,source),{redirect:'error',signal:AbortSignal.any([controller.signal,AbortSignal.timeout(6000)])}),limit);}
  async function atomic(name,data){const temporary=resolve(mediaDir,`.${name}.tmp`);await writeFile(temporary,data,{mode:0o600});await rename(temporary,resolve(mediaDir,name));}
  async function mirror() {
    let master;try{master=await get('index.m3u8',65536);}catch(error){error.channelMissing=[404,410].includes(error.status);throw error;}
    const variant=parseMaster(master.toString('ascii'));
    const parsed=parseMedia((await get(variant.manifest,65536)).toString('ascii'));
    channel.observe(parsed);
    if(parsed.segments.length>64)throw Error('Mirror window exceeded');
    const names=new Set(parsed.segments.flatMap(s=>s.gap?[s.init]:[s.init,s.name]));
    // Expire only unreferenced objects, retaining in-flight requests briefly.
    for(const [name,item]of cache)if(!names.has(name)&&Date.now()-item.at>30000){await unlink(resolve(mediaDir,name)).catch(()=>{});cache.delete(name);cacheBytes-=item.bytes;}
    for(const name of names) {
      if(!ASSET.test(name))throw Error('Unexpected mirror asset');
      const existing=cache.get(name);if(existing){existing.at=Date.now();continue;}
      const data=await get(name,8*1024*1024);
      if(cacheBytes+data.length>64*1024*1024||cache.size>=100)throw Error('Mirror cache full');
      await atomic(name,data);cache.set(name,{bytes:data.length,at:Date.now()});cacheBytes+=data.length;
    }
    await atomic('stream.m3u8',completePlaylist(parsed));await atomic('index.m3u8',master);
    if(!publisher)publisher=await createHlsPublisher({mediaDir,manifest:'index.m3u8',...identity,maxCacheEntries:128,maxConcurrent:4});
    fresh=Date.now();
  }
  const loop=(async()=>{while(!closed){try{await mirror();}catch(error){if(error.channelMissing)channel.unavailable(error.status);else if(Date.now()-channel.lastMediaAt>6000)channel.unavailable(503);}await new Promise(r=>{const timer=setTimeout(done,500);function done(){clearTimeout(timer);controller.signal.removeEventListener('abort',done);r();}controller.signal.addEventListener('abort',done,{once:true});if(closed)done();});}})();
  const controlBudget=bucket(8,1),audienceBudget=bucket(400,200),httpBudget=bucket(200,100),signBudget=bucket(40,20),eventBudget=bucket(100,50);
  function reply(res,status,data,mime='application/json'){res.writeHead(status,{'Content-Type':mime,'Cache-Control':'no-store','X-Content-Type-Options':'nosniff','Referrer-Policy':'no-referrer','Cross-Origin-Resource-Policy':'same-origin'});res.end(data);}
  const server=createServer(async(req,res)=>{
    if(active>=32||!(req.url==='/tv/audience'?audienceBudget():httpBudget())){reply(res,503,'Busy');return;}active++;
    try {
      if(req.headers['sec-fetch-site']==='cross-site'||(req.headers.origin&&req.headers.origin!==origin)){reply(res,403,'Forbidden');return;}
      const url=new URL(req.url,'http://localhost');
      if(url.pathname==='/tv/broadcast/control'&&!url.search){
        if(!control.token){reply(res,404,'Not found');return;}
        if(req.method!=='POST'){reply(res,405,'POST required');return;}
        if(!control.authorised(req.headers.authorization)){reply(res,403,'Forbidden');return;}
        if(!controlBudget()){reply(res,429,'Busy');return;}
        if(req.headers['content-type']!=='application/json'){reply(res,415,'JSON required');return;}
        let size=0;const chunks=[];
        for await(const chunk of req){size+=chunk.length;if(size>256)throw Object.assign(Error('Body budget'),{status:413});chunks.push(chunk);}
        let message;try{message=JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(Buffer.concat(chunks)));}catch{throw Object.assign(Error('Invalid lifecycle JSON'),{status:400});}
        await updateLifecycle(message);
        reply(res,200,JSON.stringify({accepted:true}));return;
      }
      if(url.pathname==='/tv/audience'&&!url.search){
        if(req.method==='POST'){
          if(!req.headers['content-type']?.startsWith('application/json')){reply(res,415,'JSON required');return;}
          let size=0;const chunks=[];
          for await(const chunk of req){size+=chunk.length;if(size>1024)throw Object.assign(Error('Body budget'),{status:413});chunks.push(chunk);}
          try{audience.update(JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(Buffer.concat(chunks))));}catch(error){if(!error.status)error.status=400;throw error;}
        }else if(!['GET','HEAD'].includes(req.method)){reply(res,405,'Method not allowed');return;}
        reply(res,200,req.method==='HEAD'?null:JSON.stringify({...audience.snapshot(),channel:channel.snapshot(),playerVersion:PLAYER_VERSION}));return;
      }
      if(!['GET','HEAD'].includes(req.method)){reply(res,405,'Method not allowed');return;}
      if(!publisher||Date.now()-fresh>10000){reply(res,503,'Off air');return;}
      if(url.pathname==='/tv/swarm/config'&&!url.search){reply(res,200,req.method==='HEAD'?null:JSON.stringify({swarmId:publisher.swarmId,streamEvent:publisher.streamEvent}));return;}
      if(url.pathname==='/tv/swarm/commitment') {
        if(!signBudget()){reply(res,429,'Busy');return;}
        const keys=['path','sequence','discontinuity','level','playlist','mediaType'];
        if([...url.searchParams.keys()].some(k=>!keys.includes(k))||keys.some(k=>url.searchParams.getAll(k).length!==1))throw Error('Invalid context');
        const event=await publisher.commitment(Object.fromEntries(url.searchParams));reply(res,200,req.method==='HEAD'?null:JSON.stringify(event));return;
      }
      const name=url.pathname.slice('/media/'.length);
      if(!url.pathname.startsWith('/media/')||url.search||!['index.m3u8','stream.m3u8'].includes(name)&&!ASSET.test(name)){reply(res,404,'Not found');return;}
      let data,mime;
      if(name.endsWith('.m3u8')){data=(await publisher.playlistSnapshot(url.pathname)).bytes;mime='application/vnd.apple.mpegurl';}
      else{if(!cache.has(name)){reply(res,404,'Not found');return;}data=await readFile(resolve(mediaDir,name));mime='video/mp4';}
      reply(res,200,req.method==='HEAD'?null:data,mime);
    }catch(error){reply(res,Number.isInteger(error.status)?error.status:503,'Unavailable');}finally{active--;}
  });
  server.maxHeadersCount=32;server.headersTimeout=10000;server.requestTimeout=10000;server.keepAliveTimeout=5000;server.maxConnections=256;
  const wss=new WebSocketServer({noServer:true,maxPayload:32768,perMessageDeflate:false,clientTracking:true});
  server.on('upgrade',(req,socket,head)=>{
    if(req.url!=='/tv/swarm/relay'||req.headers.origin!==origin||wss.clients.size>=128){socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n');return;}
    wss.handleUpgrade(req,socket,head,ws=>wss.emit('connection',ws));
  });
  function send(ws,value){if(ws.readyState!==1)return;if(ws.bufferedAmount>262144){ws.terminate();return;}ws.send(JSON.stringify(value));}
  wss.on('connection',ws=>{
    ws.subscriptions=new Map();const budget=bucket(20,5);ws.alive=true;ws.on('pong',()=>{ws.alive=true;});ws.on('error',()=>{});
    ws.on('message',(raw,binary)=>{
      if(binary||!budget()||!eventBudget()){ws.close(1008,'Rate or message limit');return;}
      try {
        const message=JSON.parse(raw.toString());if(!Array.isArray(message))throw Error('Message');
        const [type,id,...rest]=message;
        if(type==='CLOSE'&&message.length===2){ws.subscriptions.delete(id);return;}
        if(type==='REQ'&&typeof id==='string'&&id.length<=64&&rest.length===1&&ws.subscriptions.size<4&&validFilter(rest[0],identity.swarmId)){
          ws.subscriptions.set(id,rest[0]);for(const {event,at}of presence.values())if(Date.now()-at<60000&&matches(event,rest[0]))send(ws,['EVENT',id,event]);send(ws,['EOSE',id]);return;
        }
        if(type==='EVENT'&&message.length===2&&validEvent(id,identity.swarmId)) {
          if(id.kind===24170){presence.delete(id.pubkey);presence.set(id.pubkey,{event:id,at:Date.now()});while(presence.size>128)presence.delete(presence.keys().next().value);}
          for(const peer of wss.clients)for(const [sub,filter]of peer.subscriptions)if(matches(id,filter))send(peer,['EVENT',sub,id]);
          send(ws,['OK',id.id,true,'']);return;
        }
        send(ws,['NOTICE','Only this channel’s bounded, ephemeral rendezvous messages are accepted.']);
      }catch{ws.close(1008,'Invalid message');}
    });
  });
  const heartbeat=setInterval(()=>{for(const ws of wss.clients){if(!ws.alive){ws.terminate();continue;}ws.alive=false;ws.ping();}for(const [key,value]of presence)if(Date.now()-value.at>60000)presence.delete(key);},30000);
  await new Promise((r,j)=>{server.once('error',j);server.listen(port,host,r);});
  return {server,identity,async close(){closed=true;controller.abort();clearInterval(heartbeat);for(const ws of wss.clients)ws.terminate();wss.close();server.closeAllConnections();await new Promise(r=>server.close(r));await loop;await lifecycleUpdate.catch(()=>{});}};
}
if(process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href){
  const tokenFile=process.env.TV_CHANNEL_CONTROL_TOKEN_FILE;const controlToken=tokenFile?(await readFile(tokenFile,'utf8')).trim():undefined;
  const service=await startReceiver({controlToken,stateDir:process.env.STATE_DIRECTORY??process.env.TV_SWARM_STATE,origin:process.env.TV_PUBLIC_ORIGIN,upstream:process.env.TV_UPSTREAM,port:Number(process.env.TV_SWARM_PORT??28777)});
  for(const signal of ['SIGINT','SIGTERM'])process.once(signal,()=>{void service.close();});
}
