import test from 'node:test';
import assert from 'node:assert/strict';
import { createTvPlayer } from '../web/tv/player.mjs';

class Element extends EventTarget {
  textContent = ''; hidden = false; disabled = false; dataset = {};
  click() { this.dispatchEvent(new Event('click')); }
}
class Video extends Element {
  native = false; currentSrc = ''; readyState = 0; plays = 0;
  canPlayType() { return this.native ? 'maybe' : ''; }
  set src(value) { this.currentSrc = value; }
  pause() {} load() {} removeAttribute() { this.currentSrc = ''; }
  async play() { this.plays++; }
}
const settle = () => new Promise(resolve => setImmediate(resolve));
function setup(overrides = {}) {
  const video = new Video(), watch = new Element(), edge = new Element();
  const status = new Element(), indicator = new Element(), placeholder = new Element();
  const timers = new Map(); let next = 0;
  const handles = [];
  class Hls {
    static Events = { MANIFEST_PARSED: 'manifest', ERROR: 'error' };
    static isSupported() { return true; }
    handlers = new Map(); destroyed = false;
    constructor(config) { this.config=config; handles.push(this); }
    on(name, fn) { this.handlers.set(name, fn); }
    loadSource(url) { this.url = url; }
    attachMedia() {} destroy() { this.destroyed = true; }
  }
  const fetches = [];
  const player = createTvPlayer({ video, watch, edge, status, indicator, placeholder,
    fetchImpl: async (url, options) => { fetches.push({ url, options }); return { ok: true }; },
    loadEngine: async () => ({ default: Hls }), schedule: (fn) => { timers.set(++next, fn); return next; },
    cancel: id => timers.delete(id), ...overrides });
  return { player, video, watch, edge, status, indicator, placeholder, timers, handles, fetches, Hls };
}

test('no media is fetched until the viewer chooses Watch live', async () => {
  const s=setup(); await settle(); assert.equal(s.fetches.length, 0);
  s.watch.click(); await settle(); assert.equal(s.fetches.length, 1);
  assert.equal(s.handles[0].url, '/tv/live/event-tv/index.m3u8');
  s.handles[0].handlers.get('manifest')(); await settle(); assert.equal(s.video.plays, 1);
  s.video.dispatchEvent(new Event('playing')); assert.equal(s.indicator.dataset.state, 'playing');
  s.watch.click(); assert.equal(s.handles[0].destroyed, true); assert.equal(s.timers.size, 0);
});

test('a late fetch cannot restart playback after Stop', async () => {
  let resolve;
  const s=setup({ fetchImpl: () => new Promise(r => { resolve=r; }) });
  s.watch.click(); s.watch.click(); resolve({ ok: true }); await settle();
  assert.equal(s.handles.length, 0); assert.equal(s.video.plays, 0); assert.equal(s.timers.size, 0);
});

test('a late hls import cannot attach after Stop', async () => {
  let finish;
  const s=setup({ loadEngine: () => new Promise(r => { finish=r; }) });
  s.watch.click(); await settle(); s.watch.click(); finish({ default:s.Hls }); await settle();
  assert.equal(s.handles.length, 0);
});

test('off-air recovery retries only while the viewer is watching', async () => {
  const s=setup({ fetchImpl: async () => ({ ok:false }) });
  s.watch.click(); await settle(); assert.equal(s.indicator.dataset.state, 'offline');
  assert.equal(s.timers.size, 1); s.player.stop(); assert.equal(s.timers.size, 0);
});

test('fatal engine errors dispose the old player before a retry', async () => {
  const s=setup(); s.watch.click(); await settle();
  s.handles[0].handlers.get('error')(null, { fatal:true });
  assert.equal(s.handles[0].destroyed, true); assert.equal(s.timers.size, 1);
  s.player.destroy(); assert.equal(s.timers.size, 0);
});

test('native HLS needs no hls.js module and tears down its source', async () => {
  const s=setup({ preferNative:true, loadEngine: () => { throw Error('must not load'); } });
  s.video.native=true; s.watch.click(); await settle();
  assert.equal(s.video.currentSrc, '/tv/live/event-tv/index.m3u8');
  assert.equal(s.video.plays, 1); s.player.destroy(); assert.equal(s.video.currentSrc, '');
});

test('Back to live uses the actual engine position', async () => {
  const s=setup(); s.watch.click(); await settle(); s.handles[0].liveSyncPosition=42;
  s.edge.click(); assert.equal(s.video.currentTime, 42); s.player.destroy();
});

test('autoplay rejection leaves native controls available without a retry loop', async () => {
  const s=setup({ preferNative:true }); s.video.native=true;
  s.video.play=async () => { throw Error('user gesture required'); };
  s.watch.click(); await settle();
  assert.equal(s.timers.size, 0);
  assert.match(s.status.textContent, /Press play/);
  s.player.destroy();
});

test('TV keeps twenty seconds of headroom and loads complete segments', async () => {
  const s=setup(); s.watch.click(); await settle();
  assert.equal(s.handles[0].config.lowLatencyMode, false);
  assert.equal(s.handles[0].config.liveSyncDuration, 20);
  assert(s.handles[0].config.liveMaxLatencyDuration > s.handles[0].config.liveSyncDuration);
  s.player.destroy();
});

test('stream information reports received media fields and clears on Stop', async () => {
  const info=Object.fromEntries(['picture','rate','codecs','buffer','delay'].map(key=>[key,new Element()]));
  const s=setup({info}); s.watch.click(); await settle();
  s.handles[0].handlers.get('manifest')(null,{levels:[{bitrate:2100000,videoCodec:'avc1.640029',audioCodec:'mp4a.40.2'}]});
  s.video.videoWidth=1280;s.video.videoHeight=720;s.video.currentTime=20;
  s.video.buffered={length:1,start:()=>10,end:()=>29};s.handles[0].latency=10.5;
  s.video.dispatchEvent(new Event('timeupdate'));
  assert.equal(info.picture.textContent,'1280 × 720');assert.equal(info.rate.textContent,'2.10 Mbit/s');
  assert.equal(info.buffer.textContent,'9.0 seconds');assert.match(info.delay.textContent,/10.5 seconds/);
  s.player.stop();assert.equal(info.rate.textContent,'Not yet reported');assert.equal(info.buffer.textContent,'Not playing');
});


test('NOW STREAMING ident follows a successful availability check and clears on Stop', async () => {
  const s=setup(); assert.equal(s.placeholder.dataset.kind, undefined);
  s.watch.click(); await settle(); assert.equal(s.placeholder.dataset.kind,'streaming');
  s.video.dispatchEvent(new Event('playing')); assert.equal(s.placeholder.hidden,true);
  s.player.stop(); assert.equal(s.placeholder.dataset.kind,'standby'); assert.equal(s.placeholder.hidden,false);
  s.player.destroy();
});

test('off-air responses never show the NOW STREAMING ident', async () => {
  const s=setup({fetchImpl:async()=>({ok:false})});
  s.watch.click();await settle();assert.equal(s.placeholder.dataset.kind,'standby');s.player.destroy();
});


test('private preview disables peer consent even when previously checked',async()=>{
 const peers=new Element();peers.checked=true;let calls=0;
 const s=setup({peers,locationImpl:new URL('http://127.0.0.1:9999/tv/'),loadPeers:async()=>{calls++;throw Error('forbidden');}});
 s.watch.click();await settle();assert(peers.disabled);assert.equal(peers.checked,false);assert.equal(calls,0);s.player.destroy();
});

test('a late peer setup cannot attach after Stop',async()=>{
 const peers=new Element();peers.checked=true;let finish,stops=0;
 const s=setup({peers,locationImpl:new URL('https://receiver.example/tv/'),loadPeers:async()=>({attachPeers:()=>new Promise(r=>finish=r)})});
 s.watch.click();await settle();s.player.stop();finish({stop:()=>stops++,stream:'/media/index.m3u8'});await settle();assert.equal(stops,1);assert.equal(s.handles[0].url,undefined);s.player.destroy();
});

test('failed peer media falls back to the receiver on reconnect',async()=>{
 const peers=new Element();peers.checked=true;let calls=0;
 const s=setup({peers,locationImpl:new URL('https://receiver.example/tv/'),loadPeers:async()=>({attachPeers:async()=>{calls++;return {stop(){},stream:'/media/index.m3u8'};}})});
 s.watch.click();await settle();assert.equal(s.handles[0].url,'/media/index.m3u8');
 s.handles[0].handlers.get('error')(null,{fatal:true});const retry=[...s.timers.values()][0];retry();await settle();assert.equal(calls,1);assert.equal(s.handles[1].url,'/tv/live/event-tv/index.m3u8');s.player.destroy();
});

test('pause and resume preserve the engine and do not repeat startup buffering',async()=>{
 const s=setup();s.video.paused=false;s.video.pause=()=>{s.video.paused=true;};s.video.play=async()=>{s.video.paused=false;s.video.plays++;};
 s.watch.click();await settle();const engine=s.handles[0];s.video.paused=false;s.video.dispatchEvent(new Event('playing'));
 s.player.togglePlayback();assert.equal(s.indicator.dataset.state,'paused');assert.equal(s.video.paused,true);assert.equal(engine.destroyed,false);
 s.player.togglePlayback();await settle();assert.equal(s.video.paused,false);assert.equal(s.handles.length,1);assert.equal(s.fetches.length,1);s.player.destroy();
});
test('a natural end shows Broadcast ended and never retries automatically',async()=>{
 const s=setup();s.watch.click();await settle();s.video.dispatchEvent(new Event('ended'));
 assert.equal(s.indicator.dataset.state,'ended');assert.equal(s.player.watching,false);assert.equal(s.timers.size,0);assert.equal(s.placeholder.hidden,false);
 assert.equal(s.handles[0].destroyed,true);s.player.destroy();
});
test('confirmed end drains buffered video, while reconnecting does not declare an end',async()=>{
 const s=setup();s.watch.click();await settle();s.video.readyState=4;s.video.currentTime=10;s.video.paused=false;s.video.buffered={length:1,start:()=>0,end:()=>20};let finished=0;s.handles[0].finish=()=>finished++;
 s.video.dispatchEvent(new Event('playing'));s.player.setChannelState('reconnecting');assert.equal(s.indicator.dataset.state,'playing');
 s.player.setChannelState('ended');assert.equal(finished,1);assert.equal(s.handles[0].destroyed,false);assert.equal(s.player.watching,true);
 s.video.currentTime=19.95;s.video.dispatchEvent(new Event('timeupdate'));assert.equal(s.indicator.dataset.state,'ended');assert.equal(s.timers.size,0);
 s.player.setChannelState('live');assert.equal(s.indicator.dataset.state,'idle');assert.equal(s.player.watching,false);s.player.destroy();
});
