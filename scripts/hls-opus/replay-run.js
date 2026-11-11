'use strict';
// Plays sapflow's own Opus LL-HLS in headless Chrome and Firefox with hls.js,
// through the W0 page (scripts/whip-w0/pages/play.html: Web Audio analyser,
// decoded-byte count, click detector).
//
// Every playlist and media byte served is sapflow's, written by the
// `write_opus_evidence` test in sapflow-hls (opus_real_tests.rs). This harness
// adds only:
//   - live replay: each media playlist snapshot is served from the media time
//     it appeared, a blocking reload (_HLS_msn, _HLS_part) is held until a
//     snapshot satisfies it, and a part or segment is held until a playlist
//     lists it (so preload hints wait, as sapflow's server makes them wait);
//   - vod: the last snapshot with EXT-X-ENDLIST added, so playback starts at
//     the beginning of the stream, as W0's FFmpeg assets did. hls.js 1.7.4
//     does not move past the playlist's leading EXT-X-GAP entries from
//     position 0, so the page as served passes hls.js a startPosition at the
//     first real segment (the W0 file itself is unchanged);
//   - ps312: a copy whose init segment has the dOps pre-skip patched to 312,
//     for comparison with W0. Pre-skip 0 is what sapflow writes;
//   - the other CODECS spelling: a copy whose multivariant playlist says
//     "Opus" where sapflow says "opus", or the reverse, with a probe page
//     (/codecs.html) that records MediaSource.isTypeSupported and
//     canPlayType for both spellings.
//
// Usage (repository root):
//   node scripts/hls-opus/replay-run.js --evidence <write_opus_evidence out>
//     --vendor <dir with hls.min.js> --out <dir> --range 47100-47199
//     [--browsers chrome,firefox] [--runs 4] [--seconds 14] [--only <text>]
// FIREFOX and CHROME select the browser binaries, as in scripts/whip-w0.
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');

const W0 = path.join(__dirname, '..', 'whip-w0');
const { launchChrome, launchFirefox } = require(path.join(W0, 'browser.js'));
const ports = require(path.join(W0, 'ports.js'));

const args = process.argv.slice(2);
const opt = (n, d) => { const i = args.indexOf('--' + n); return i >= 0 ? args[i + 1] : d; };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const HOLD_MAX = 6000;

// One replayable stream directory: snapshots in order, and the media time at
// which each URI is first listed (as a part, segment or map, not a hint).
function load(dir) {
  const snaps = JSON.parse(fs.readFileSync(path.join(dir, 'snapshots.json'), 'utf8'));
  const available = new Map();
  for (const s of snaps) {
    s.text = fs.readFileSync(path.join(dir, s.file), 'utf8');
    for (const line of s.text.split('\n')) {
      if (line.startsWith('#EXT-X-PRELOAD-HINT')) continue;
      for (const m of line.matchAll(/URI="([^"]+)"/g)) if (!available.has(m[1])) available.set(m[1], s.ms);
      if (line && !line.startsWith('#') && !available.has(line)) available.set(line, s.ms);
    }
  }
  return { dir, snaps, available };
}

// The same stream with the multivariant playlist's Opus codec string
// respelt: "opus" becomes "Opus" and the reverse.
function respelt(evidence, out, name) {
  const src = path.join(evidence, name), dst = path.join(out, 'variants', `${name}-respelt`);
  fs.mkdirSync(path.dirname(dst), { recursive: true });
  fs.cpSync(src, dst, { recursive: true });
  const master = path.join(dst, 'master.m3u8'), text = fs.readFileSync(master, 'utf8');
  const swapped = text.replace(/CODECS="([^"]*)"/, (_, c) => `CODECS="${c.split(',').map((x) => (x === 'opus' ? 'Opus' : x === 'Opus' ? 'opus' : x)).join(',')}"`);
  if (swapped === text) throw new Error('no Opus codec string in ' + master);
  fs.writeFileSync(master, swapped);
  return load(dst);
}

const PROBE = `<!doctype html><meta charset="utf-8"><title>codecs probe</title><script>
const label = new URLSearchParams(location.search).get('label');
const types = ['audio/mp4; codecs="opus"', 'audio/mp4; codecs="Opus"', 'video/mp4; codecs="avc1.4d401f,opus"', 'video/mp4; codecs="avc1.4d401f,Opus"'];
const v = document.createElement('video');
const out = { userAgent: navigator.userAgent, mse: {}, canPlayType: {} };
for (const t of types) { out.mse[t] = window.MediaSource ? MediaSource.isTypeSupported(t) : null; out.canPlayType[t] = v.canPlayType(t); }
fetch('/result/' + label + '.json', { method: 'POST', body: JSON.stringify(out, null, 1) }).then(() => fetch('/done?label=' + label, { method: 'POST' }));
</script>`;

function variant(evidence, out, name, preskip) {
  const src = path.join(evidence, name);
  if (!preskip) return load(src);
  const dst = path.join(out, 'variants', `${name}-ps${preskip}`);
  fs.mkdirSync(path.dirname(dst), { recursive: true });
  fs.cpSync(src, dst, { recursive: true });
  const init = path.join(dst, 'live_init.mp4'), b = fs.readFileSync(init), i = b.indexOf('dOps');
  if (i < 0) throw new Error('no dOps box in ' + init);
  b.writeUInt16BE(preskip, i + 6); // size, 'dOps', version, channels, then pre-skip
  fs.writeFileSync(init, b);
  return load(dst);
}

// Seconds of EXT-X-GAP entries before the first real segment of the last
// snapshot: where the first segment starts on hls.js's timeline.
function leadingGaps(st) {
  let secs = 0, gap = false;
  for (const line of st.snaps[st.snaps.length - 1].text.split('\n')) {
    if (line === '#EXT-X-GAP') gap = true;
    else if (line.startsWith('#EXTINF:')) { if (!gap) break; secs += parseFloat(line.slice(8)); gap = false; }
  }
  return +secs.toFixed(6);
}

const TYPES = { '.m3u8': 'application/vnd.apple.mpegurl', '.mp4': 'video/mp4', '.html': 'text/html', '.js': 'text/javascript' };

async function main() {
  const evidence = path.resolve(opt('evidence'));
  const vendor = path.resolve(opt('vendor'));
  const out = path.resolve(opt('out'));
  const range = ports.parseRange(opt('range', '47100-47199'));
  const runs = +opt('runs', 4), seconds = +opt('seconds', 14), only = opt('only', '');
  const browsers = opt('browsers', 'chrome,firefox').split(',');
  fs.mkdirSync(out, { recursive: true });
  const streams = {
    'av-ps0': variant(evidence, out, 'live-av', 0),
    'audio-ps0': variant(evidence, out, 'live-audio', 0),
    'av-ps312': variant(evidence, out, 'live-av', 312),
    'av-respelt': respelt(evidence, out, 'live-av'),
    'audio-respelt': respelt(evidence, out, 'live-audio'),
  };
  const clocks = new Map(), waiters = new Map(), log = new Map();
  const note = (id, what) => { if (!log.has(id)) log.set(id, []); log.get(id).push(what); };

  const server = http.createServer(async (req, res) => {
    const url = new URL(req.url, 'http://x');
    if (req.method === 'POST') {
      const chunks = []; req.on('data', (c) => chunks.push(c));
      req.on('end', () => {
        if (url.pathname.startsWith('/result/')) fs.writeFileSync(path.join(out, path.basename(url.pathname)), Buffer.concat(chunks));
        if (url.pathname === '/done') { const l = url.searchParams.get('label'); (waiters.get(l) || (() => {}))(); }
        res.writeHead(204).end();
      });
      return;
    }
    const send = (status, body, type) => res.writeHead(status, { 'Content-Type': type || 'application/octet-stream', 'Cache-Control': 'no-store', 'Content-Length': Buffer.byteLength(body) }).end(body);
    if (url.pathname === '/play.html') return send(200, page, TYPES['.html']);
    if (url.pathname === '/codecs.html') return send(200, PROBE, TYPES['.html']);
    if (url.pathname === '/vendor/hls.min.js') return send(200, fs.readFileSync(path.join(vendor, 'hls.min.js')), TYPES['.js']);
    const m = /^\/run\/([\w-]+)\/([\w-]+)\/(live|vod)\/([\w.]+)$/.exec(url.pathname);
    if (!m || !streams[m[2]]) return send(404, '');
    const [, id, name, mode, file] = m, st = streams[name];
    const now = () => { const c = clocks.get(id); return c ? c.base + (Date.now() - c.t0) : st.snaps[0].ms; };
    if (file === 'stream.m3u8') {
      if (mode === 'vod') {
        const last = st.snaps[st.snaps.length - 1].text.split('\n').filter((l) => !l.startsWith('#EXT-X-PRELOAD-HINT')).join('\n');
        return send(200, last.replace(/\n*$/, '\n#EXT-X-ENDLIST\n'), TYPES['.m3u8']);
      }
      if (!clocks.has(id)) clocks.set(id, { t0: Date.now(), base: st.snaps[0].ms });
      const msn = url.searchParams.get('_HLS_msn'), part = url.searchParams.get('_HLS_part');
      const current = () => { let k = 0; while (k + 1 < st.snaps.length && st.snaps[k + 1].ms <= now()) k++; return k; };
      let k = current();
      if (msn !== null) {
        const M = +msn, P = part === null ? null : +part;
        let j = st.snaps.findIndex((s) => M < s.msn || (P !== null && M === s.msn && P < s.parts));
        if (j < 0) j = st.snaps.length - 1;
        const wait = st.snaps[j].ms - now();
        if (wait > 0) await sleep(Math.min(wait, HOLD_MAX));
        k = wait > HOLD_MAX ? current() : Math.max(current(), j);
        note(id, ['playlist', msn, part, Math.max(0, Math.round(wait)), st.snaps[k].file]);
      } else note(id, ['playlist', null, null, 0, st.snaps[k].file]);
      return send(200, st.snaps[k].text, TYPES['.m3u8']);
    }
    const p = path.join(st.dir, file);
    if (!fs.existsSync(p)) { note(id, ['404', file]); return send(404, ''); }
    const at = st.available.get(file);
    if (mode === 'live' && at !== undefined && at > now()) { const w = at - now(); note(id, ['held', file, Math.round(w)]); await sleep(Math.min(w, HOLD_MAX)); }
    note(id, ['file', file]);
    return send(200, fs.readFileSync(p), TYPES[path.extname(file)]);
  });
  const original = fs.readFileSync(path.join(W0, 'pages', 'play.html'), 'utf8');
  const page = original.replace('const hls = new Hls();', "const hls = new Hls(q.get('start') ? { startPosition: +q.get('start') } : {});");
  if (page === original) throw new Error('play.html no longer has the Hls constructor this patches');
  const port = await ports.listenTcp(server, '127.0.0.1', range);
  console.log('replay server on', port);

  const cases = [];
  for (const b of browsers) {
    const add = (stream, mode, n) => { for (let r = 1; r <= n; r++) cases.push({ b, stream, mode, r }); };
    if (b === 'chrome') { add('av-ps0', 'live', +opt('chrome-runs', 2)); add('audio-ps0', 'live', 1); add('av-ps0', 'vod', 1); }
    else { add('av-ps0', 'live', runs); add('av-ps0', 'vod', runs); add('av-ps312', 'live', runs); add('av-ps312', 'vod', runs); add('audio-ps0', 'live', 1); }
    add('av-respelt', 'live', 1); add('audio-respelt', 'live', 1);
  }
  const summary = [];
  for (const b of browsers) {
    const label = `${b}-codecs`;
    if (only && !label.includes(only)) continue;
    const done = new Promise((r) => { waiters.set(label, () => r(true)); setTimeout(() => r(false), 30000); });
    const h = b === 'chrome' ? launchChrome(`http://127.0.0.1:${port}/codecs.html?label=${label}`) : launchFirefox(`http://127.0.0.1:${port}/codecs.html?label=${label}`);
    const finished = await done;
    await h.stop();
    let r = null; try { r = JSON.parse(fs.readFileSync(path.join(out, label + '.json'), 'utf8')); } catch {}
    const row = { label, finished, ua: r && (r.userAgent.match(/(Firefox|Chrome)\/[\d.]+/) || [])[0], mse: r && r.mse, canPlayType: r && r.canPlayType };
    console.log(JSON.stringify(row));
    summary.push(row);
  }
  for (const c of cases) {
    const label = `${c.b}-${c.stream}-${c.mode}-${c.r}`;
    if (only && !label.includes(only)) continue;
    const id = label + '-' + Date.now();
    const src = `/run/${id}/${c.stream}/${c.mode}/master.m3u8`;
    const start = c.mode === 'vod' ? `&start=${leadingGaps(streams[c.stream])}` : '';
    const url = `http://127.0.0.1:${port}/play.html?src=${encodeURIComponent(src)}&player=hlsjs&label=${label}&seconds=${seconds}${start}`;
    const done = new Promise((r) => { waiters.set(label, () => r(true)); setTimeout(() => r(false), (seconds + 40) * 1000); });
    const load0 = os.loadavg()[0];
    const h = c.b === 'chrome' ? launchChrome(url) : launchFirefox(url);
    const finished = await done;
    await h.stop();
    let r = null; try { r = JSON.parse(fs.readFileSync(path.join(out, label + '.json'), 'utf8')); } catch {}
    const a = (r && r.audio) || {};
    const first = a.firstAudibleBlock;
    const reqs = log.get(id) || [];
    const row = {
      label, finished, load: +load0.toFixed(1), hlsjs: r && r.hlsjsVersion, ua: r && (r.userAgent.match(/(Firefox|Chrome)\/[\d.]+/) || [])[0],
      codecs: r && r.manifest && r.manifest.levels.map((l) => l.attrsCodecs), tStart: r && (r.samples.find((s) => s.t > 0) || {}).t, tEnd: r && r.currentTimeEnd, advanced: r && r.advancedSeconds,
      mediaError: r && r.mediaError, errors: r && r.errors, hlsErrors: r && r.hlsEvents.filter((e) => e[0] === 'hls-error').map((e) => `${e[3]}${e[4] ? ' (fatal)' : ''}`),
      audioBytes: r && r.audioDecodedBytesEnd, audible: a.audible, firstAudibleMs: a.firstAudibleMs, hz: [a.steadyLeftHz, a.steadyRightHz], rms: [a.steadyLeftRms, a.steadyRightRms],
      stepFirst: a.maxStepFirstBlocks, stepSteady: a.maxStepSteady, stepToneMax: a.expectedMaxStepRightTone,
      clicksAtStart: (a.clicks || []).filter((x) => first >= 0 && x[0] <= first + 3).map((x) => `${x[1]}ms:${x[2]}`),
      clicksLater: a.clickBlocksAfterFirstFour, silentAfterOnset: a.silentBlocksAfterOnset,
      requests: { files: reqs.filter((q) => q[0] === 'file').length, held: reqs.filter((q) => q[0] === 'held').length, blocking: reqs.filter((q) => q[0] === 'playlist' && q[1] !== null).length, missing: reqs.filter((q) => q[0] === '404').map((q) => q[1]) },
    };
    console.log(JSON.stringify(row));
    summary.push(row);
    fs.writeFileSync(path.join(out, label + '-requests.json'), JSON.stringify(reqs));
  }
  fs.writeFileSync(path.join(out, 'replay-summary.json'), JSON.stringify(summary, null, 1));
  server.close();
}

main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
