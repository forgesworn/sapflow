'use strict';
// A3.8 for WHIP: audio and video sync through sapflow against the EBU R37
// range (audio no more than 40 ms early or 60 ms late against the video).
// Dependency-free Node 24; needs ffmpeg 8 (its WHIP muxer for --publisher
// ffmpeg, and to measure) and, for --publisher chrome, Google Chrome. Stops
// only what it started, by PID.
//
//   node scripts/whip-sync.js --bin <sapflow> --out <dir> [--publisher chrome|ffmpeg]
//        [--transport udp|tcp] [--runs 10] [--seconds 20] [--base 47300]
//        [--source page|file] [--no-source]
//
// Each source has a white flash every 2 s from 1 s, on black, and a 10 ms
// 1 kHz click at the same instants.
//
// --publisher chrome: headless Chrome publishes over WHIP with the encoder
//   told to keep its resolution, at 640x360 and about 30 fps.
//   --source page (the default) makes the media in the page: a canvas and a
//   WebAudio click, both timed by the AudioContext's clock (see generated()
//   below). --source file captures the files below through Chrome's fake
//   devices (with --no-sandbox so they can be read; the browser loads only
//   this script's page, on loopback); in Chrome 157 headless on macOS its
//   video stalled after a few frames, and its two devices start
//   independently, so nothing aligns their files.
//   In the page, watchers (MediaStreamTrackProcessor) on clones of the
//   tracks give the source's own offset, on the page's clock (source).
// --publisher ffmpeg: FFmpeg's WHIP muxer sends the file source (640x360 at
//   30 fps, encoded once and stream-copied), so the timestamps it sends are
//   the file's: the offset left is what sapflow adds. UDP only (it has no
//   ICE-TCP).
//
// Each run measures the served HLS (each codec epoch's init and the segments
// the media playlist lists, fetched just before the publisher stops) and the
// recording: each flash's first frame against the onset of the nearest click
// (its first millisecond above -30 dBFS), within 500 ms. Offset is click
// minus flash: positive is audio late. Net is the served offset less the
// source's (medians). The server's log (sapflow::whip at debug) gives how the
// start was aligned. --out holds everything a run writes.
const { spawn, spawnSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');

const args = process.argv.slice(2);
const opt = (n, d) => { const i = args.indexOf('--' + n); return i >= 0 ? args[i + 1] : d; };
const bin = opt('bin'), out = opt('out');
const publisherKind = opt('publisher', 'chrome'), transport = opt('transport', 'udp');
const runs = +opt('runs', 10), seconds = +opt('seconds', 20), base = +opt('base', 47300);
// Leaves out the page's source measurement (MediaStreamTrackProcessor).
const noSource = args.includes('--no-source');
// Chrome's media: a canvas and WebAudio in the page (page), or the test
// files through its fake devices (file). Chrome 157's file capture stalled
// after a few frames headless on macOS (y4m and Motion-JPEG alike), so page
// is the default.
const sourceKind = opt('source', 'page');
if (!bin || !out) { console.error('--bin and --out are required'); process.exit(2); }
if (!['chrome', 'ffmpeg'].includes(publisherKind)) { console.error('--publisher chrome|ffmpeg'); process.exit(2); }
if (!['udp', 'tcp'].includes(transport) || (publisherKind === 'ffmpeg' && transport !== 'udp')) {
  console.error('--transport udp|tcp (ffmpeg: udp only)'); process.exit(2);
}
const CHROME = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const port = { hls: base + 1, srt: base + 2, whip: base + 3, media: base + 4, page: base + 5, tcp: base + 6 };
const TOKEN = 'whipsync:' + crypto.randomBytes(12).toString('hex');
const EARLY = -40, LATE = 60; // ms, EBU R37
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
fs.mkdirSync(out, { recursive: true });

function ff(argv, what) {
  const r = spawnSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', ...argv], { encoding: 'utf8' });
  if (r.status !== 0) throw new Error(`${what}: ${r.stderr.slice(-400)}`);
}

// The source, once.
function source() {
  const y4m = path.join(out, 'src.y4m'), wav = path.join(out, 'src.wav'), mkv = path.join(out, 'src-whip.mkv');
  const mjpeg = path.join(out, 'src.mjpeg');
  if (!fs.existsSync(y4m)) {
    ff(['-f', 'lavfi', '-i', 'color=c=black:s=640x360:r=30:d=10',
      '-vf', "drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='between(mod(n\\,60)\\,30\\,31)'",
      '-pix_fmt', 'yuv420p', '-f', 'yuv4mpegpipe', y4m], 'video source');
  }
  if (!fs.existsSync(mjpeg)) ff(['-i', y4m, '-c:v', 'mjpeg', '-q:v', '3', '-f', 'mjpeg', mjpeg], 'mjpeg source');
  if (!fs.existsSync(wav)) {
    ff(['-f', 'lavfi', '-i', "aevalsrc='gte(t\\,1)*lt(mod(t-1\\,2)\\,0.01)*0.8*sin(2*PI*1000*t)':s=48000:d=10",
      '-ac', '2', '-c:a', 'pcm_s16le', wav], 'audio source');
  }
  if (!fs.existsSync(mkv)) {
    ff(['-i', y4m, '-i', wav, '-c:v', 'libx264', '-preset', 'veryfast', '-tune', 'zerolatency', '-profile:v', 'baseline',
      '-g', '30', '-bf', '0', '-b:v', '1500k', '-c:a', 'libopus', '-b:a', '128k', '-ar', '48000', '-ac', '2', mkv], 'whip source');
  }
  return { y4m, mjpeg, wav, mkv };
}

const PAGE = (event) => `<!doctype html><meta charset="utf-8"><title>WHIP A3.8</title>
<script>
const report = (o) => fetch('/report', { method: 'POST', body: JSON.stringify(o) });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// Capture timestamps at the source: each flash's first frame and each click's
// first loud sample, in microseconds on the tracks' own clock.
const seen = { frames: 0, audio: 0, vArrive: [], aArrive: [], first: {} };
async function watchVideo(track, found, until) {
  const reader = new MediaStreamTrackProcessor({ track }).readable.getReader();
  const c = new OffscreenCanvas(8, 8), g = c.getContext('2d', { willReadFrequently: true });
  let lit = false;
  while (performance.now() < until) {
    const { value: f, done } = await reader.read(); if (done) break;
    seen.frames++;
    if (!seen.first.video) seen.first.video = { timestamp: f.timestamp, arrival: performance.now() * 1e3 };
    g.drawImage(f, 0, 0, 8, 8);
    const d = g.getImageData(0, 0, 8, 8).data; let sum = 0;
    for (let i = 0; i < d.length; i += 4) sum += d[i];
    const now = sum / 64 > 128;
    if (now && !lit) { found.push(f.timestamp); seen.vArrive.push(performance.now()); }
    lit = now; f.close();
  }
  reader.cancel();
}
async function watchAudio(track, found, until) {
  const reader = new MediaStreamTrackProcessor({ track }).readable.getReader();
  let quiet = 0;
  while (performance.now() < until) {
    const { value: a, done } = await reader.read(); if (done) break;
    seen.audio++;
    if (!seen.first.audio) seen.first.audio = { timestamp: a.timestamp, arrival: performance.now() * 1e3 };
    const n = a.numberOfFrames, s = new Float32Array(n);
    a.copyTo(s, { planeIndex: 0, format: 'f32-planar' });
    for (let i = 0; i < n; i++) {
      if (Math.abs(s[i]) > 0.03) { if (quiet > a.sampleRate / 10) { found.push(a.timestamp + 1e6 * i / a.sampleRate); seen.aArrive.push(performance.now() - 1e3 * (n - i) / a.sampleRate); } quiet = 0; }
      else quiet++;
    }
    a.close();
  }
  reader.cancel();
}
// The test source made in the page: a click (10 ms of 1 kHz) every 2 s from
// 1 s on the AudioContext's clock, and the canvas white for 66 ms from the
// animation frame nearest each click by that same clock (currentTime, which
// is where rendering has reached, and so where the destination's track has
// reached), black otherwise. A frame goes to the track (requestFrame) on the
// animation frame at which the colour changes, and otherwise 33 ms after
// the last, so about 30 fps. Animation frames come every 16.7 ms, so a flash
// starts up to 8.3 ms either side of its click; the page records by how
// much on the audio clock (lateness), and the watchers below measure the
// source as a whole.
async function generated(out) {
  const c = document.createElement('canvas'); c.width = 640; c.height = 360;
  const g = c.getContext('2d');
  const ctx = new AudioContext({ sampleRate: 48000 });
  await ctx.resume();
  const dest = ctx.createMediaStreamDestination();
  dest.channelCount = 2;
  const first = ctx.currentTime + 1;
  for (let k = 0; k < ${Math.ceil(seconds / 2) + 2}; k++) {
    const o = ctx.createOscillator(), gain = ctx.createGain();
    o.frequency.value = 1000; gain.gain.value = 0.8;
    o.connect(gain).connect(dest);
    o.start(first + 2 * k); o.stop(first + 2 * k + 0.01);
  }
  const [track] = c.captureStream(0).getVideoTracks();
  out.lateness = [];
  const HALF = 1 / 120;
  let white = false, last = -1;
  const draw = () => {
    const now = ctx.currentTime, t = performance.now();
    const since = (now - first + HALF) % 2 - HALF;
    const on = now >= first - HALF && since < 0.066;
    if (on && !white) out.lateness.push(Math.round(since * 1e4) / 10);
    if (on !== white || t - last >= 33) {
      g.fillStyle = on ? '#fff' : '#000';
      g.fillRect(0, 0, 640, 360);
      track.requestFrame();
      last = t;
    }
    white = on;
    requestAnimationFrame(draw);
  };
  draw();
  out.audioContext = { baseLatency: ctx.baseLatency, outputLatency: ctx.outputLatency, sampleRate: ctx.sampleRate };
  return new MediaStream([track, ...dest.stream.getAudioTracks()]);
}
(async () => {
  const out = { errors: [], ua: navigator.userAgent, flashes: [], clicks: [] };
  const progress = setInterval(() => report({ progress: { frames: seen.frames, audio: seen.audio, flashes: out.flashes.length, clicks: out.clicks.length, errors: out.errors } }), 2000);
  try {
    const media = ${JSON.stringify(sourceKind)} === 'file'
      ? await navigator.mediaDevices.getUserMedia({
          video: { width: 640, height: 360, frameRate: 30 },
          audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false, channelCount: 2 } })
      : await generated(out);
    const until = performance.now() + ${seconds * 1000};
    const [vt] = media.getVideoTracks(), [at] = media.getAudioTracks();
    const watching = (!${noSource} && typeof MediaStreamTrackProcessor === 'function')
      ? Promise.all([watchVideo(vt.clone(), out.flashes, until), watchAudio(at.clone(), out.clicks, until)])
      : (out.errors.push('no MediaStreamTrackProcessor'), Promise.resolve());
    const pc = new RTCPeerConnection();
    let vsender = null;
    media.getTracks().forEach((t) => { const tr = pc.addTransceiver(t, { direction: 'sendonly', streams: [media] }); if (t.kind === 'video') vsender = tr.sender; });
    const p = vsender.getParameters(); p.degradationPreference = 'maintain-resolution';
    if (p.encodings && p.encodings[0]) p.encodings[0].maxBitrate = 1500000;
    await vsender.setParameters(p).catch((e) => out.errors.push('setParameters ' + e.message));
    await pc.setLocalDescription(await pc.createOffer());
    await Promise.race([new Promise((r) => { pc.onicegatheringstatechange = () => pc.iceGatheringState === 'complete' && r(); }), sleep(1500)]);
    const r = await fetch('http://127.0.0.1:${port.whip}/whip/live/${event}', {
      method: 'POST', headers: { 'Content-Type': 'application/sdp', Authorization: 'Bearer ${TOKEN}' }, body: pc.localDescription.sdp });
    out.post = r.status; out.location = r.headers.get('Location');
    if (r.status !== 201) throw new Error('POST ' + r.status + ' ' + await r.text());
    await pc.setRemoteDescription({ type: 'answer', sdp: await r.text() });
    for (let i = 0; i < 100 && pc.connectionState !== 'connected'; i++) await sleep(100);
    out.connection = pc.connectionState;
    const stats = await pc.getStats();
    stats.forEach((s) => { if (s.type === 'candidate-pair' && s.nominated && s.state === 'succeeded') {
      const l = stats.get(s.localCandidateId); out.protocol = l && l.protocol; } });
    await report({ live: true });
    await watching;
    await sleep(Math.max(0, until - performance.now()));
    const d = await fetch(out.location, { method: 'DELETE', headers: { Authorization: 'Bearer ${TOKEN}' } });
    out.delete = d.status; pc.close();
  } catch (e) { out.errors.push(String(e && e.message || e)); }
  clearInterval(progress);
  out.done = true; out.vArrive = seen.vArrive; out.aArrive = seen.aArrive; out.first = seen.first;
  await report(out);
})();
</script>`;

function sapflow(dir) {
  const env = {
    ...process.env,
    SAPFLOW_HTTP_ADDR: `127.0.0.1:${port.hls}`, SAPFLOW_SRT_ADDR: `127.0.0.1:${port.srt}`, SAPFLOW_RTMP_ADDR: 'off',
    SAPFLOW_PUBLISH_USER: TOKEN.split(':')[0], SAPFLOW_PUBLISH_PASS: TOKEN.split(':')[1],
    SAPFLOW_RECORDINGS: path.join(dir, 'recordings'), SAPFLOW_RECORD_MIN_FREE_GB: '0', SAPFLOW_RECONNECT_GRACE: '2',
    SAPFLOW_WHIP_ADDR: `127.0.0.1:${port.whip}`, SAPFLOW_WHIP_UDP_ADDR: `127.0.0.1:${port.media}`,
    SAPFLOW_WHIP_PUBLIC_URL: `http://127.0.0.1:${port.whip}`, SAPFLOW_WHIP_ALLOW_ORIGINS: `http://127.0.0.1:${port.page}`,
    ...(transport === 'udp'
      ? { SAPFLOW_WHIP_CANDIDATES: `udp 127.0.0.1:${port.media}` }
      : { SAPFLOW_WHIP_CANDIDATES: `tcp 127.0.0.1:${port.tcp}`, SAPFLOW_WHIP_TCP_ADDR: `127.0.0.1:${port.tcp}` }),
    RUST_LOG: process.env.SYNC_RUST_LOG || 'info,sapflow::whip=debug',
  };
  const log = fs.openSync(path.join(dir, 'sapflow.log'), 'w');
  return spawn(bin, [], { env, stdio: ['ignore', log, log] });
}

function chrome(url, src, dir) {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'whip-sync-chrome-'));
  const flags = ['--headless=new', `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check',
    '--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream',
    ...(sourceKind === 'file' ? [
      `--use-file-for-fake-video-capture=${process.env.SYNC_VIDEO === 'y4m' ? src.y4m : src.mjpeg}`,
      `--use-file-for-fake-audio-capture=${src.wav}`,
      // The fake devices read their files from sandboxed processes, which
      // may not open them; this browser loads only this script's own page.
      '--no-sandbox'] : []),
    '--autoplay-policy=no-user-gesture-required', '--allow-loopback-in-peer-connection', '--disable-background-timer-throttling', '--disable-renderer-backgrounding',
    '--disable-backgrounding-occluded-windows', '--disable-features=MacAppCodeSignClone'];
  const log = fs.openSync(path.join(dir, 'chrome.log'), 'w');
  return { child: spawn(CHROME, [...flags, '--enable-logging=stderr', '--v=0', url], { stdio: ['ignore', log, log], detached: true }), profile };
}

function stop(child) {
  return new Promise((r) => {
    if (!child || child.exitCode !== null || child.signalCode !== null) return r();
    child.once('exit', () => r());
    try { child.kill('SIGTERM'); } catch { return r(); }
    setTimeout(() => { try { child.kill('SIGKILL'); } catch {} }, 4000);
  });
}

async function get(url) {
  const r = await fetch(url).catch(() => null);
  return r && r.ok ? Buffer.from(await r.arrayBuffer()) : null;
}

// The media playlist's segments, grouped by codec epoch, each written as its
// init followed by its segments.
async function fetchHls(event, dir) {
  const baseUrl = `http://127.0.0.1:${port.hls}/live/${event}/`;
  const text = (await get(baseUrl + 'stream.m3u8') || '').toString();
  const groups = [];
  let gap = false;
  for (const line of text.split('\n')) {
    const map = /^#EXT-X-MAP:URI="([^"]+)"/.exec(line);
    if (map) { groups.push({ init: map[1], segments: [] }); continue; }
    if (line.startsWith('#EXT-X-GAP')) { gap = true; continue; }
    if (line && !line.startsWith('#') && groups.length) { if (!gap) groups[groups.length - 1].segments.push(line); gap = false; }
  }
  const files = [];
  for (const [i, g] of groups.entries()) {
    if (!g.segments.length) continue;
    const parts = [await get(baseUrl + g.init)];
    for (const s of g.segments) parts.push(await get(baseUrl + s));
    if (parts.some((p) => !p)) continue;
    const f = path.join(dir, `hls-epoch${i}.mp4`);
    fs.writeFileSync(f, Buffer.concat(parts));
    files.push(f);
  }
  return files;
}

// Flash and click times (seconds, the file's own timeline) in one file.
function events(file) {
  const vlog = file + '.video.txt', alog = file + '.audio.txt';
  spawnSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', '-i', file, '-map', '0:v:0',
    '-vf', `signalstats,metadata=mode=print:key=lavfi.signalstats.YAVG:file=${vlog}`, '-f', 'null', '-'], { encoding: 'utf8' });
  spawnSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', '-i', file, '-map', '0:a:0',
    '-af', `aresample=48000,asetnsamples=n=48:p=0,astats=metadata=1:reset=1:measure_overall=Peak_level:measure_perchannel=none,ametadata=mode=print:key=lavfi.astats.Overall.Peak_level:file=${alog}`,
    '-f', 'null', '-'], { encoding: 'utf8' });
  const series = (f, key) => {
    const rows = []; let t = null;
    if (!fs.existsSync(f)) return rows;
    for (const line of fs.readFileSync(f, 'utf8').split('\n')) {
      const m = /pts_time:(-?[\d.]+)/.exec(line); if (m) { t = +m[1]; continue; }
      const v = new RegExp(key.replace(/\./g, '\\.') + '=(-?[\\d.]+|-inf)').exec(line);
      if (v && t !== null) rows.push([t, v[1] === '-inf' ? -200 : +v[1]]);
    }
    return rows;
  };
  const flashes = [], clicks = [];
  let lit = false;
  for (const [t, y] of series(vlog, 'lavfi.signalstats.YAVG')) { const on = y > 100; if (on && !lit) flashes.push(t); lit = on; }
  let quiet = 0;
  for (const [t, db] of series(alog, 'lavfi.astats.Overall.Peak_level')) {
    if (db > -30) { if (quiet >= 100) clicks.push(t); quiet = 0; } else quiet++;
  }
  return { flashes, clicks };
}

// Click minus flash, in milliseconds, for each flash with a click within
// 500 ms.
function offsets({ flashes, clicks }) {
  const o = [];
  for (const f of flashes) {
    let best = null;
    for (const c of clicks) if (Math.abs(c - f) < 0.5 && (best === null || Math.abs(c - f) < Math.abs(best - f))) best = c;
    if (best !== null) o.push(Math.round((best - f) * 1000 * 10) / 10);
  }
  return o;
}

const stats = (xs) => xs.length ? { n: xs.length, min: Math.min(...xs), max: Math.max(...xs),
  median: [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)] } : { n: 0 };

async function oneRun(n, src) {
  const dir = path.join(out, `${publisherKind}-${transport}-${n}`);
  fs.rmSync(dir, { recursive: true, force: true }); fs.mkdirSync(dir, { recursive: true });
  const event = `event-sync-${n}`;
  const s = sapflow(dir);
  const run = { run: n, publisher: publisherKind, transport, load: +os.loadavg()[0].toFixed(2) };
  let page = null, browser = null, ffw = null, report = null, live = null;
  try {
    await sleep(800);
    const started = Date.now();
    if (publisherKind === 'ffmpeg') {
      ffw = spawn('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-re', '-stream_loop', '-1', '-i', src.mkv,
        '-t', String(seconds), '-c', 'copy', '-f', 'whip', '-whip_flags', 'dtls_active', '-authorization', TOKEN,
        `http://127.0.0.1:${port.whip}/whip/live/${event}`], { stdio: ['ignore', 'ignore', fs.openSync(path.join(dir, 'ffmpeg.log'), 'w')] });
      live = Promise.resolve();
    } else {
      let liveResolve; live = new Promise((r) => { liveResolve = r; });
      report = new Promise((resolve) => {
        page = http.createServer((req, res) => {
          if (req.method === 'GET' && req.url === '/') { res.writeHead(200, { 'content-type': 'text/html' }); return res.end(PAGE(event)); }
          if (req.method === 'POST' && req.url === '/report') {
            let body = ''; req.on('data', (d) => { body += d; });
            req.on('end', () => {
              res.end('ok'); const o = JSON.parse(body);
              if (o.progress) fs.appendFileSync(path.join(dir, 'page-progress.log'), JSON.stringify(o.progress) + '\n');
              else if (o.live) liveResolve(); else resolve(o);
            });
            return;
          }
          res.writeHead(404); res.end();
        }).listen(port.page, '127.0.0.1');
      });
      browser = chrome(`http://127.0.0.1:${port.page}/`, src, dir);
    }
    await Promise.race([live, sleep(15000)]);
    // The served HLS, just before the publisher stops.
    await sleep(Math.max(0, seconds * 1000 - (Date.now() - started) - 2500));
    const hls = await fetchHls(event, dir);
    if (publisherKind === 'ffmpeg') await new Promise((r) => ffw.once('exit', r));
    else { const r = await Promise.race([report, sleep(20000).then(() => null)]); run.page = r; }
    // The recording finishes when the 2 s grace is over.
    await sleep(4000);
    const alog = fs.readFileSync(path.join(dir, 'sapflow.log'), 'utf8');
    const al = /WHIP timeline aligned alignment=(\S+)/.exec(alog);
    run.alignment = al ? al[1] : null;
    const recDir = path.join(dir, 'recordings');
    const recs = fs.existsSync(recDir)
      ? fs.readdirSync(recDir, { recursive: true }).map((f) => path.join(recDir, f)).filter((f) => f.endsWith('.mp4')) : [];
    run.hls = hls.map((f) => ({ file: path.basename(f), ...((e) => ({ flashes: e.flashes.length, clicks: e.clicks.length, offsets: offsets(e) }))(events(f)) }));
    run.recordings = recs.map((f) => ({ file: path.basename(f), ...((e) => ({ flashes: e.flashes.length, clicks: e.clicks.length, offsets: offsets(e) }))(events(f)) }));
    if (run.page && run.page.flashes) {
      // Chrome at its source: click minus flash on the page's clock, as each
      // reached the watchers (a click less the samples after it in its
      // chunk). Not on the tracks' own timestamps: in Chrome 157 these have
      // different origins (the canvas track's starts at 0 at its first
      // frame; the audio track's is near the page clock), so they do not
      // compare. The first of each is kept in the results (first).
      run.page_protocol = run.page.protocol; run.page_errors = run.page.errors;
      run.source_arrival = offsets({ flashes: run.page.vArrive.map((t) => t / 1e3), clicks: run.page.aArrive.map((t) => t / 1e3) });
      run.audio_context = run.page.audioContext;
      run.lateness = run.page.lateness;
      // Each track's first timestamp and when it reached the page: whether
      // the two tracks' timestamps share an origin.
      run.first = run.page.first;
      delete run.page;
    }
  } catch (e) { run.error = String(e && e.message || e); }
  finally {
    if (browser) { await stop(browser.child); fs.rmSync(browser.profile, { recursive: true, force: true }); }
    if (page) page.close();
    if (ffw) await stop(ffw);
    await stop(s);
  }
  const all = [...(run.hls || []).flatMap((h) => h.offsets), ...(run.recordings || []).flatMap((h) => h.offsets)];
  run.within = all.length > 0 && all.every((o) => o >= EARLY && o <= LATE);
  return run;
}

(async () => {
  const src = source();
  const results = [];
  for (let n = 1; n <= runs; n++) {
    const r = await oneRun(n, src);
    const hls = (r.hls || []).flatMap((h) => h.offsets), rec = (r.recordings || []).flatMap((h) => h.offsets);
    // Net: the served offset less the source's own (both medians), so what
    // the publisher's sending and the server add between them.
    if (r.source_arrival && r.source_arrival.length && hls.length) r.net = Math.round((stats(hls).median - stats(r.source_arrival).median) * 10) / 10;
    results.push(r);
    console.log(JSON.stringify({ run: n, publisher: publisherKind, transport, load: r.load, alignment: r.alignment,
      hls: stats(hls), recording: stats(rec), source: r.source_arrival ? stats(r.source_arrival) : undefined, net: r.net,
      lateness: r.lateness ? stats(r.lateness) : undefined,
      first: n === 1 ? r.first : undefined, within: r.within, error: r.error, page_errors: r.page_errors }));
  }
  fs.writeFileSync(path.join(out, `${publisherKind}-${transport}.json`), JSON.stringify(results, null, 1));
  const all = (k) => results.flatMap((r) => (r[k] || []).flatMap((h) => h.offsets));
  console.log('SUMMARY', JSON.stringify({ publisher: publisherKind, transport, runs: results.length,
    within: results.filter((r) => r.within).length, hls: stats(all('hls')), recording: stats(all('recordings')),
    source: stats(results.flatMap((r) => r.source_arrival || [])), net: stats(results.filter((r) => r.net !== undefined).map((r) => r.net)),
    alignments: [...new Set(results.map((r) => r.alignment))] }));
})();
