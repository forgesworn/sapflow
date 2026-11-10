'use strict';
// Orchestrates one W0 run. Usage:
//   node run.js capture udp|tcp --host <lan-ip> --out <dir> [--port 443]
//   node run.js capture udp|tcp --advertise <addr> [--udp-port N]   advertise an address that is not ours
//   node run.js loopback --out <dir> [--which ramp|opus|all]
//   node run.js capture|loopback ... --browser chrome|firefox|safari   (default chrome; safari uses synthetic media)
//   --ff-prefs '{"pref":value}'   extra Firefox prefs (JSON)
//   --query '&ramp=5'   extra query string for the page (non-Chrome browsers)
//   --range 47000-47199   keep every listener we open inside this port range; with --port for the TCP relay
//   node run.js obs udp|tcp --host <lan-ip> --out <dir> [--range ..] [--port N]   OBS WHIP output (see obs-run.js)
//   node run.js record-opus --out <dir> [--range ..]   20 s of stereo Opus from headless Chrome (A0.2 source)
//   node run.js play --assets <dir> --vendor <dir> --out <dir> [--browsers chrome,firefox,safari]   A0.2 playback matrix
//   node run.js ffmpeg udp [--dtls-active] --host <lan-ip> --out <dir>   (reference client, non-target)
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { start } = require('./fake-whip.js');
const { parseRange } = require('./ports.js');
const { launchFirefox, launchSafari, sleep } = require('./browser.js');

const CHROME = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const args = process.argv.slice(2);
const cmd = args[0];
const opt = (n, d) => { const i = args.indexOf('--' + n); return i > 0 ? args[i + 1] : d; };
const mode = ['udp', 'tcp'].includes(args[1]) ? args[1] : 'udp';
const browser = opt('browser', 'chrome');
const range = parseRange(opt('range'));
const out = path.resolve(opt('out', './w0-out'));
const timeoutMs = +(opt('timeout', cmd === 'loopback' ? 330 : cmd === 'record-opus' ? 60 : 40)) * 1000;

function trash(p) {
  const dest = path.join(os.homedir(), '.Trash', path.basename(p) + '-' + Date.now());
  try { fs.renameSync(p, dest); } catch (e) { console.error('could not move to Trash:', p, e.message); }
}

(async () => {
  if (cmd === 'play') { await require('./play-run.js').main({ opt, range, out }); process.exit(0); }
  if (cmd === 'obs') { await require('./obs-run.js').main({ opt, range, out, mode }); process.exit(0); }
  const ffActive = args.includes('--dtls-active');
  const label = cmd === 'loopback' ? 'loopback' : `${cmd === 'ffmpeg' ? 'ffmpeg' : browser}-${mode}${ffActive ? '-active' : ''}`;
  const srv = await start({ mode, host: opt('host'), advertise: opt('advertise'), udpPort: +opt('udp-port', 0), outDir: out, label, relayPort: +opt('port', 443), transportCc: false, range });
  console.log(`label=${label} http=${srv.httpPort} udp=${srv.udpPort} tcp-relay=${srv.tcpPort} load=${os.loadavg().map((x) => x.toFixed(1))}`);
  let child = null, profile = null, handle = null;
  if (cmd === 'ffmpeg') {
    const url = `http://127.0.0.1:${srv.httpPort}/whip?label=${label}`;
    child = spawn('ffmpeg', ['-hide_banner', '-loglevel', 'info', '-re', '-f', 'lavfi', '-i', 'testsrc2=size=1280x720:rate=30', '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000',
      '-c:v', 'libx264', '-profile:v', 'baseline', '-preset', 'veryfast', '-tune', 'zerolatency', '-g', '60', '-c:a', 'libopus', '-ac', '2', '-t', '20', ...(ffActive ? ['-whip_flags', 'dtls_active'] : []), '-f', 'whip', url], { stdio: ['ignore', 'ignore', fs.openSync(path.join(out, `${label}.ffmpeg.log`), 'w')] });
  } else if (browser !== 'chrome') {
    const q = (cmd === 'loopback' ? `loopback.html?which=${opt('which', 'all')}&xform=script${browser === 'safari' ? '&gesture=1' : ''}` : `capture.html?label=${label}${browser === 'safari' ? '&synthetic=1' : ''}`);
    const url = `http://127.0.0.1:${srv.httpPort}/${q}${opt('query', '')}`;
    try {
      handle = browser === 'firefox' ? launchFirefox(url, { extraPrefs: JSON.parse(opt('ff-prefs', '{}')) }) : await launchSafari(url, { range });
    } catch (e) { console.log('launch failed:', e.message, e.safariAutomationDisabled ? '(remote automation not enabled)' : ''); srv.ev.launchError = e.message; srv.close(); process.exit(2); }
    child = handle.child;
    if (browser === 'safari' && cmd === 'loopback') { await sleep(2500); srv.ev.safariClick = await handle.click('#go'); }
  } else {
    profile = fs.mkdtempSync(path.join(os.tmpdir(), 'w0-chrome-'));
    const page = cmd === 'loopback' ? `loopback.html?which=${opt('which', 'all')}` : cmd === 'record-opus' ? 'record-opus.html' : `capture.html?label=${label}`;
    child = spawn(CHROME, ['--headless=new', `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check',
      '--autoplay-policy=no-user-gesture-required', '--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream',
      '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows',
      ...(range ? [] : ['--remote-debugging-port=0']), `http://127.0.0.1:${srv.httpPort}/${page}`], { stdio: 'ignore', detached: true });
  }
  let t;
  const timedOut = new Promise((r) => { t = setTimeout(() => { console.log('timeout'); srv.ev.timedOut = true; r(); }, timeoutMs); });
  await Promise.race([srv.done, timedOut, new Promise((r) => (child.on('exit', r)))]);
  if (cmd === 'ffmpeg') await new Promise((r) => setTimeout(r, 1500));
  clearTimeout(t);
  srv.close();
  // Stop only what we started: our child's own process group, then wait for it.
  if (handle) { await handle.stop(); srv.ev.sessionEnded = handle.sessionEnded; console.log('events:', JSON.stringify(srv.ev)); process.exit(0); }
  try { process.kill(cmd === 'ffmpeg' ? child.pid : -child.pid, 'SIGTERM'); } catch {}
  await new Promise((r) => setTimeout(r, 2500));
  try { process.kill(cmd === 'ffmpeg' ? child.pid : -child.pid, 0); console.log('child group still alive, sending SIGKILL'); process.kill(cmd === 'ffmpeg' ? child.pid : -child.pid, 'SIGKILL'); } catch {}
  if (profile) trash(profile);
  console.log('events:', JSON.stringify(srv.ev));
  process.exit(0);
})();
