'use strict';
// A3.1 and A3.6 for WHIP, against the real sapflow binary on loopback.
// Dependency-free Node 24. Stops only what it started, by PID; never pkill.
//
//   cargo build -p sapflow [--target <triple>]
//   node scripts/whip-e2e.js --bin <sapflow> --vendor <dir with hls.min.js> --out <scratch dir> \
//     [--base 47300] [--seconds 10] [--transport udp|tcp|proxy] [--resize | --resize-loop]
//
// Ports: base+1 HLS, base+2 SRT, base+3 WHIP signalling, base+4 WHIP media,
// base+5 this script's page server, base+6 the ICE-TCP listener, base+7 the
// PROXY relay. CHROME picks the browser binary.
//
// --transport picks the only candidate the answer advertises, so the
// browser has one way in:
//   udp    the media UDP socket (the default);
//   tcp    the ICE-TCP listener on 127.0.0.1 (passive, RFC 6544), so the
//          browser must use ICE-TCP;
//   proxy  a layer-4 relay on 127.0.0.1 that prefixes each connection with a
//          PROXY protocol v2 header and forwards it from ::1 to the ICE-TCP
//          listener on [::1], which believes headers only from ::1
//          (SAPFLOW_WHIP_TCP_PROXY_PROTOCOL=v2, SAPFLOW_WHIP_TRUSTED_PROXY=::1).
//          The browser's POST still comes from 127.0.0.1, so it is not taken
//          as the proxy's.
// The nominated pair's protocol, from the browser's statistics, must match.
// Ports are 1024 or above: Chrome 157 starts no ICE-TCP to a private address
// below that (W0).
//
// 1. A3.6 default: sapflow without SAPFLOW_WHIP_ADDR binds neither WHIP
//    port (lsof on its PID), and SAPFLOW_WHIP_UDP_ADDR alone is refused at
//    start, binding nothing.
// 2. A3.1: headless Chrome with fake media, on a page from another origin
//    (so CORS is exercised), POSTs to sapflow, plays the stream in hls.js
//    and draws a decoded frame; then DELETEs the session. ffmpeg then decodes
//    every recording whole, which must give no error, and a frame as PNG.
//    --resize halves the sender's resolution (scaleResolutionDownBy) once
//    100 frames have played, as a browser under load does: playback must
//    carry on at the new size. Every codec epoch the server logs must have
//    its own recording, with or without --resize, since a browser may
//    ramp its resolution up by itself; the longest stall of the playhead
//    is reported.
//    --resize-loop toggles that halving every 5 s for 60 s, then holds
//    the picture until 80 s: more codec epochs than the recording's roll
//    budget (SAPFLOW_RECORDING_ROLL_*, a burst of 8 by default). Every
//    epoch must still be served and play; the recording rolls for at most
//    the first file and the burst, then stops (that file marked
//    epoch-rate) and resumes once the held picture has lasted the settle
//    time (that file marked too).
// 3. A3.6 privacy: sapflow ran with RUST_LOG=trace; its log must not hold
//    the token, any address, port or mDNS name from the offer, or the
//    publisher's address or port. The check is first shown to catch a
//    planted token. Over TCP the publisher's real source port is not in the
//    offer (active candidates say 9): it is taken from sapflow's established
//    connections (lsof) or from the relay, and the run fails without it.
//
// Everything a run writes (logs, the offer, the recording, frames) stays in
// --out, which is scratch, never the repository.
const { spawn, spawnSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');

const args = process.argv.slice(2);
const opt = (n, d) => { const i = args.indexOf('--' + n); return i >= 0 ? args[i + 1] : d; };
const bin = opt('bin'), vendor = opt('vendor'), out = opt('out');
const loop = args.includes('--resize-loop');
const base = +opt('base', 47300), seconds = +opt('seconds', loop ? 80 : 10);
const resize = args.includes('--resize');
// The server's default roll budget, which --resize-loop runs past.
const ROLL_BURST = 8;
const transport = opt('transport', 'udp');
if (!['udp', 'tcp', 'proxy'].includes(transport)) { console.error('--transport udp|tcp|proxy'); process.exit(2); }
const CHROME = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const port = { hls: base + 1, srt: base + 2, whip: base + 3, media: base + 4, page: base + 5, tcp: base + 6, relay: base + 7 };
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
    let sender = null;
    media.getTracks().forEach((t) => { const tr = pc.addTransceiver(t, { direction: 'sendonly', streams: [media] }); if (t.kind === 'video') sender = tr.sender; });
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
      const local = stats.get(s.localCandidateId), remote = stats.get(s.remoteCandidateId);
      if (local) out.publisher = { address: local.address, port: local.port };
      out.pair = { local: local && { protocol: local.protocol, tcpType: local.tcpType, type: local.candidateType },
        remote: remote && { protocol: remote.protocol, tcpType: remote.tcpType, port: remote.port } }; } });
    // The playlist exists once the first part is out, after the start hold.
    const playlist = 'http://127.0.0.1:${port.hls}/live/${EVENT}/index.m3u8';
    for (let i = 0; i < 150; i++) { const p = await fetch(playlist).catch(() => null); if (p && p.ok) break; await sleep(100); }
    const v = document.getElementById('v');
    const hls = new Hls({ lowLatencyMode: true });
    hls.on(Hls.Events.ERROR, (_, d) => { if (d.fatal) out.errors.push('hls ' + d.details); });
    hls.loadSource(playlist);
    hls.attachMedia(v);
    v.play().catch((e) => out.errors.push('play ' + e.message));
    // Playing: the playhead advances by ${seconds} s from where it started
    // (and, with --resize, past the change). The longest wait with the
    // playhead still is the stall.
    let start = null, still = null, last = -1;
    out.widths = []; out.stall = 0;
    for (let i = 0; i < ${seconds * 10 + 150}; i++) {
      if (start === null && v.videoWidth > 0 && v.currentTime > 0) start = v.currentTime;
      if (start !== null) {
        if (v.currentTime !== last) { last = v.currentTime; still = performance.now(); }
        else out.stall = Math.max(out.stall, performance.now() - still);
      }
      if (v.videoWidth && out.widths[out.widths.length - 1] !== v.videoWidth) out.widths.push(v.videoWidth);
      // Forced only from the full 640 wide, so Chrome's own ramp up cannot
      // pass for it: the playhead must then reach 320 and play on.
      if (${resize} && out.resizedAt === undefined && v.videoWidth === 640 && v.getVideoPlaybackQuality().totalVideoFrames > 100) {
        const p = sender.getParameters(); p.encodings[0].scaleResolutionDownBy = 2;
        await sender.setParameters(p).catch((e) => out.errors.push('setParameters ' + e.message));
        out.resizedAt = v.currentTime; out.resizeFrom = v.videoWidth;
      }
      // Toggled every 5 s from the first time at 640 wide, for 60 s.
      if (${loop} && v.videoWidth === 640 && out.loopFrom === undefined && v.getVideoPlaybackQuality().totalVideoFrames > 100) out.loopFrom = performance.now();
      if (${loop} && out.loopFrom !== undefined) {
        const n = Math.floor((performance.now() - out.loopFrom) / 5000);
        if (n < 12 && n !== out.toggle) {
          out.toggle = n;
          const p = sender.getParameters(); p.encodings[0].scaleResolutionDownBy = n % 2 ? 1 : 2;
          await sender.setParameters(p).catch((e) => out.errors.push('setParameters ' + e.message));
        }
      }
      if (out.resizeFrom && out.halfAt === undefined && v.videoWidth === out.resizeFrom / 2) out.halfAt = v.currentTime;
      if (out.halfAt !== undefined) out.afterHalf = v.currentTime - out.halfAt;
      if (start !== null && v.currentTime - start >= ${seconds} && (!${resize} || out.afterHalf >= 3)) break;
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
    '--disable-backgrounding-occluded-windows',
    // The only --disable-features flag (Chrome honours one): without it each
    // launch on macOS leaves a 1.4 GB code-sign clone of the app behind.
    '--disable-features=MacAppCodeSignClone'];
  const child = spawn(CHROME, [...flags, url], { stdio: 'ignore', detached: true });
  return { child, profile };
}

// A PROXY protocol v2 header (the specification's section 2.2) for a TCP
// connection over IPv4: the 12-byte signature, version 2 with the PROXY
// command (0x21), TCP over IPv4 (0x11), the address block's length (12),
// then source address, destination address, source port and destination
// port, all in network order.
function proxyV2(src, srcPort, dst, dstPort) {
  const v4 = (ip) => Buffer.from(ip.replace(/^::ffff:/, '').split('.').map(Number));
  const b = Buffer.alloc(28);
  Buffer.from([0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x0d, 0x0a, 0x51, 0x55, 0x49, 0x54, 0x0a]).copy(b, 0);
  b[12] = 0x21; b[13] = 0x11; b.writeUInt16BE(12, 14);
  v4(src).copy(b, 16); v4(dst).copy(b, 20);
  b.writeUInt16BE(srcPort, 24); b.writeUInt16BE(dstPort, 26);
  return b;
}

// A test-only layer-4 relay: each connection to 127.0.0.1:listen is opened
// again from ::1 to [::1]:target, the header sent first, then bytes copied
// both ways unchanged. `clients` collects each browser end it accepted.
function relay(listen, target, clients) {
  const sockets = new Set();
  const server = net.createServer((client) => {
    clients.push({ address: client.remoteAddress.replace(/^::ffff:/, ''), port: client.remotePort });
    const up = net.connect({ host: '::1', port: target, localAddress: '::1' }, () => {
      up.write(proxyV2(client.remoteAddress, client.remotePort, client.localAddress, client.localPort));
      client.pipe(up); up.pipe(client);
    });
    for (const s of [client, up]) { sockets.add(s); s.on('close', () => { sockets.delete(s); client.destroy(); up.destroy(); }); s.on('error', () => {}); }
  });
  server.stop = () => { for (const s of sockets) s.destroy(); server.close(); };
  return new Promise((r) => server.listen(listen, '127.0.0.1', () => r(server)));
}

// The far ends of sapflow's established connections to its ICE-TCP port.
function tcpPeers(pid, local) {
  const r = spawnSync('lsof', ['-nP', '-a', '-p', String(pid), '-iTCP', '-sTCP:ESTABLISHED'], { encoding: 'utf8' });
  const found = [];
  for (const l of r.stdout.split('\n')) {
    const m = new RegExp(`:${local}->\\[?([^\\]\\s]+?)\\]?:(\\d+) `).exec(l);
    if (m) found.push({ address: m[1], port: +m[2] });
  }
  return found;
}

// Every literal a privacy check looks for, from the offer and the publisher.
function secrets(offer, publisher, extra = []) {
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
  for (const p of [publisher, ...extra].filter(Boolean)) {
    found.add(`${p.address}:${p.port}`); found.add(String(p.port)); if (p.address !== '127.0.0.1') found.add(p.address);
  }
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
  // 0. The panic-injection hooks: the session's panic and armed reads
  // (sapflow-whip's test-panic feature) and the server's feed and ingest
  // payloads (its own test build). A build without test targets, as the
  // release recipe (cargo build --release -p sapflow) and this gate make,
  // contains none of them. One that includes the test targets
  // (--all-targets) contains sapflow-whip's, inert unless a test turns them
  // on (docs/whip-ingest.md, A3.4); this check then fails, as that is not a
  // build to run here.
  const image = fs.readFileSync(bin);
  const markers = ['sapflow-whip test: panic here', 'sapflow-whip test: panic in ', 'sapflow test: panic in the'];
  check('no panic-injection hook in a build without test targets', markers.every((m) => !image.includes(Buffer.from(m))));

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
    SAPFLOW_WHIP_PUBLIC_URL: `http://127.0.0.1:${port.whip}`,
    SAPFLOW_WHIP_ALLOW_ORIGINS: `http://127.0.0.1:${port.page}`,
    ...{
      udp: { SAPFLOW_WHIP_CANDIDATES: `udp 127.0.0.1:${port.media}` },
      tcp: { SAPFLOW_WHIP_CANDIDATES: `tcp 127.0.0.1:${port.tcp}`, SAPFLOW_WHIP_TCP_ADDR: `127.0.0.1:${port.tcp}` },
      proxy: {
        SAPFLOW_WHIP_CANDIDATES: `tcp 127.0.0.1:${port.relay}`, SAPFLOW_WHIP_TCP_ADDR: `[::1]:${port.tcp}`,
        SAPFLOW_WHIP_TCP_PROXY_PROTOCOL: 'v2', SAPFLOW_WHIP_TRUSTED_PROXY: '::1',
      },
    }[transport],
  }, log, transport === 'udp' ? 'WHIP listening' : 'WHIP ICE-TCP listening');
  const on = sockets(s.child.pid);
  fs.writeFileSync(path.join(out, 'on-sockets.txt'), on.join('\n') + '\n');
  check('WHIP on: signalling and media sockets bound', bound(on, port.whip) && bound(on, port.media));
  if (transport !== 'udp') check('WHIP on: the ICE-TCP listener is bound', bound(on, port.tcp));
  // Over TCP, the publisher's real ends: sapflow's peers (direct), or the
  // browser ends the relay accepted (proxy), whose addresses the PROXY
  // headers gave sapflow.
  const peers = [];
  const proxy = transport === 'proxy' ? await relay(port.relay, port.tcp, peers) : null;
  const watch = transport === 'tcp' ? setInterval(() => {
    for (const p of tcpPeers(s.child.pid, port.tcp)) if (!peers.some((q) => q.port === p.port)) peers.push(p);
  }, 250) : null;
  // Every init the media playlist served while the browser played: one
  // recording per served codec epoch is the rule.
  const maps = new Set();
  const served = setInterval(() => {
    http.get(`http://127.0.0.1:${port.hls}/live/${EVENT}/stream.m3u8`, (res) => {
      let body = ''; res.on('data', (d) => { body += d; });
      res.on('end', () => { for (const m of body.matchAll(/#EXT-X-MAP:URI="([^"]+)"/g)) maps.add(m[1]); });
    }).on('error', () => {});
  }, 250);
  let report;
  const done = new Promise((r) => { report = r; });
  const page = await pageServer(report);
  const browser = launchChrome(`http://127.0.0.1:${port.page}/`);
  const r = await Promise.race([done, sleep((seconds + 60) * 1000).then(() => null)]);
  clearInterval(served);
  if (watch) clearInterval(watch);
  await stopGroup(browser.child.pid);
  page.close();
  if (proxy) proxy.stop();
  trash(browser.profile);
  if (!r) { check('page reported', false, 'timeout'); }
  else {
    fs.writeFileSync(path.join(out, 'offer.sdp'), r.offer || '');
    if (r.frame) fs.writeFileSync(path.join(out, 'hls-frame.png'), Buffer.from(r.frame.split(',')[1], 'base64'));
    check('POST 201 with an absolute Location', r.post === 201 && /^http:\/\/127\.0\.0\.1:\d+\/whip\/session\/[0-9a-f]{32}$/.test(r.location || ''), `${r.post}`);
    check('ICE and DTLS connected', r.connection === 'connected', r.connection);
    const want = transport === 'udp' ? 'udp' : 'tcp';
    const pair = r.pair || {};
    check(`the nominated pair is ${want.toUpperCase()}`, pair.local && pair.local.protocol === want && pair.remote && pair.remote.protocol === want
      && (want === 'udp' || pair.remote.tcpType === 'passive'), JSON.stringify(pair));
    check('hls.js played the stream', r.played >= seconds && r.width > 0 && r.decoded >= seconds * 10, `${(r.played || 0).toFixed(1)} s at ${r.width}x${r.height}, ${r.decoded} frames decoded, ${r.dropped} dropped; widths ${(r.widths || []).join(' then ')}, longest stall ${((r.stall || 0) / 1000).toFixed(2)} s`);
    if (resize) {
      check('the forced change halved the width and playback carried on', r.resizeFrom === 640 && r.halfAt !== undefined && r.afterHalf >= 3,
        `forced at ${r.resizeFrom || 'never'} wide, ${r.halfAt === undefined ? 'never reached half' : `played ${r.afterHalf.toFixed(1)} s at half width`}; widths ${r.widths.join(' then ')}, longest stall ${(r.stall / 1000).toFixed(2)} s`);
    }
    check('DELETE 200', r.delete === 200, `${r.delete}`);
    check('no page errors', r.errors.length === 0, r.errors.join('; '));
  }
  // The stream is held for the 2 s grace after DELETE; then the recording ends.
  await sleep(4000);
  await stopGroup(s.child.pid);
  const files = fs.readdirSync(recordings, { recursive: true }).map((f) => path.join(recordings, f)).filter((f) => f.endsWith('.mp4'));
  check('a recording was written', files.length > 0, files.map((f) => path.basename(f)).join(', '));
  // One recording per codec epoch that was served: every init the playlist
  // named. A browser may also ramp its resolution up by itself at the
  // start. The server's log agrees: an epoch that ended with no part
  // (previous_parts=0, counted after the change sealed its open part) was
  // never in the playlist.
  const changes = fs.readFileSync(log, 'utf8').replace(/\x1b\[[0-9;]*m/g, '').split('\n')
    .filter((l) => l.includes('mid-stream; starting a new codec epoch'));
  const withParts = changes.filter((l) => !/previous_parts=0\b/.test(l)).length;
  if (loop) {
    // Past the budget: the first file and the burst, then the resumed one.
    check('every codec epoch served, the recording within its roll budget',
      maps.size > ROLL_BURST + 1 && files.length <= ROLL_BURST + 2 && files.length >= Math.min(maps.size, ROLL_BURST + 1),
      `${files.length} files, ${maps.size} inits served, ${changes.length} epoch changes; widths ${r ? (r.widths || []).join(' then ') : '-'}`);
  } else {
    check('one recording per served codec epoch', maps.size > 0 && files.length === maps.size && withParts + 1 === maps.size && (!resize || maps.size > 1),
      `${files.length} files, ${maps.size} inits served, ${changes.length} epoch changes (${changes.length - withParts} from an epoch with no part)`);
  }
  for (const f of files) {
    const probe = spawnSync('ffprobe', ['-v', 'error', '-count_frames', '-show_entries', 'stream=codec_name,width,height,nb_read_frames,sample_rate,channels,start_time,duration', '-of', 'json', f], { encoding: 'utf8' });
    let streams = [];
    try { streams = JSON.parse(probe.stdout).streams || []; } catch {}
    fs.writeFileSync(path.join(out, path.basename(f) + '.probe.json'), probe.stdout);
    const video = streams.find((x) => x.codec_name === 'h264'), audio = streams.find((x) => x.codec_name === 'opus');
    // The whole file, not a frame count: a file whose frames do not match
    // its init can still count frames. A file that lost whole fragments
    // still decodes cleanly, so this does not see missing media; the
    // server's completeness marks are what report that.
    // At the stream's own time base: ffmpeg's default guesses one from the
    // frame rate, and a file whose video starts after its audio (late audio
    // kept at a codec change) then has frames rounded onto each other,
    // which it reports though nothing fails to decode.
    const whole = spawnSync('ffmpeg', ['-hide_banner', '-v', 'error', '-i', f, '-enc_time_base', 'demux', '-f', 'null', '-'], { encoding: 'utf8' });
    const decodeErrors = whole.stderr.split('\n').filter(Boolean);
    fs.writeFileSync(path.join(out, path.basename(f) + '.decode.txt'), whole.stderr);
    check('recording: decodes whole with no error', whole.status === 0 && decodeErrors.length === 0 && video && +video.nb_read_frames > 0,
      video ? `${video.nb_read_frames} H.264 frames at ${video.width}x${video.height}, ${decodeErrors.length} decode errors; video from ${video.start_time} for ${video.duration} s, audio from ${audio ? audio.start_time : '-'} for ${audio ? audio.duration : '-'} s` : probe.stderr.trim());
    check('recording: Opus decodes', audio && +audio.nb_read_frames > 0, audio ? `${audio.nb_read_frames} frames, ${audio.channels} channels at ${audio.sample_rate}` : '');
    const png = path.join(out, path.basename(f) + '.frame.png');
    const ff = spawnSync('ffmpeg', ['-v', 'error', '-y', '-i', f, '-frames:v', '1', png], { encoding: 'utf8' });
    check('recording: a frame decodes to PNG', ff.status === 0 && fs.existsSync(png) && fs.statSync(png).size > 0, ff.stderr.trim());
  }

  // 3. A3.6 privacy, with the check shown to work first.
  const logText = fs.readFileSync(log, 'utf8');
  const plain = logText.replace(/\x1b\[[0-9;]*m/g, '');
  check('the session ended, deleted, with nothing lost', /WHIP session ended reason="?deleted"? count=\d+ lost=0/.test(plain));
  // Every file, not any one of them.
  const finished = plain.split('\n').filter((l) => l.includes('recording finished'));
  if (loop) {
    // A file keeps its first reason only, so the file that ran out can read
    // another; every incomplete one must have one, and one must be the
    // budget's.
    const rate = finished.filter((l) => /reason="?epoch-rate/.test(l)).length;
    check('every recording finished complete or marked, the budget\'s marks among them',
      finished.length === files.length && rate >= 1 && finished.every((l) => /complete=true/.test(l) || /reason="?[a-z]/.test(l)),
      `${finished.filter((l) => /complete=true/.test(l)).length} of ${finished.length} complete, ${rate} marked epoch-rate, ${files.length} files`);
    // The resume: the last file started, after the stretch the budget left
    // unrecorded, is marked epoch-rate and begins with a keyframe.
    const resumed = [...files].sort().pop();
    const line = resumed && finished.find((l) => l.includes(path.basename(resumed)));
    const first = resumed ? spawnSync('ffprobe', ['-v', 'error', '-select_streams', 'v:0', '-show_entries', 'packet=flags', '-of', 'csv=p=0', '-read_intervals', '%+#1', resumed], { encoding: 'utf8' }).stdout.trim() : '';
    check('recording resumed after the budget\'s gap, marked, on a keyframe',
      !!line && /reason="?epoch-rate/.test(line) && first.startsWith('K'),
      `${resumed ? path.basename(resumed) : 'no file'}: ${line ? (line.match(/reason="?[a-z-]+/) || ['complete'])[0] : 'no finish line'}, first packet ${first || 'none'}`);
  } else {
    check('every recording finished complete', finished.length === files.length && finished.every((l) => /complete=true/.test(l)),
      `${finished.filter((l) => /complete=true/.test(l)).length} of ${finished.length} complete, ${files.length} files`);
  }
  if (transport !== 'udp') {
    check('the publisher\'s real TCP ends were seen', peers.length > 0 && peers.every((p) => p.port >= 1024 && p.port !== port.tcp),
      `${peers.length} connection(s)`);
  }
  const list = r ? secrets(r.offer || '', r.publisher, peers) : [TOKEN];
  const planted = logText + `\nplanted ${TOKEN}\n`;
  check('privacy check catches a planted token', hits(planted, TOKEN).length === 1);
  // Trace logging was on: the runtime's own trace lines are there. A TCP run
  // logs fewer lines than a UDP one, so the count is a floor, not a target.
  const traces = logText.split('\n').filter((l) => l.includes('TRACE')).length;
  check('trace log is non-trivial', logText.split('\n').length > 50 && traces >= 10, `${logText.split('\n').length} lines, ${traces} at trace`);
  const leaks = list.map((x) => ({ x, lines: hits(logText, x) })).filter((h) => h.lines.length);
  fs.writeFileSync(path.join(out, 'privacy.json'), JSON.stringify({ checked: list.length, leaks: leaks.map((l) => ({ lines: l.lines.slice(0, 5) })) }, null, 1));
  check('trace log holds no token, offer address, port or mDNS name, or publisher address', leaks.length === 0,
    `${list.length} literals checked (publisher seen: ${!!(r && r.publisher)}, ${peers.length} TCP ends)${leaks.length ? '; see privacy.json' : ''}`);

  fs.writeFileSync(path.join(out, 'results.json'), JSON.stringify(results, null, 1));
  const failed = results.filter((x) => !x.ok).length;
  console.log(failed ? `${failed} FAILED` : 'ALL PASSED');
  process.exit(failed ? 1 : 0);
})();
