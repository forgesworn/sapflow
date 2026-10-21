import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

const source = readFileSync(new URL('./live-viewer.html', import.meta.url), 'utf8');
const context = vm.createContext({ArrayBuffer, Uint8Array, Date, btoa});
vm.runInContext(source.match(/<script id="diagnostics">([\s\S]*?)<\/script>/)[1], context);
const {Evidence, ranges, bytes, loaderClass} = context.SapflowViewerDiagnostics;
const plain = value => JSON.parse(JSON.stringify(value));
assert.deepEqual(plain(ranges({length:2, start:i => [0, 4][i], end:i => [2, 6][i]})), [[0, 2], [4, 6]]);
const evidence = new Evidence(16, 3);
const storage = new Uint8Array([9, 1, 2, 3, 9]);
evidence.loaded({payload:storage.subarray(1, 4), frag:{sn:1, url:'/part1.mp4'}});
storage[1] = 8;
assert.deepEqual([...evidence.media[0].bytes], [1, 2, 3]);
evidence.loaded({payload:new Uint8Array([0, 1]), frag:{sn:'initSegment', url:'/init.mp4'}});
evidence.playlist({networkDetails:{status:200, responseText:'#EXTM3U', getResponseHeader:() => 'application/vnd.apple.mpegurl'}});
const circular = {sn:2, elementaryStreams:{video:{startPTS:1, endPTS:2}}};
circular.loop = circular;
const failure = evidence.error({details:'fragParsingError', error:new Error('no media'), frag:circular,
  networkDetails:{status:200, getResponseHeader:() => 'video/mp4'}}, {currentTime:1, buffered:{length:0}});
assert.equal(failure.message, 'no media');
assert.equal(failure.response.contentType, 'video/mp4');
assert.equal(evidence.incident.playlists[0].text, '#EXTM3U');
for (let sn = 3; sn < 15; sn++) evidence.loaded({payload:new Uint8Array(4).fill(sn), frag:{sn}});
assert.ok(evidence.media.length <= 3 && evidence.used <= 16);
assert.ok(evidence.incidentUsed <= 16);
const beforeOversize = evidence.used;
evidence.loaded({payload:new Uint8Array(17), frag:{sn:999}});
assert.equal(evidence.used, beforeOversize);
assert.equal(evidence.incident.media[0].frag.sn, 1);
const packet = plain(evidence.packet({hlsVersion:'fixture'}));
assert.equal(packet.incident.media[0].base64, 'AQID');
assert.equal(packet.incident.init[0].base64, 'AAE=');
assert.equal(packet.incident.error.frag.elementaryStreams.video.endPTS, 2);
assert.doesNotThrow(() => JSON.stringify(packet));
for (let i = 0; i < 150; i++) evidence.error({details:'fixture'}, null);
assert.equal(evidence.failures.length, 128);
const transferred = new Uint8Array([7, 8]).buffer;
const loaderEvidence = new Evidence();
class Loader {
  load(context, config, callbacks) { callbacks.onSuccess({data:transferred}, {}, context, {}); }
}
const RecordingLoader = loaderClass(Loader, loaderEvidence);
new RecordingLoader().load({frag:{sn:12}}, {}, {onSuccess:() => {
  structuredClone(transferred, {transfer:[transferred]});
}});
assert.deepEqual([...loaderEvidence.media[0].bytes], [7, 8]);
assert.equal(bytes(transferred), null);
assert.equal(loaderEvidence.captureErrors, 0);
console.log('Viewer diagnostics: buffer holes, byte ownership, bounded retention, error context and export passed');

// Deterministic recovery clock: only one native timer may exist at a time.
vm.runInContext(source.match(/<script id="recovery">([\s\S]*?)<\/script>/)[1], context);
const Recovery = context.SapflowViewerRecovery;
function recoveryFixture() {
  let now = 0, next = 0;
  const timers = new Map(), calls = [], states = [];
  const recovery = new Recovery({
    network:() => calls.push('network'), media:() => calls.push('media'), stop:() => calls.push('stop'),
    wall:() => now, now:() => now, onState:state => states.push(plain(state)),
    setTimer:(callback, delay) => {
      assert.equal(timers.size, 0, 'more than one recovery timer');
      const id = ++next; timers.set(id, {callback, at:now + delay}); return id;
    },
    clearTimer:id => timers.delete(id),
  });
  return {recovery, calls, states, timers,
    step() {
      assert.equal(timers.size, 1, 'expected one scheduled action');
      const [id, task] = [...timers][0]; timers.delete(id); now = task.at; task.callback();
    },
    progress(time, frames = time * 30, ready = 4, paused = false) { recovery.progress({time, frames, ready, paused}); },
  };
}
const networkFatal = {fatal:true, type:'networkError', details:'levelLoadError'};
const mediaFatal = {fatal:true, type:'mediaError', details:'bufferAppendError'};
{
  const f = recoveryFixture();
  for (let i = 0; i < 20; i++) f.recovery.error(networkFatal);
  assert.equal(f.timers.size, 1);
  f.step(); assert.deepEqual(f.calls, ['network']);
  f.progress(1); f.progress(1.5, 30); // Clock moved; decoded frames did not.
  assert.equal(f.recovery.attempts.network, 1);
  f.progress(1.5, 45, 1); // Insufficient media ready state is not progress.
  assert.equal(f.recovery.attempts.network, 1);
  f.progress(1.5, 45);
  assert.equal(f.recovery.phase, 'playing');
  assert.equal(f.recovery.attempts.network, 0);
  assert.equal(f.timers.size, 0);
  f.recovery.error(networkFatal); f.step(); // Later genuine outage gets a new budget.
  assert.deepEqual(f.calls, ['network', 'network']);
  assert.ok(f.recovery.history.some(action => action.action === 'actual-progress'));
}
{
  const f = recoveryFixture();
  f.recovery.error(networkFatal);
  while (f.timers.size) f.step(); // No further error callbacks; watchdog must terminate.
  assert.deepEqual(f.calls, ['network', 'network', 'network', 'stop']);
  assert.equal(f.recovery.terminal, 'network-recovery-exhausted');
  f.recovery.error(networkFatal); f.recovery.resume();
  assert.equal(f.timers.size, 0);
}
{
  const f = recoveryFixture();
  f.recovery.error(mediaFatal);
  while (f.timers.size) f.step();
  assert.deepEqual(f.calls, ['media', 'media', 'stop']);
  assert.equal(f.recovery.terminal, 'media-recovery-exhausted');
}
{
  const f = recoveryFixture();
  f.recovery.error(networkFatal); f.recovery.pause();
  assert.equal(f.timers.size, 0);
  f.recovery.error(mediaFatal); f.progress(1); f.progress(2);
  assert.equal(f.recovery.phase, 'paused');
  assert.deepEqual(f.calls, []);
  f.recovery.resume(); assert.equal(f.timers.size, 1);
  f.step(); assert.deepEqual(f.calls, ['media']);
  f.recovery.pause(); // Pause during an attempt cancels its watchdog, retaining budget.
  assert.equal(f.recovery.attempts.media, 1);
  assert.equal(f.timers.size, 0);
  f.recovery.resume(); f.step();
  assert.deepEqual(f.calls, ['media', 'media']);
}
{
  const f = recoveryFixture();
  f.recovery.error(networkFatal); f.recovery.end(true);
  assert.equal(f.timers.size, 0);
  assert.equal(f.recovery.phase, 'ending');
  assert.deepEqual(f.calls, []); // ENDLIST still allows already listed media to drain.
  f.recovery.error(networkFatal); f.recovery.resume(); f.progress(1); f.progress(2);
  assert.equal(f.timers.size, 0);
  f.recovery.end(); assert.equal(f.recovery.phase, 'ended');
  assert.deepEqual(f.calls, ['stop']);
}
for (const status of [401, 403]) {
  const f = recoveryFixture();
  f.recovery.error(networkFatal);
  f.recovery.error({...networkFatal, response:{code:status}});
  assert.equal(f.timers.size, 0);
  assert.equal(f.recovery.terminal, 'authorisation-failed');
  f.recovery.resume(); assert.deepEqual(f.calls, ['stop']);
}
{
  const f = recoveryFixture();
  f.recovery.error({fatal:true, type:'networkError', details:'levelParsingError', reason:'media sequence mismatch7 /new_seg7.mp4'});
  assert.equal(f.recovery.terminal, 'origin-media-sequence-mismatch');
  assert.deepEqual(f.calls, ['stop']);
  assert.equal(f.timers.size, 0); // No source reload hides a broken origin timeline.
}
{
  const f = recoveryFixture();
  f.recovery.error({...networkFatal, fatal:false});
  assert.equal(f.timers.size, 0);
  f.progress(1); f.progress(2);
  assert.equal(f.recovery.phase, 'playing');
  f.recovery.waiting(); assert.equal(f.recovery.phase, 'waiting');
  assert.equal(f.timers.size, 0); // Waiting state does not fabricate frame progress.
  for (let i = 0; i < 100; i++) { f.recovery.pause(); f.recovery.resume(); }
  assert.equal(f.recovery.history.length, 64);
  assert.equal(f.recovery.history.some(item => /recreate|reload-source/.test(item.action)), false);
}
console.log('Viewer recovery: single-flight budgets/watchdogs, actual progress, pause/resume, End/auth, origin mismatch and bounded history passed');

// Execute the actual page wiring against deterministic media/Hls adapters.
// This catches status-timer regressions and pause intent during reattachment.
function pageFixture() {
  let clock = 0, timerId = 0;
  const timers = new Map(), intervals = [];
  class Element {
    constructor() { this.handlers = new Map(); this.textContent = ''; }
    addEventListener(type, callback) { const list = this.handlers.get(type) || []; list.push(callback); this.handlers.set(type, list); }
    emit(type) { for (const callback of this.handlers.get(type) || []) callback({type}); }
  }
  const video = Object.assign(new Element(), {currentTime:0, readyState:4, paused:true, autoplay:true,
    playbackRate:1, frames:0, plays:0, buffered:{length:0},
    getVideoPlaybackQuality() { return {totalVideoFrames:this.frames, droppedVideoFrames:0}; },
    play() { this.plays++; this.paused = false; this.emit('play'); return Promise.resolve(); },
    pause() { this.paused = true; this.emit('pause'); },
  });
  const elements = new Map([['video', video], ...['status', 'toggle', 'live', 'export'].map(id => ['#' + id, new Element()])]);
  class Hls {
    static Events = {LEVEL_LOADED:'level', ERROR:'error', MANIFEST_PARSED:'manifest', MEDIA_ATTACHED:'attached'};
    static DefaultConfig = {loader:class {}};
    static version = 'test-pinned';
    static instances = [];
    constructor() { this.events = new Map(); this.sources = []; this.calls = []; this.latency = 1; Hls.instances.push(this); }
    on(type, callback) { const list = this.events.get(type) || []; list.push(callback); this.events.set(type, list); }
    emit(type, data) { for (const callback of this.events.get(type) || []) callback(type, data); }
    loadSource(url) { this.sources.push(url); }
    attachMedia(media) { this.video = media; }
    startLoad() { this.calls.push('network'); }
    stopLoad() { this.calls.push('stop'); }
    recoverMediaError() { this.calls.push('media'); this.video.pause(); }
  }
  const sandbox = {Hls, URLSearchParams, ArrayBuffer, Uint8Array, Date, btoa,
    location:{search:'?event=event-recovery-test'}, performance:{now:() => clock},
    document:{querySelector:name => elements.get(name)}, console:{warn:() => {}},
    setInterval:callback => { intervals.push(callback); return intervals.length; },
    setTimeout:(callback, delay) => {
      assert.equal(timers.size, 0, 'page has multiple recovery timers');
      const id = ++timerId; timers.set(id, {callback, at:clock + delay}); return id;
    },
    clearTimeout:id => timers.delete(id),
  };
  sandbox.window = sandbox;
  const page = vm.createContext(sandbox);
  const inline = [...source.matchAll(/<script(?: id="[^"]+")?>([\s\S]*?)<\/script>/g)].map(match => match[1]);
  assert.equal(inline.length, 3);
  inline.forEach(script => vm.runInContext(script, page));
  const hls = Hls.instances[0];
  return {page, hls, video, elements, timers, instances:Hls.instances,
    step() {
      assert.equal(timers.size, 1);
      const [id, task] = [...timers][0]; timers.delete(id); clock = task.at; task.callback();
    },
    refresh() { intervals.forEach(callback => callback()); },
    advance(time, frames) { video.currentTime = time; video.frames = frames; video.emit('timeupdate'); },
    manifest() { hls.emit(Hls.Events.MANIFEST_PARSED); },
    error(data) { hls.emit(Hls.Events.ERROR, data); },
    attach() { hls.emit(Hls.Events.MEDIA_ATTACHED); },
    level(live) { hls.emit(Hls.Events.LEVEL_LOADED, {details:{live}}); },
  };
}
{
  const f = pageFixture(); f.manifest(); f.advance(0, 0); f.advance(0.5, 15); f.refresh();
  assert.match(f.elements.get('#status').textContent, /^Playing/);
  f.video.readyState = 1; f.video.emit('waiting'); f.refresh();
  assert.match(f.elements.get('#status').textContent, /Waiting/);
  f.error(networkFatal); f.step();
  assert.deepEqual(f.hls.calls, ['network']);
  f.error({fatal:true, type:'networkError', details:'levelParsingError', reason:'media sequence mismatch7'});
  f.refresh();
  assert.match(f.elements.get('#status').textContent, /origin-media-sequence-mismatch/);
  assert.equal(f.video.paused, false); // A false paused flag cannot overwrite fatal status.
  assert.equal(f.timers.size, 0);
  assert.equal(f.instances.length, 1);
  assert.deepEqual(f.hls.sources, ['/live/event-recovery-test/index.m3u8']);
  assert.equal(f.page.metrics().recovery.terminal, 'origin-media-sequence-mismatch');
  assert.equal(f.page.evidence.failures.length, 2);
}
{
  const f = pageFixture(); f.manifest(); f.advance(0, 0); f.advance(0.5, 15);
  f.error(mediaFatal); f.step();
  assert.equal(f.page.recovery.paused, false, 'internal detach must not become user pause');
  assert.equal(f.video.paused, true);
  assert.equal(f.elements.get('#toggle').textContent, 'Pause');
  f.elements.get('#toggle').onclick(); // Explicit pause while detached is authoritative.
  f.attach();
  assert.equal(f.video.plays, 1, 'media reattachment must preserve intentional pause');
  assert.equal(f.page.recovery.paused, true);
  assert.equal(f.timers.size, 0);
  f.elements.get('#toggle').onclick(); f.step(); f.attach();
  assert.equal(f.page.recovery.paused, false);
  assert.deepEqual(f.hls.calls, ['media', 'media']);
  f.video.emit('pause'); // Late queued reset event after play() is ignored.
  assert.equal(f.page.recovery.paused, false);
  f.advance(1, 30);
  assert.equal(f.page.recovery.phase, 'playing');
  assert.equal(f.timers.size, 0);
  f.video.pause(); assert.equal(f.page.recovery.paused, true);
  f.level(false); f.video.emit('ended'); f.refresh();
  assert.equal(f.page.recovery.phase, 'ended');
  assert.match(f.elements.get('#status').textContent, /^Broadcast ended/);
  assert.equal(f.instances.length, 1);
}
{
  const f = recoveryFixture(); let throws = 0;
  f.recovery.network = () => { throws++; throw Error('synchronous loader failure'); };
  f.recovery.error(networkFatal);
  while (f.timers.size) f.step();
  assert.equal(throws, 3);
  assert.equal(f.recovery.terminal, 'network-recovery-exhausted');
}
console.log('Viewer page wiring: fatal/waiting status, retained evidence, same source, decoder reattach pause intent and bounded thrown actions passed');
