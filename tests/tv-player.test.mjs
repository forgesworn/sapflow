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
    constructor() { handles.push(this); }
    on(name, fn) { this.handlers.set(name, fn); }
    loadSource(url) { this.url = url; }
    attachMedia() {} destroy() { this.destroyed = true; }
  }
  const fetches = [];
  const player = createTvPlayer({ video, watch, edge, status, indicator, placeholder,
    fetchImpl: async (url, options) => { fetches.push({ url, options }); return { ok: true }; },
    loadHls: async () => ({ default: Hls }), schedule: (fn) => { timers.set(++next, fn); return next; },
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
  const s=setup({ loadHls: () => new Promise(r => { finish=r; }) });
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
  const s=setup({ preferNative:true, loadHls: () => { throw Error('must not load'); } });
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
