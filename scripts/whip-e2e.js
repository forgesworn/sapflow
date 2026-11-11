'use strict';
// A3.1 and A3.6 for WHIP, against the real sapflow binary on loopback.
// Dependency-free Node 24. Stops only what it started, by PID; never pkill.
//
//   cargo build -p sapflow [--target <triple>]
//   node scripts/whip-e2e.js --bin <sapflow> --vendor <dir with hls.min.js> --out <scratch dir> \
//     [--base 47300] [--seconds 10]
//
// Ports: base+1 HLS, base+2 SRT, base+3 WHIP signalling, base+4 WHIP media,
// base+5 this script's page server. CHROME picks the browser binary.
//
// 1. A3.6 default: sapflow without SAPFLOW_WHIP_ADDR binds neither WHIP
//    port (lsof on its PID), and SAPFLOW_WHIP_UDP_ADDR alone is refused at
//    start, binding nothing.
// 2. A3.1: headless Chrome with fake media, on a page from another origin
//    (so CORS is exercised), POSTs to sapflow, plays the stream in hls.js
//    and draws a decoded frame; then DELETEs the session. ffprobe and ffmpeg
//    then decode the recording: frame count, dimensions, a frame as PNG.
// 3. A3.6 privacy: sapflow ran with RUST_LOG=trace; its log must not hold
//    the token, any address, port or mDNS name from the offer, or the
//    publisher's address or port. The check is first shown to catch a
//    planted token.
//
// Everything a run writes (logs, the offer, the recording, frames) stays in
// --out, which is scratch, never the repository.
const { spawn, spawnSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');

const args = process.argv.slice(2);
const opt = (n, d) => { const i = args.indexOf('--' + n); return i >= 0 ? args[i + 1] : d; };
const bin = opt('bin'), vendor = opt('vendor'), out = opt('out');
const base = +opt('base', 47300), seconds = +opt('seconds', 10);
const CHROME = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const port = { hls: base + 1, srt: base + 2, whip: base + 3, media: base + 4, page: base + 5 };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, detail = '') => { results.push({ name, ok: !!ok, detail }); console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ': ' + detail : ''}`); };

if (!bin || !vendor || !out) { console.error('--bin, --vendor and --out are required'); process.exit(2); }
fs.mkdirSync(out, { recursive: true });
const TOKEN = 'whipe2e:' + crypto.randomBytes(12).toString('hex');
const EVENT = 'event-whip-e2e';

function trash(p) {
  if (!fs.existsSync(p)) return;
  const dest = path.join(os.homedir(), '.Trash', path.basename(p) + '-' + Date.now());
  try { fs.renameSync(p, dest); } catch (e) { console.error('could not move to the Trash:', p, e.message); }
}

async function stopGroup(pid) {
  try { process.kill(-pid, 'SIGTERM'); } catch {}
  for (let i = 0; i < 50; i++) { await sleep(100); try { process.kill(-pid, 0); } catch { return; } }
  try { process.kill(-pid, 'SIGKILL'); } catch {}
}

// Starts sapflow with `env`, logging to `log`; resolves once `ready` shows.
async function sapflow(env, log, ready) {
  const fd = fs.openSync(log, 'w');
  const child = spawn(bin, [], { env: { PATH: process.env.PATH, HOME: process.env.HOME, ...env }, stdio: ['ignore', fd, fd], detached: true });
  const exited = new Promise((r) => child.on('exit', (code) => r(code)));
  for (let i = 0; i < 100; i++) {
    await sleep(100);
    const text = fs.readFileSync(log, 'utf8');
    if (ready && text.includes(ready)) return { child, exited };
    if (child.exitCode !== null) break;
  }
  return { child, exited };
}

// The process's open internet sockets, as lsof prints them.
function sockets(pid) {
  const r = spawnSync('lsof', ['-nP', '-a', '-p', String(pid), '-i'], { encoding: 'utf8' });
  return r.stdout.split('\n').slice(1).filter(Boolean);
}
const bound = (lines, p) => lines.some((l) => l.includes(`:${p} `) || l.endsWith(`:${p}`) || l.includes(`:${p}->`) || l.includes(`:${p} (`));

const common = {
  SAPFLOW_HTTP_ADDR: `127.0.0.1:${port.hls}`,
  SAPFLOW_SRT_ADDR: `127.0.0.1:${port.srt}`,
  SAPFLOW_RTMP_ADDR: 'off',
  SAPFLOW_PUBLISH_USER: TOKEN.split(':')[0],
  SAPFLOW_PUBLISH_PASS: TOKEN.split(':')[1],
};

const PAGE = `<!doctype html><meta charset="utf-8"><title>WHIP A3.1</title>
<video id="v" muted playsinline width="640" height="360"></video><canvas id="c" width="640" height="360"></canvas>
<script src="/hls.min.js"></script>
<script>
const report = (o) => fetch('/report', { method: 'POST', body: JSON.stringify(o) });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
(async () => {
  const out = { errors: [], ua: navigator.userAgent };
  try {
    const media = await navigator.mediaDevices.getUserMedia({ audio: true, video: { width: 640, height: 360 } });
    const pc = new RTCPeerConnection();
    media.getTracks().forEach((t) => pc.addTransceiver(t, { direction: 'sendonly', streams: [media] }));
    await pc.setLocalDescription(await pc.createOffer());
    await Promise.race([new Promise((r) => { pc.onicegatheringstatechange = () => pc.iceGatheringState === 'complete' && r(); }), sleep(1500)]);
    out.offer = pc.localDescription.sdp;
    const r = await fetch('http://127.0.0.1:${port.whip}/whip/live/${EVENT}', {
      method: 'POST', headers: { 'Content-Type': 'application/sdp', Authorization: 'Bearer ${TOKEN}' }, body: out.offer });
    out.post = r.status; out.location = r.headers.get('Location');
    if (r.status !== 201) throw new Error('POST ' + r.status + ' ' + await r.text());
    await pc.setRemoteDescription({ type: 'answer', sdp: await r.text() });
    for (let i = 0; i < 100 && pc.connectionState !== 'connected'; i++) await sleep(100);
    out.connection = pc.connectionState;
    const stats = await pc.getStats();
    stats.forEach((s) => { if (s.type === 'candidate-pair' && s.nominated && s.state === 'succeeded') {
      const local = stats.get(s.localCandidateId); if (local) out.publisher = { address: local.address, port: local.port }; } });
    // The playlist exists once the first part is out, after the start hold.
    const playlist = 'http://127.0.0.1:${port.hls}/live/${EVENT}/index.m3u8';
    for (let i = 0; i < 150; i++) { const p = await fetch(playlist).catch(() => null); if (p && p.ok) break; await sleep(100); }
    const v = document.getElementById('v');
    const hls = new Hls({ lowLatencyMode: true });
    hls.on(Hls.Events.ERROR, (_, d) => { if (d.fatal) out.errors.push('hls ' + d.details); });
    hls.loadSource(playlist);
    hls.attachMedia(v);
    v.play().catch((e) => out.errors.push('play ' + e.message));
    // Playing: the playhead advances by ${seconds} s from where it started.
    let start = null;
    for (let i = 0; i < ${seconds * 10 + 150}; i++) {
      if (start === null && v.videoWidth > 0 && v.currentTime > 0) start = v.currentTime;
      if (start !== null && v.currentTime - start >= ${seconds}) break;
      await sleep(100);
    }
    out.played = start === null ? 0 : v.currentTime - start; out.width = v.videoWidth; out.height = v.videoHeight;
    const q = v.getVideoPlaybackQuality(); out.decoded = q.totalVideoFrames; out.dropped = q.droppedVideoFrames;
    const c = document.getElementById('c'); c.getContext('2d').drawImage(v, 0, 0, 640, 360);
    out.frame = c.toDataURL('image/png');
    const d = await fetch(out.location, { method: 'DELETE', headers: { Authorization: 'Bearer ${TOKEN}' } });
    out.delete = d.status;
    pc.close();
  } catch (e) { out.errors.push(String(e && e.message || e)); }
  out.done = true;
  await report(out);
})();
</script>`;

function pageServer(onReport) {
  const hlsjs = fs.readFileSync(path.join(vendor, 'hls.min.js'));
  const server = http.createServer((req, res) => {
    if (req.method === 'GET' && req.url === '/') { res.writeHead(200, { 'content-type': 'text/html' }); return res.end(PAGE); }
    if (req.method === 'GET' && req.url === '/hls.min.js') { res.writeHead(200, { 'content-type': 'text/javascript' }); return res.end(hlsjs); }
    if (req.method === 'POST' && req.url === '/report') {
      let body = ''; req.on('data', (d) => { body += d; }); req.on('end', () => { res.end('ok'); onReport(JSON.parse(body)); });
      return;
    }
    res.writeHead(404); res.end();
  });
  return new Promise((r) => server.listen(port.page, '127.0.0.1', () => r(server)));
}

function launchChrome(url) {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'whip-e2e-chrome-'));
  const flags = ['--headless=new', `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check',
    '--autoplay-policy=no-user-gesture-required', '--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream',
    '--allow-loopback-in-peer-connection', '--disable-background-timer-throttling', '--disable-renderer-backgrounding',
    '--disable-backgrounding-occluded-windows'];
  const child = spawn(CHROME, [...flags, url], { stdio: 'ignore', detached: true });
  return { child, profile };
}

// Every literal a privacy check looks for, from the offer and the publisher.
function secrets(offer, publisher) {
  const found = new Set([TOKEN, TOKEN.split(':')[1]]);
  for (const line of offer.split(/\r?\n/)) {
    const cand = /^a=candidate:\S+ \d+ \S+ \d+ (\S+) (\d+) typ (\S+)(?: raddr (\S+) rport (\d+))?/.exec(line);
    if (cand) {
      found.add(cand[1]); found.add(`${cand[1]}:${cand[2]}`); found.add(cand[2]);
      if (cand[4]) { found.add(cand[4]); if (cand[5] !== '0') found.add(cand[5]); }
    }
    const c = /^c=IN IP[46] (\S+)/.exec(line); if (c && c[1] !== '0.0.0.0') found.add(c[1]);
    const pwd = /^a=ice-pwd:(\S+)/.exec(line); if (pwd) found.add(pwd[1]);
  }
  if (publisher) { found.add(`${publisher.address}:${publisher.port}`); found.add(String(publisher.port)); if (publisher.address !== '127.0.0.1') found.add(publisher.address); }
  // The server's own loopback address appears in its listen lines; it is
  // the publisher's only with the publisher's port, checked above.
  found.delete('127.0.0.1');
  return [...found].filter((s) => s.length >= 4);
}
// Lines of `text` holding `s`; a number only as a whole word.
function hits(text, s) {
  const re = /^\d+$/.test(s) ? new RegExp(`(^|[^0-9])${s}([^0-9]|$)`) : null;
  return text.split('\n').filter((l) => (re ? re.test(l) : l.includes(s)));
}

(async () => {
  // 0. The panic-injection hook exists only in test builds.
  const marker = 'sapflow-whip test: panic here';
  check('the binary has no panic-injection hook', !fs.readFileSync(bin).includes(Buffer.from(marker)));

  // 1. A3.6 default: nothing bound without SAPFLOW_WHIP_ADDR.
  let s = await sapflow({ ...common }, path.join(out, 'off.log'), 'HLS listening');
  const off = sockets(s.child.pid);
  fs.writeFileSync(path.join(out, 'off-sockets.txt'), off.join('\n') + '\n');
  check('WHIP off: HLS and SRT bound', bound(off, port.hls) && bound(off, port.srt), `${off.length} sockets`);
  check('WHIP off: no WHIP signalling socket', !bound(off, port.whip));
  check('WHIP off: no WHIP media socket', !bound(off, port.media));
  await stopGroup(s.child.pid);
  s = await sapflow({ ...common, SAPFLOW_WHIP_UDP_ADDR: `127.0.0.1:${port.media}` }, path.join(out, 'udp-only.log'), null);
  const code = await Promise.race([s.exited, sleep(5000).then(() => 'running')]);
  const text = fs.readFileSync(path.join(out, 'udp-only.log'), 'utf8');
  check('SAPFLOW_WHIP_UDP_ADDR alone is refused at start', code === 2 && text.includes('needs SAPFLOW_WHIP_ADDR'), `exit ${code}`);
  if (code === 'running') await stopGroup(s.child.pid);

  // 2. A3.1.
  const recordings = path.join(out, 'recordings');
  fs.mkdirSync(recordings, { recursive: true });
  const log = path.join(out, 'whip.log');
  s = await sapflow({
    ...common,
    RUST_LOG: 'trace',
    SAPFLOW_RECORDINGS: recordings,
    SAPFLOW_RECORD_MIN_FREE_GB: '0',
    SAPFLOW_RECONNECT_GRACE: '2',
    SAPFLOW_WHIP_ADDR: `127.0.0.1:${port.whip}`,
    SAPFLOW_WHIP_UDP_ADDR: `127.0.0.1:${port.media}`,
    SAPFLOW_WHIP_CANDIDATES: `udp 127.0.0.1:${port.media}`,
    SAPFLOW_WHIP_PUBLIC_URL: `http://127.0.0.1:${port.whip}`,
    SAPFLOW_WHIP_ALLOW_ORIGINS: `http://127.0.0.1:${port.page}`,
  }, log, 'WHIP listening');
  const on = sockets(s.child.pid);
  fs.writeFileSync(path.join(out, 'on-sockets.txt'), on.join('\n') + '\n');
  check('WHIP on: signalling and media sockets bound', bound(on, port.whip) && bound(on, port.media));
  let report;
  const done = new Promise((r) => { report = r; });
  const page = await pageServer(report);
  const browser = launchChrome(`http://127.0.0.1:${port.page}/`);
  const r = await Promise.race([done, sleep((seconds + 60) * 1000).then(() => null)]);
  await stopGroup(browser.child.pid);
  page.close();
  trash(browser.profile);
  if (!r) { check('page reported', false, 'timeout'); }
  else {
    fs.writeFileSync(path.join(out, 'offer.sdp'), r.offer || '');
    if (r.frame) fs.writeFileSync(path.join(out, 'hls-frame.png'), Buffer.from(r.frame.split(',')[1], 'base64'));
    check('POST 201 with an absolute Location', r.post === 201 && /^http:\/\/127\.0\.0\.1:\d+\/whip\/session\/[0-9a-f]{32}$/.test(r.location || ''), `${r.post}`);
    check('ICE and DTLS connected', r.connection === 'connected', r.connection);
    check('hls.js played the stream', r.played >= seconds && r.width > 0 && r.decoded >= seconds * 10, `${(r.played || 0).toFixed(1)} s at ${r.width}x${r.height}, ${r.decoded} frames decoded, ${r.dropped} dropped`);
    check('DELETE 200', r.delete === 200, `${r.delete}`);
    check('no page errors', r.errors.length === 0, r.errors.join('; '));
  }
  // The stream is held for the 2 s grace after DELETE; then the recording ends.
  await sleep(4000);
  await stopGroup(s.child.pid);
  const files = fs.readdirSync(recordings, { recursive: true }).map((f) => path.join(recordings, f)).filter((f) => f.endsWith('.mp4'));
  check('a recording was written', files.length > 0, files.map((f) => path.basename(f)).join(', '));
  for (const f of files) {
    const probe = spawnSync('ffprobe', ['-v', 'error', '-count_frames', '-show_entries', 'stream=codec_name,width,height,nb_read_frames,sample_rate,channels', '-of', 'json', f], { encoding: 'utf8' });
    let streams = [];
    try { streams = JSON.parse(probe.stdout).streams || []; } catch {}
    fs.writeFileSync(path.join(out, path.basename(f) + '.probe.json'), probe.stdout);
    const video = streams.find((x) => x.codec_name === 'h264'), audio = streams.find((x) => x.codec_name === 'opus');
    check('recording: H.264 frames decode', video && +video.nb_read_frames > 0, video ? `${video.nb_read_frames} frames at ${video.width}x${video.height}` : probe.stderr.trim());
    check('recording: Opus decodes', audio && +audio.nb_read_frames > 0, audio ? `${audio.nb_read_frames} frames, ${audio.channels} channels at ${audio.sample_rate}` : '');
    const png = path.join(out, path.basename(f) + '.frame.png');
    const ff = spawnSync('ffmpeg', ['-v', 'error', '-y', '-i', f, '-frames:v', '1', png], { encoding: 'utf8' });
    check('recording: a frame decodes to PNG', ff.status === 0 && fs.existsSync(png) && fs.statSync(png).size > 0, ff.stderr.trim());
  }

  // 3. A3.6 privacy, with the check shown to work first.
  const logText = fs.readFileSync(log, 'utf8');
  const plain = logText.replace(/\x1b\[[0-9;]*m/g, '');
  check('the session ended, deleted, with nothing lost', /WHIP session ended reason="?deleted"? count=\d+ lost=0/.test(plain));
  check('the recording finished complete', /recording finished .*complete=true/.test(plain));
  const list = r ? secrets(r.offer || '', r.publisher) : [TOKEN];
  const planted = logText + `\nplanted ${TOKEN}\n`;
  check('privacy check catches a planted token', hits(planted, TOKEN).length === 1);
  check('trace log is non-trivial', logText.split('\n').length > 100 && logText.includes('TRACE'), `${logText.split('\n').length} lines`);
  const leaks = list.map((x) => ({ x, lines: hits(logText, x) })).filter((h) => h.lines.length);
  fs.writeFileSync(path.join(out, 'privacy.json'), JSON.stringify({ checked: list.length, leaks: leaks.map((l) => ({ lines: l.lines.slice(0, 5) })) }, null, 1));
  check('trace log holds no token, offer address, port or mDNS name, or publisher address', leaks.length === 0,
    `${list.length} literals checked (publisher seen: ${!!(r && r.publisher)})${leaks.length ? '; see privacy.json' : ''}`);

  fs.writeFileSync(path.join(out, 'results.json'), JSON.stringify(results, null, 1));
  const failed = results.filter((x) => !x.ok).length;
  console.log(failed ? `${failed} FAILED` : 'ALL PASSED');
  process.exit(failed ? 1 : 0);
})();
