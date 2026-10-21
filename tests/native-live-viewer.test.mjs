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
function fixture(supported = 'maybe', frameCounter = true) {
  let now = 0, assignments = 0, decoded = 0;
  const handlers = new Map();
  const video = {currentTime:0, videoWidth:720, videoHeight:1280, readyState:1,
    networkState:2, paused:false, ended:false, error:null,
    buffered:{length:1, start:() => 0, end:() => 2}, seekable:{length:0},
    canPlayType:() => supported,
    addEventListener:(type, handler) => handlers.set(type, handler),
    removeEventListener:(type, handler) => { assert.equal(handlers.get(type), handler); handlers.delete(type); },
    set src(value) { assignments++; assert.equal(value, '/live/event-native/index.m3u8'); },
    load:() => assert.fail('fixture must not reload the native source')};
  if (frameCounter) video.getVideoPlaybackQuality = () => ({totalVideoFrames:decoded});
  const observation = new Observation(video, 'event-native', {now:() => now,
    wall:() => 'fixture-time', instanceId:'stable-instance'});
  return {video, handlers, observation, assignments:() => assignments,
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
  assert.equal(f.tick(4000, 3, 60).state, 'stalled'); // Clock-only progress remains insufficient.
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
  assert.equal(f.tick(1000, 1, 0).state, 'stalled'); // A clock jump with no decoded frames is insufficient.
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
  assert.equal(metrics.state, 'playing'); assert.equal(metrics.decodedFrames, null);
  assert.deepEqual(plain(metrics.buffered), [[0, 2]]);
  assert.equal(metrics.bufferedAheadSeconds, 1);
  f.tick(1000, 3); assert.equal(f.observation.sample().bufferedAheadSeconds, null);
}
console.log('Native HLS fixture: constrained event, one source, progress/stall/pause/error, rotation identity and bounded evidence passed');
