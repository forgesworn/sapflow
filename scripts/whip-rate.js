'use strict';
// A3.10's control publisher: headless Chrome over WHIP to sapflow on
// loopback, to show that a stimulus from SAPFLOW_TEST_WHIP_FEEDBACK (a
// sapflow built with --features test-feedback) moves a sender that does
// respond, beside OBS (scripts/whip-obs.js). Dependency-free Node 24; needs
// Google Chrome, ffprobe. Stops only what it started, by PID.
//
//   node scripts/whip-rate.js --bin <sapflow> --out <dir> [--seconds 60]
//        [--feedback loss:<n>:<s>|remb:<bit/s>:<s>] [--base 47300]
//
// The page sends a 1280x720 canvas of moving noise at 30 fps and a WebAudio
// tone, with no bitrate cap of its own, and records once a second Chrome's
// own view: the video sender's target bitrate, bytes sent, and the
// transport's available outgoing bitrate. Results in --out: sapflow.log,
// the recording, page.json and result.json (the recording's video rate a
// second from ffprobe's packet sizes, beside the page's series).
const { spawn, spawnSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');

const args = process.argv.slice(2);
const opt = (n, d) => { const i = args.indexOf('--' + n); return i >= 0 ? args[i + 1] : d; };
const bin = opt('bin'), out = opt('out');
if (!bin || !out) { console.error('--bin and --out are required'); process.exit(2); }
const seconds = +opt('seconds', 60), base = +opt('base', 47300), feedback = opt('feedback');
const CHROME = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const port = { hls: base, whip: base + 1, media: base + 2, srt: base + 3, page: base + 4 };
const EVENT = 'event-rate';
const TOKEN = 'rate:' + crypto.randomBytes(12).toString('hex');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
fs.mkdirSync(out, { recursive: true });

const PAGE = `<!doctype html><meta charset="utf-8"><title>WHIP A3.10 control</title>
<script>
const report = (o) => fetch('/report', { method: 'POST', body: JSON.stringify(o) });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
(async () => {
  const out = { errors: [], series: [] };
  try {
    const c = document.createElement('canvas'); c.width = 1280; c.height = 720;
    const g = c.getContext('2d');
    const img = g.createImageData(1280, 720);
    let seed = 1;
    const draw = () => {
      // Fresh noise every frame, so the encoder can use any rate it is given.
      const d = img.data;
      for (let i = 0; i < d.length; i += 4) { seed = (seed * 1103515245 + 12345) >>> 0; const v = seed >>> 24; d[i] = v; d[i + 1] = v ^ 0x55; d[i + 2] = 255 - v; d[i + 3] = 255; }
      g.putImageData(img, 0, 0);
      requestAnimationFrame(draw);
    };
    draw();
    const ctx = new AudioContext({ sampleRate: 48000 }); await ctx.resume();
    const o = ctx.createOscillator(), dest = ctx.createMediaStreamDestination(); o.frequency.value = 440; o.connect(dest); o.start();
    const media = new MediaStream([...c.captureStream(30).getVideoTracks(), ...dest.stream.getAudioTracks()]);
    const pc = new RTCPeerConnection();
    let vsender = null;
    media.getTracks().forEach((t) => { const tr = pc.addTransceiver(t, { direction: 'sendonly', streams: [media] }); if (t.kind === 'video') vsender = tr.sender; });
    const p = vsender.getParameters(); p.degradationPreference = 'maintain-resolution';
    await vsender.setParameters(p).catch((e) => out.errors.push('setParameters ' + e.message));
    await pc.setLocalDescription(await pc.createOffer());
    await Promise.race([new Promise((r) => { pc.onicegatheringstatechange = () => pc.iceGatheringState === 'complete' && r(); }), sleep(1500)]);
    const r = await fetch('http://127.0.0.1:${port.whip}/whip/live/${EVENT}', {
      method: 'POST', headers: { 'Content-Type': 'application/sdp', Authorization: 'Bearer ${TOKEN}' }, body: pc.localDescription.sdp });
    out.post = r.status; const location = r.headers.get('Location');
    if (r.status !== 201) throw new Error('POST ' + r.status + ' ' + await r.text());
    const answer = await r.text();
    out.answer_remb = /goog-remb/.test(answer); out.answer_twcc = /transport-cc/.test(answer);
    await pc.setRemoteDescription({ type: 'answer', sdp: answer });
    for (let i = 0; i < 100 && pc.connectionState !== 'connected'; i++) await sleep(100);
    out.connection = pc.connectionState;
    const t0 = performance.now();
    let lastBytes = 0;
    while (performance.now() - t0 < ${seconds * 1000}) {
      await sleep(1000);
      const s = await vsender.getStats(); let row = { t: Math.round((performance.now() - t0) / 100) / 10 };
      s.forEach((x) => {
        if (x.type === 'outbound-rtp' && x.kind === 'video') { row.target_kbps = Math.round((x.targetBitrate || 0) / 1000); row.sent_kbps = Math.round(((x.bytesSent || 0) - lastBytes) * 8 / 1000); lastBytes = x.bytesSent || 0; row.width = x.frameWidth; row.fps = x.framesPerSecond; row.limit = x.qualityLimitationReason; }
        if (x.type === 'candidate-pair' && x.nominated && x.state === 'succeeded') row.available_kbps = Math.round((x.availableOutgoingBitrate || 0) / 1000);
      });
      out.series.push(row);
    }
    const d = await fetch(location, { method: 'DELETE', headers: { Authorization: 'Bearer ${TOKEN}' } });
    out.delete = d.status; pc.close();
  } catch (e) { out.errors.push(String(e && e.message || e)); }
  await report(out);
})();
</script>`;

function stopChild(child, ms = 4000) {
  return new Promise((r) => {
    if (!child || child.exitCode !== null || child.signalCode !== null) return r();
    child.once('exit', () => r());
    try { child.kill('SIGTERM'); } catch { return r(); }
    setTimeout(() => { try { child.kill('SIGKILL'); } catch {} }, ms);
  });
}

function videoKbps(f) {
  const p = spawnSync('ffprobe', ['-v', 'error', '-select_streams', 'v:0', '-show_entries', 'packet=pts_time,size', '-of', 'csv=p=0', f], { encoding: 'utf8' });
  const per = {};
  for (const l of p.stdout.trim().split('\n').filter(Boolean)) { const [t, size] = l.split(','); const s = Math.floor(+t); per[s] = (per[s] || 0) + (+size) * 8; }
  return Object.keys(per).sort((a, b) => a - b).map((s) => [+s, Math.round(per[s] / 1000)]);
}

(async () => {
  const env = {
    ...process.env,
    SAPFLOW_HTTP_ADDR: `127.0.0.1:${port.hls}`, SAPFLOW_SRT_ADDR: `127.0.0.1:${port.srt}`, SAPFLOW_RTMP_ADDR: 'off',
    SAPFLOW_PUBLISH_USER: TOKEN.split(':')[0], SAPFLOW_PUBLISH_PASS: TOKEN.split(':')[1],
    SAPFLOW_RECORDINGS: path.join(out, 'recordings'), SAPFLOW_RECORD_MIN_FREE_GB: '0', SAPFLOW_RECONNECT_GRACE: '2',
    SAPFLOW_WHIP_ADDR: `127.0.0.1:${port.whip}`, SAPFLOW_WHIP_UDP_ADDR: `127.0.0.1:${port.media}`,
    SAPFLOW_WHIP_CANDIDATES: `udp 127.0.0.1:${port.media}`, SAPFLOW_WHIP_PUBLIC_URL: `http://127.0.0.1:${port.whip}`,
    SAPFLOW_WHIP_ALLOW_ORIGINS: `http://127.0.0.1:${port.page}`, RUST_LOG: 'info,sapflow::whip=debug',
  };
  if (feedback) env.SAPFLOW_TEST_WHIP_FEEDBACK = feedback;
  const slog = fs.openSync(path.join(out, 'sapflow.log'), 'w');
  const srv = spawn(bin, [], { env, stdio: ['ignore', slog, slog] });
  let browser = null, profile = null;
  const info = { seconds, feedback: feedback || null, load: os.loadavg()[0] };
  try {
    await sleep(800);
    const page = await new Promise((resolve) => {
      const s = http.createServer((req, res) => {
        if (req.method === 'GET' && req.url === '/') { res.writeHead(200, { 'content-type': 'text/html' }); return res.end(PAGE); }
        if (req.method === 'POST' && req.url === '/report') { let b = ''; req.on('data', (d) => { b += d; }); req.on('end', () => { res.end('ok'); resolve({ s, o: JSON.parse(b) }); }); return; }
        res.writeHead(404); res.end();
      }).listen(port.page, '127.0.0.1');
      profile = fs.mkdtempSync(path.join(os.tmpdir(), 'whip-rate-chrome-'));
      const clog = fs.openSync(path.join(out, 'chrome.log'), 'w');
      browser = spawn(CHROME, ['--headless=new', `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check',
        '--autoplay-policy=no-user-gesture-required', '--allow-loopback-in-peer-connection', '--disable-background-timer-throttling',
        '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows', '--disable-features=MacAppCodeSignClone',
        `http://127.0.0.1:${port.page}/`], { stdio: ['ignore', clog, clog], detached: true });
      setTimeout(() => resolve({ s, o: null }), (seconds + 40) * 1000);
    });
    page.s.close();
    info.page = page.o;
    fs.writeFileSync(path.join(out, 'page.json'), JSON.stringify(page.o, null, 1));
  } catch (e) { info.error = String(e && e.message || e); }
  finally {
    if (browser) await stopChild(browser);
    if (profile) fs.rmSync(profile, { recursive: true, force: true });
    await sleep(4000);
    await stopChild(srv);
  }
  const recDir = path.join(out, 'recordings');
  const recs = fs.existsSync(recDir) ? fs.readdirSync(recDir, { recursive: true }).map((f) => path.join(recDir, f)).filter((f) => f.endsWith('.mp4')) : [];
  info.recordings = recs.map((f) => ({ file: path.basename(f), video_kbps: videoKbps(f) }));
  const text = fs.readFileSync(path.join(out, 'sapflow.log'), 'utf8');
  info.feedback_armed = /test feedback hooks armed/.test(text);
  fs.writeFileSync(path.join(out, 'result.json'), JSON.stringify(info, null, 1));
  const p = info.page || {};
  console.log(JSON.stringify({ feedback: info.feedback, armed: info.feedback_armed, load: info.load, post: p.post, answer_remb: p.answer_remb, answer_twcc: p.answer_twcc,
    errors: p.errors, series: (p.series || []).map((r) => `${r.t}:${r.sent_kbps}/${r.target_kbps}`).join(' ') }));
})();
