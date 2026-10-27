// Real decoding gate. Use only an operator-owned loopback preview URL.
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
import { writeFile } from 'node:fs/promises';

const [base, output, secondsText='30', engine='chrome'] = process.argv.slice(2);
const url=new URL(base);
assert.equal(url.hostname, '127.0.0.1'); assert.equal(url.protocol, 'http:');
assert(output); const seconds=Number(secondsText); assert(seconds>=10 && seconds<=3600);
const module=process.env.SAPFLOW_PLAYWRIGHT_MODULE;
const { chromium, webkit }=await import(module ? pathToFileURL(module).href : 'playwright');
let browser;
const requests=[], errors=[], failures=[];
let startupFailures=2;
try {
  browser=engine==='webkit' ? await webkit.launch({ headless:true }) : await chromium.launch({ channel:'chrome', headless:true,
    args:['--disable-background-networking','--disable-quic','--autoplay-policy=no-user-gesture-required'] });
  const context=await browser.newContext({ viewport:{ width:1280,height:1000 }, serviceWorkers:'block' });
  await context.route('**/*', route => {
    const request=route.request(), target=new URL(request.url());
    if (target.protocol==='blob:' || target.origin===url.origin) {
      requests.push(target.pathname);
      if (target.pathname==='/tv/live/event-tv/index.m3u8' && startupFailures>0) {
        startupFailures--; return route.fulfill({status:404,body:'Off air'});
      }
      return route.continue();
    }
    failures.push('Unexpected external request'); return route.abort();
  });
  const page=await context.newPage();
  page.on('pageerror', error=>errors.push(error.message));
  await page.addInitScript(() => {
    window.__peerAttempts=0;
    window.RTCPeerConnection=class { constructor() { window.__peerAttempts++; throw Error('Peer connection forbidden'); } };
  });
  await page.goto(base);
  assert.equal(await page.locator('h1').textContent(), 'ForgeSworn TV');
  await page.waitForTimeout(1000);
  assert.equal(requests.some(x=>x.includes('/live/')), false, 'no media before watch intent');
  await page.locator('#watch').click();
  await page.waitForFunction(()=>document.querySelector('#indicator').dataset.state==='offline');
  await page.waitForFunction(() => {
    const v=document.querySelector('video');
    return v.currentTime>1 && (v.getVideoPlaybackQuality?.().totalVideoFrames ?? v.webkitDecodedFrameCount ?? 0)>10;
  }, null, { timeout:90000 });
  const snapshots=[];
  const sample=()=>page.evaluate(() => {
    const v=document.querySelector('video'), q=v.getVideoPlaybackQuality?.();
    return { time:v.currentTime, frames:q?.totalVideoFrames ?? v.webkitDecodedFrameCount ?? 0,
      dropped:q?.droppedVideoFrames ?? v.webkitDroppedFrameCount ?? 0,
      width:v.videoWidth,height:v.videoHeight,readyState:v.readyState,paused:v.paused,
      audioBytes:v.webkitAudioDecodedByteCount ?? null,peerAttempts:window.__peerAttempts,
      state:document.querySelector('#indicator').dataset.state };
  });
  const initial=await sample(); snapshots.push(initial);
  const audio=await page.evaluate(async () => {
    const Audio=window.AudioContext ?? window.webkitAudioContext;
    if (!Audio) return { available:false };
    const context=new Audio(); await context.resume();
    const source=context.createMediaElementSource(document.querySelector('video'));
    const analyser=context.createAnalyser(), silent=context.createGain(); silent.gain.value=0;
    source.connect(analyser); analyser.connect(silent); silent.connect(context.destination);
    await new Promise(resolve=>setTimeout(resolve,500));
    const values=new Float32Array(analyser.fftSize); analyser.getFloatTimeDomainData(values);
    const rms=Math.sqrt(values.reduce((sum,x)=>sum+x*x,0)/values.length);
    window.__audioProbe={ context,source,analyser,silent };
    return { available:true,rms };
  });
  if (engine==='chrome') assert(audio.available && audio.rms>0.00001,'decoded synthetic audio reaches Web Audio');
  await page.screenshot({ path:output+'.png', fullPage:true });
  const started=Date.now();
  while (Date.now()-started<seconds*1000) { await page.waitForTimeout(5000); snapshots.push(await sample()); }
  const final=snapshots.at(-1);
  assert(final.frames>initial.frames+seconds*18, 'sustained decoding exceeds 18 frames per second');
  assert(final.time>initial.time+seconds*.85, 'playback keeps pace with wall time');
  assert(final.dropped-initial.dropped < (final.frames-initial.frames)*.1, 'fewer than 10% dropped frames');
  assert(snapshots.filter(s=>s.state==='buffering').length<=1, 'no sustained buffering');
  assert.equal(final.peerAttempts,0); assert(final.width>=320 && final.height>=180);
  await page.locator('#live-edge').click();
  await page.waitForFunction(()=>document.querySelector('#indicator').dataset.state==='playing');
  await page.locator('#watch').click();
  await page.waitForTimeout(1000);
  const stopped=await sample(); assert(stopped.paused); assert.equal(stopped.state,'idle');
  const count=requests.length; await page.waitForTimeout(2500); assert.equal(requests.length,count,'Stop ends requests');
  assert.equal(errors.length,0,errors.join('\n')); assert.equal(failures.length,0);
  await page.setViewportSize({ width:390,height:844 }); await page.screenshot({ path:output+'-mobile.png', fullPage:true });
  await writeFile(output+'.json',JSON.stringify({ engine, wallSeconds:(Date.now()-started)/1000,initial,final,stopped,
    snapshots,audio,offAirRecovered:startupFailures===0,requests:requests.length,externalRequests:failures.length,pageErrors:errors },null,2)+'\n');
  console.log(JSON.stringify({ engine,frames:final.frames-initial.frames,advance:final.time-initial.time,
    dimensions:[final.width,final.height],audioBytes:final.audioBytes,externalRequests:failures.length,stopVerified:true }));
} catch (error) { console.error(error); throw error; }
finally { await browser?.close(); }
