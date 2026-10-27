// Generated media only. Two isolated browser peers against the receiver adapter.
import assert from 'node:assert/strict';
import {createServer,request as httpRequest} from 'node:http';
import {connect} from 'node:net';
import {readFile,mkdtemp,rm,writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {once} from 'node:events';
import {execFileSync} from 'node:child_process';
import {startReceiver} from '../target/tv-relayswarm/server.mjs';
const upstream=process.argv[2];assert(new URL(upstream).hostname==='127.0.0.1');
const home=await mkdtemp(join(tmpdir(),'sapflow-tv-peers-')),root=resolve('web/tv');
let receiver,browser,backend;
const front=createServer(async(req,res)=>{
  if(req.url.startsWith('/media/')||req.url.startsWith('/tv/swarm/')){
    if(!backend){res.writeHead(503);res.end();return;}
    const outgoing=httpRequest({hostname:'127.0.0.1',port:backend,path:req.url,headers:req.headers},incoming=>{res.writeHead(incoming.statusCode,incoming.headers);incoming.pipe(res);});outgoing.on('error',()=>{res.writeHead(503);res.end();});outgoing.end();return;
  }
  const file=req.url==='/tv/'?'index.html':req.url.slice('/tv/'.length);
  if(!req.url.startsWith('/tv/')||file.includes('..')||!(/^[a-z0-9./_-]+$/.test(file))){res.writeHead(404);res.end();return;}
  try{const data=await readFile(join(root,file));res.setHeader('Content-Type',file.endsWith('.mjs')?'text/javascript':file.endsWith('.wasm')?'application/wasm':file.endsWith('.css')?'text/css':'text/html');res.end(data);}catch{res.writeHead(404);res.end();}
});
front.on('upgrade',(req,socket,head)=>{const target=connect(backend,'127.0.0.1',()=>{target.write(`${req.method} ${req.url} HTTP/1.1\r\n`+Object.entries(req.headers).map(([k,v])=>`${k}: ${v}\r\n`).join('')+'\r\n');target.write(head);socket.pipe(target);target.pipe(socket);});target.on('error',()=>socket.destroy());socket.on('error',()=>target.destroy());socket.on('close',()=>target.destroy());});
try {
  front.listen(0,'127.0.0.1');await once(front,'listening');const base=`http://127.0.0.1:${front.address().port}`;
  receiver=await startReceiver({stateDir:home,origin:base,upstream,port:0});backend=receiver.server.address().port;
  for(let i=0;i<100;i++){if((await fetch(base+'/tv/swarm/config')).ok)break;await new Promise(r=>setTimeout(r,100));}
  const {chromium}=await import(pathToFileURL(process.env.SAPFLOW_PLAYWRIGHT_MODULE).href);
  browser=await chromium.launch({channel:'chrome',headless:true,args:['--autoplay-policy=no-user-gesture-required','--disable-background-networking','--disable-features=WebRtcHideLocalIpsWithMdns','--allow-loopback-in-peer-connection','--use-fake-ui-for-media-stream','--use-fake-device-for-media-stream']});
  const contexts=[],pages=[],errors=[];
  async function viewer(){
    const context=await browser.newContext({serviceWorkers:'block'});contexts.push(context);
    await context.route('**/*',route=>new URL(route.request().url()).origin===base?route.continue():route.abort());
    const page=await context.newPage();pages.push(page);page.on('pageerror',e=>errors.push(e.message));page.on('response',r=>{if(r.status()>=400)console.error('fixture HTTP '+r.status()+' '+new URL(r.url()).pathname);});page.on('console',m=>{if(m.type()==='error')console.error(m.text());});await page.goto(base+'/tv/');
    assert(await page.locator('#peer-assist').isDisabled(),'private preview UI never offers peers');
    await page.evaluate(async()=>{
      // Test-only synthetic microphone permission exposes loopback ICE candidates.
      // Production TV never requests a microphone or camera.
      const fake=await navigator.mediaDevices.getUserMedia({audio:true});for(const track of fake.getTracks())track.stop();
      const {default:Engine}=await import('/tv/cmaf-player.mjs');const {createHlsSwarm}=await import('/tv/vendor/relayswarm-hls-swarm-0.4.4.mjs');
      const config=await (await fetch('/tv/swarm/config')).json();
      const engine=new Engine();window.__errors=[];window.__seeks=[];
      const element=document.querySelector('video'),clock=Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype,'currentTime');
      Object.defineProperty(element,'currentTime',{get:()=>clock.get.call(element),set:value=>{const from=clock.get.call(element),ranges=element.buffered;let skippedBuffered=0;for(let i=0;i<ranges.length;i++)skippedBuffered+=Math.max(0,Math.min(value,ranges.end(i))-Math.max(from,ranges.start(i)));window.__seeks.push({from,to:value,skippedBuffered});clock.set.call(element,value);}});
      window.__waitSeconds=0;window.__waitAt=null;const video=document.querySelector('video');video.addEventListener('waiting',()=>{window.__waitAt??=performance.now();});video.addEventListener('playing',()=>{if(window.__waitAt!==null)window.__waitSeconds+=(performance.now()-window.__waitAt)/1000;window.__waitAt=null;});
      engine.on(Engine.Events.ERROR,(_,e)=>window.__errors.push(e));engine.on(Engine.Events.MANIFEST_PARSED,()=>document.querySelector('video').play());
      const swarm=createHlsSwarm({...config,mode:'peer-first',relays:[location.origin.replace('http:','ws:')+'/tv/swarm/relay'],iceServers:[],serve:true,
        coreWasm:'/tv/vendor/relayswarm-core-0.4.4.wasm',originFallbackMs:200,maxPeerWaitMs:500,originReserveMs:3000,sustainablePeerWait:true,traceRequests:200,
        getCommitment:async(context,{signal})=>{
          const frag=context.frag;const query=new URLSearchParams({path:new URL(context.url).pathname,sequence:String(frag.sn),discontinuity:String(frag.cc),level:String(frag.level),playlist:new URL(frag.baseurl).pathname,mediaType:frag.type});
          const r=await fetch('/tv/swarm/commitment?'+query,{signal});if(!r.ok)throw Error('Commitment HTTP '+r.status);const event=await r.json();if(window.__corruptCommitment)event.sig='0'.repeat(128);return event;
        }});
      window.__swarm=swarm;window.__engine=engine;swarm.attach(engine);engine.loadSource('/media/index.m3u8');engine.attachMedia(document.querySelector('video'));
    });
    await page.waitForFunction(()=>document.querySelector('video').currentTime>1,null,{timeout:30000});return page;
  }
  const first=await viewer();await first.waitForTimeout(3000);const second=await viewer();
  await second.waitForFunction(()=>window.__swarm.metrics().bytes.fromPeers>0&&document.querySelector('video').getVideoPlaybackQuality().totalVideoFrames>100,null,{timeout:45000});
  const metrics=await second.evaluate(()=>({metrics:window.__swarm.metrics(),errors:window.__errors,time:document.querySelector('video').currentTime,frames:document.querySelector('video').getVideoPlaybackQuality().totalVideoFrames,trace:window.__swarm.trace()}));
  assert(metrics.metrics.bytes.fromPeers>0);assert(metrics.frames>100);assert.deepEqual(metrics.errors,[]);
  await second.evaluate(()=>{window.__corruptCommitment=true;});
  await second.waitForFunction(()=>window.__swarm.metrics().segments.commitmentRejected>0,null,{timeout:20000});
  const corrupt=await second.evaluate(()=>({rejected:window.__swarm.metrics().segments.commitmentRejected,time:document.querySelector('video').currentTime,errors:window.__errors}));
  assert(corrupt.rejected>0);assert.deepEqual(corrupt.errors,[]);await second.evaluate(()=>{window.__corruptCommitment=false;});
  const publisherPid=Number(process.argv[3]);assert(Number.isSafeInteger(publisherPid)&&publisherPid>1);
  const processes=execFileSync('ps',['-axo','pid=,ppid=,comm='],{encoding:'utf8'}).trim().split('\n').map(line=>{const [pid,ppid,...command]=line.trim().split(/\s+/);return {pid:Number(pid),ppid:Number(ppid),command:command.join(' ')};});
  const descendants=new Set([publisherPid]);let changed=true;while(changed){changed=false;for(const p of processes)if(descendants.has(p.ppid)&&!descendants.has(p.pid)){descendants.add(p.pid);changed=true;}}
  const encoders=processes.filter(p=>descendants.has(p.pid)&&/(?:^|\/)ffmpeg$/.test(p.command));assert.equal(encoders.length,1,'only the fixture publisher encoder may be restarted');
  const reconnectBefore=await second.evaluate(()=>({time:document.querySelector('video').currentTime,wait:window.__waitSeconds,seeks:window.__seeks.length}));
  process.kill(encoders[0].pid,'SIGTERM');await second.waitForTimeout(10000);
  const reconnectAfter=await second.evaluate(count=>({seeks:window.__seeks.slice(count),time:document.querySelector('video').currentTime,wait:window.__waitSeconds,errors:window.__errors,ranges:Array.from({length:document.querySelector('video').buffered.length},(_,i)=>[document.querySelector('video').buffered.start(i),document.querySelector('video').buffered.end(i)]),offset:window.__engine.buffer.timestampOffset}),reconnectBefore.seeks);console.log(JSON.stringify({reconnectBefore,reconnectAfter}));
  assert(reconnectAfter.time-reconnectBefore.time>9,'timestamp epoch reconnect keeps playing');assert(reconnectAfter.seeks.every(s=>s.skippedBuffered<=.101),'reconnect never skips unplayed buffered programme');
  assert(reconnectAfter.wait-reconnectBefore.wait<.5,'timestamp epoch reconnect does not introduce sustained buffering');assert.deepEqual(reconnectAfter.errors,[]);
  const before=await second.evaluate(()=>document.querySelector('video').currentTime);
  await contexts[0].close();await second.waitForTimeout(10000);
  const fallback=await second.evaluate(()=>({time:document.querySelector('video').currentTime,metrics:window.__swarm.metrics(),errors:window.__errors}));
  assert(fallback.time>before+8,'playback survives departure');assert(fallback.metrics.bytes.fromOrigin>metrics.metrics.bytes.fromOrigin);assert.deepEqual(fallback.errors,[]);
  await second.evaluate(()=>{window.__swarm.stop();window.__engine.destroy();});
  const stopped=await second.evaluate(()=>window.__swarm.metrics());assert(stopped.stopped);assert.equal(stopped.peers.connected,0);
  assert.deepEqual(errors,[]);await writeFile('target/tv-browser/peers.json',JSON.stringify({metrics,corrupt,reconnectBefore,reconnectAfter,fallback,stopped},null,2));
  console.log(JSON.stringify({verifiedPeerBytes:metrics.metrics.bytes.fromPeers,frames:metrics.frames,departureProgress:fallback.time-before,connectionsAfterStop:stopped.peers.connected}));
}catch(error){if(browser){const pages=browser.contexts().flatMap(c=>c.pages());for(const page of pages)try{console.error(JSON.stringify(await page.evaluate(()=>({engine:{started:window.__engine?.started,url:window.__engine?.url,source:window.__engine?.source?.readyState,buffer:window.__engine?.buffer?.buffered?.length,video:document.querySelector('video')?.currentSrc,network:document.querySelector('video')?.networkState},metrics:window.__swarm?.metrics(),errors:window.__errors,trace:window.__swarm?.trace()}))));}catch{}}throw error;}
finally{await browser?.close();await receiver?.close();front.closeAllConnections();await new Promise(r=>front.close(r));await rm(home,{recursive:true,force:true});}
