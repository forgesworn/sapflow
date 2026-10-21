import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

const html = readFileSync(new URL('./native-live-viewer.html', import.meta.url), 'utf8');
const context = vm.createContext({performance});
vm.runInContext(html.match(/<script id="observation">([\s\S]*?)<\/script>/)[1], context);
const {sourceFor, Observation} = context.SapflowNativeHls;
const plain = value => JSON.parse(JSON.stringify(value));
assert.equal(sourceFor('event-native_1'), '/live/event-native_1/index.m3u8');
for (const event of ['', '../stream', 'event?a=b', 'https://example.invalid', '%2f', 'x/y', 'a'.repeat(129), null]) {
  assert.throws(() => sourceFor(event));
}
function fixture(supported = 'maybe', frameCounter = true, presentation = false) {
  let now = 0, assignments = 0, decoded = 0;
  let nextCallback = 0, requested = 0, cancelled = 0;
  const handlers = new Map(), callbacks = new Map();
  const video = {currentTime:0, videoWidth:720, videoHeight:1280, readyState:1,
    networkState:2, paused:false, ended:false, error:null,
    buffered:{length:1, start:() => 0, end:() => 2}, seekable:{length:0},
    canPlayType:() => supported,
    addEventListener:(type, handler) => handlers.set(type, handler),
    removeEventListener:(type, handler) => { assert.equal(handlers.get(type), handler); handlers.delete(type); },
    set src(value) { assignments++; assert.equal(value, '/live/event-native/index.m3u8'); },
    load:() => assert.fail('fixture must not reload the native source')};
  if (frameCounter) video.getVideoPlaybackQuality = () => ({totalVideoFrames:decoded});
  if (presentation) {
    video.requestVideoFrameCallback = callback => {
      requested++;
      if (presentation === 'throws') throw new Error('Unavailable');
      const id = nextCallback++; callbacks.set(id, callback);
      assert.equal(callbacks.size, 1, 'only one presentation callback may be pending');
      return id;
    };
    video.cancelVideoFrameCallback = id => { cancelled++; callbacks.delete(id); };
  }
  const observation = new Observation(video, 'event-native', {now:() => now,
    wall:() => 'fixture-time', instanceId:'stable-instance'});
  return {video, handlers, observation, assignments:() => assignments,
    callbackPending:() => callbacks.size, requested:() => requested, cancelled:() => cancelled,
    peekCallback:() => callbacks.values().next().value,
    present(mediaTime = video.currentTime) {
      const next = callbacks.entries().next().value;
      if (!next) return false;
      callbacks.delete(next[0]); next[1](now, {mediaTime}); return true;
    },
    tick(ms, time = video.currentTime, frames = decoded) {
      now += ms; video.currentTime = time; decoded = frames; return observation.sample();
    }};
}
assert.throws(() => fixture(''), /Native HLS is unavailable/);
{
  const f = fixture(); f.video.readyState = 4;
  assert.equal(f.tick(1, 0, 30).state, 'waiting'); // Frame counter becomes visible before media time.
  assert.equal(f.tick(1000, 1, 30).state, 'playing');
  f.tick(1000, 0, 40); // A backwards media-time reset establishes a fresh baseline.
  assert.equal(f.tick(4000, 0, 40).state, 'stalled');
  assert.equal(f.tick(1000, 1, 70).state, 'playing');
}
{
  const f = fixture(); f.video.readyState = 4;
  f.tick(1000, 1, 0); // Media time and decoder counters can update on separate observations.
  assert.equal(f.tick(1, 1, 30).state, 'playing');
  f.tick(1000, 2, 30);
  assert.equal(f.tick(1, 2, 60).state, 'playing');
  assert.equal(f.tick(4000, 3, 60).state, 'unverified'); // Clock-only progress remains insufficient.
  assert.equal(f.tick(1, 3, 90).state, 'playing');
  f.tick(1000, 4, 0); // Decoder replacement resets its counter, not necessarily media time.
  assert.equal(f.tick(4000, 4, 0).state, 'stalled');
  assert.equal(f.tick(1000, 5, 30).state, 'playing');
}
{
  const f = fixture();
  assert.equal(f.observation.sample().state, 'waiting');
  assert.equal(f.tick(4000).state, 'stalled'); // paused=false never means playing by itself.
  f.video.readyState = 4;
  assert.equal(f.tick(1000, 1, 0).state, 'unverified'); // Clock progress cannot prove presentation.
  assert.equal(f.tick(1000, 2, 30).state, 'playing');
  assert.equal(f.tick(4000).state, 'stalled');
  f.video.paused = true;
  assert.equal(f.tick(1000, 3, 60).state, 'paused');
  f.video.paused = false; f.video.error = {code:3};
  assert.equal(f.tick(1000, 4, 90).state, 'error');
  f.video.error = null; f.video.ended = true;
  assert.equal(f.tick(1000).state, 'ended');
  assert.equal(f.assignments(), 1);
}
{
  const f = fixture(); f.video.readyState = 4;
  f.tick(1000, 1, 30);
  for (const [width, height, time] of [[1280, 720, 2], [720, 1280, 3]]) {
    Object.assign(f.video, {videoWidth:width, videoHeight:height}); f.handlers.get('resize')();
    const metrics = f.tick(1000, time, time * 30);
    assert.equal(metrics.state, 'playing'); assert.equal(metrics.instanceId, 'stable-instance');
    assert.equal(metrics.width, width); assert.equal(metrics.height, height);
    assert.equal(metrics.sourceAssignments, 1);
  }
  assert.equal(f.assignments(), 1);
  for (let i = 0; i < 200; i++) f.handlers.get('waiting')();
  assert.equal(f.observation.sample().events.length, 128);
  const snapshot = f.observation.sample(); snapshot.events[0].type = 'mutated';
  assert.equal(f.observation.sample().events[0].type, 'waiting');
  f.observation.dispose(); f.observation.dispose(); assert.equal(f.handlers.size, 0);
}
{
  const f = fixture('probably', false); f.video.readyState = 4;
  const metrics = f.tick(1000, 1);
  assert.equal(metrics.state, 'waiting'); assert.equal(metrics.decodedFrames, null);
  assert.equal(f.tick(4000, 5).state, 'unverified'); // No frame signal cannot certify playback.
  assert.deepEqual(plain(metrics.buffered), [[0, 2]]);
  assert.equal(metrics.bufferedAheadSeconds, 1);
  f.tick(1000, 3); assert.equal(f.observation.sample().bufferedAheadSeconds, null);
}
{
  const f = fixture('probably', true, true); f.video.readyState = 4;
  f.present(1);
  assert.equal(f.tick(1000, 1, 775).state, 'playing');
  for (let second = 2; second <= 7; second++) {
    for (let frame = 0; frame < 29; frame++) f.present(second + frame / 30);
    const metrics = f.tick(1000, second, 775);
    assert.equal(metrics.state, 'playing', 'frozen decoded quality must not override progressing presentation');
    assert.equal(metrics.decodedFrames, 775);
    assert.equal(metrics.presentationSupported, true);
  }
  const metrics = f.observation.sample();
  assert.equal(metrics.presentedCallbacks, 175);
  assert.equal(metrics.lastPresentedMediaTime, 7 + 28 / 30);
  assert.equal(f.callbackPending(), 1); assert.equal(f.assignments(), 1);
  f.present(8);
  assert.equal(f.tick(1000, 8, 0).state, 'playing'); // Decoder-stat reset cannot hide real presentation.
  f.present(NaN); f.present(Infinity);
  assert.equal(f.observation.sample().lastPresentedMediaTime,8); // Retain finite scalar metadata only.
  f.observation.dispose();
}
{
  const f = fixture('probably', true, true); f.video.readyState = 4;
  const unknown = f.tick(4000, 4, 120);
  assert.equal(unknown.state, 'unverified'); // Decoder activity cannot replace absent presentation.
  assert.equal(unknown.stateReason, 'presentation-callback-unresponsive');
  assert.equal(unknown.progressSignal, 'presentation-callback');
  assert.equal(unknown.clockProgressAgeMs, 0);
  assert.equal(unknown.lastClockProgressElapsedMs, 4000);
  assert.equal(unknown.selectedFrameProgressAgeMs, 4000);
  assert.equal(unknown.lastSelectedFrameProgressElapsedMs, null);
  assert.equal(unknown.lastProgressElapsedMs, null);
  assert.equal(unknown.decodedFrames, 120);
  f.present(4);
  assert.equal(f.tick(1, 4, 120).state, 'playing'); // Clock-first, callback-later observations combine.
  f.present(5);
  assert.equal(f.tick(1000, 5, 120).state, 'playing'); // Callback-first, clock-later also combines.
  for (let frame = 0; frame < 100; frame++) f.present(5);
  assert.equal(f.tick(4000, 5, 120).state, 'stalled'); // Callbacks alone cannot qualify a frozen clock.
  f.video.paused = true; f.present(6);
  assert.equal(f.tick(1000, 6, 120).state, 'paused');
  f.video.paused = false; f.video.error = {code:3};
  assert.equal(f.tick(1000, 7, 120).state, 'error');
  f.video.error = null; f.video.ended = true;
  assert.equal(f.tick(1000, 7, 120).state, 'ended');
  f.observation.dispose();
}
{
  const f = fixture('probably', true, true);
  assert.equal(f.requested(), 1); assert.equal(f.callbackPending(), 1);
  const stale = f.peekCallback(), requests = f.requested();
  f.observation.dispose(); f.observation.dispose();
  assert.equal(f.cancelled(), 1); assert.equal(f.callbackPending(), 0); assert.equal(f.handlers.size, 0);
  stale(0, {mediaTime:1});
  assert.equal(f.requested(), requests); assert.equal(f.observation.sample().presentedCallbacks, 0);
}
{
  const f = fixture('probably', true, 'throws'); f.video.readyState = 4;
  const metrics = f.tick(4000, 4, 120);
  assert.equal(metrics.presentationSupported, true);
  assert.equal(metrics.presentationRequestFailed, true);
  assert.equal(metrics.progressSignal, 'presentation-callback');
  assert.equal(metrics.state, 'unverified');
  assert.equal(metrics.stateReason, 'presentation-callback-unavailable');
  assert.equal(f.tick(1000, 5, 150).state, 'unverified');
  assert.equal(f.requested(), 1); // No retries, hidden fallback or new owner.
  assert.equal(f.callbackPending(), 0);
  f.observation.dispose();
}
{
  const f = fixture('probably', false); f.video.readyState = 4;
  const unknown = f.tick(4000, 4);
  assert.equal(unknown.state, 'unverified');
  assert.equal(unknown.stateReason, 'frame-signal-unavailable');
  assert.equal(unknown.progressSignal, 'none');
  assert.equal(unknown.decodedFrames, null);
  assert.equal(unknown.clockProgressAgeMs, 0);
  assert.equal(f.tick(3999, 4).state, 'unverified');
  const frozen = f.tick(1, 4);
  assert.equal(frozen.state, 'stalled');
  assert.equal(frozen.stateReason, 'media-clock-nonprogress');
  assert.equal(frozen.clockProgressAgeMs, 4000);
}
{
  const f = fixture('probably', true, true); f.video.readyState = 4;
  const frozen = f.tick(4000);
  assert.equal(frozen.state, 'stalled');
  assert.equal(frozen.stateReason, 'media-clock-nonprogress');
  assert.equal(frozen.clockProgressAgeMs, 4000);
  assert.equal(frozen.selectedFrameProgressAgeMs, 4000);
  f.present(0);
  const callbackOnly = f.tick(1000);
  assert.equal(callbackOnly.state, 'stalled');
  assert.equal(callbackOnly.stateReason, 'media-clock-nonprogress');
  assert.equal(callbackOnly.selectedFrameProgressAgeMs, 0);
  assert.equal(callbackOnly.lastSelectedFrameProgressElapsedMs, 5000);
  assert.equal(callbackOnly.lastClockProgressElapsedMs, null);
  const paired = f.tick(1, 1);
  assert.equal(paired.state, 'playing');
  assert.equal(paired.stateReason, 'paired-progress');
  assert.equal(paired.lastProgressElapsedMs, 5001);
  // Re-reading metrics creates no additional progress evidence.
  assert.deepEqual(plain(f.observation.sample()), plain(paired));
  f.observation.dispose();
}
{
  const f = fixture('probably', true, true); f.video.readyState = 4;
  f.tick(1000, 1); // Clock advances first; callbacks remain unavailable for five seconds.
  f.present(1);
  const stalePair = f.tick(5000, 1);
  assert.equal(stalePair.state, 'stalled', 'a late callback cannot refresh a clock already frozen for five seconds');
  assert.equal(stalePair.stateReason, 'media-clock-nonprogress');
  assert.equal(stalePair.lastProgressElapsedMs, null);
  assert.equal(f.tick(1, 2).state, 'playing'); // Fresh clock plus callback evidence can still pair.
  f.observation.dispose();
}
{
  const f = fixture('probably', true, true); f.video.readyState = 4;
  f.video.requestVideoFrameCallback = () => { throw new Error('Re-arm failed'); };
  f.present(1);
  const paired = f.tick(1000, 1, 30);
  assert.equal(paired.state, 'playing'); // One genuine callback remains valid evidence.
  assert.equal(paired.presentationRequestFailed, true);
  const unknown = f.tick(4000, 5, 150);
  assert.equal(unknown.state, 'unverified');
  assert.equal(unknown.stateReason, 'presentation-callback-unavailable');
  assert.equal(unknown.progressSignal, 'presentation-callback');
  assert.equal(unknown.presentedCallbacks, 1);
  f.observation.dispose();
}
{
  // Exercise the actual pagehide wiring, with no browser, timers or network.
  const f = fixture('probably', true, true); f.observation.dispose();
  const nodes = {video:f.video, status:{}, play:{}, export:{}};
  let hide, cleared = 0;
  const page = vm.createContext({SapflowNativeHls:context.SapflowNativeHls, URLSearchParams,
    document:{getElementById:id => nodes[id]}, location:{search:'?event=event-native'},
    crypto:{randomUUID:() => 'page-instance'}, setInterval:() => 42,
    clearInterval:id => { assert.equal(id,42); cleared++; },
    window:{addEventListener:(event,callback,options) => {
      assert.equal(event,'pagehide'); assert.equal(options.once,true); hide=callback;
    }}});
  vm.runInContext(html.match(/<script>([\s\S]*?)<\/script>/)[1],page);
  assert.equal(f.callbackPending(),1);
  const stale=f.peekCallback(), requests=f.requested(), cancellations=f.cancelled();
  hide();
  assert.equal(cleared,1); assert.equal(f.callbackPending(),0);
  assert.equal(f.cancelled(),cancellations+1); assert.equal(f.handlers.size,0);
  stale(0,{mediaTime:9}); assert.equal(f.requested(),requests);
}
console.log('Native HLS fixture: owned presentation callbacks, quality fallback, one source, paired progress/unverified/clock stall/pause/error, rotation identity and bounded evidence passed');
