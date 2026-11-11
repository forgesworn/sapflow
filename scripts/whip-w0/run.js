'use strict';
// Orchestrates one W0 run. Usage:
//   node run.js capture udp|tcp --host <lan-ip> --out <dir> [--port 443]
//   node run.js capture udp|tcp --advertise <addr> [--udp-port N]   advertise an address that is not ours
//   node run.js loopback --out <dir> [--which ramp|opus|all]
//   node run.js ffmpeg udp [--dtls-active] --host <lan-ip> --out <dir>   (reference client, non-target)
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { start } = require('./fake-whip.js');

const CHROME = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const args = process.argv.slice(2);
const cmd = args[0];
const opt = (n, d) => { const i = args.indexOf('--' + n); return i > 0 ? args[i + 1] : d; };
const mode = ['udp', 'tcp'].includes(args[1]) ? args[1] : 'udp';
const out = path.resolve(opt('out', './w0-out'));
const timeoutMs = +(opt('timeout', cmd === 'loopback' ? 330 : 40)) * 1000;

function trash(p) {
  const dest = path.join(os.homedir(), '.Trash', path.basename(p) + '-' + Date.now());
  try { fs.renameSync(p, dest); } catch (e) { console.error('could not move to Trash:', p, e.message); }
}

(async () => {
  const ffActive = args.includes('--dtls-active');
  const label = cmd === 'loopback' ? 'loopback' : `${cmd === 'ffmpeg' ? 'ffmpeg' : 'chrome'}-${mode}${ffActive ? '-active' : ''}`;
  const srv = await start({ mode, host: opt('host'), advertise: opt('advertise'), udpPort: +opt('udp-port', 0), outDir: out, label, relayPort: +opt('port', 443), transportCc: false });
  console.log(`label=${label} http=${srv.httpPort} udp=${srv.udpPort} tcp-relay=${srv.tcpPort} load=${os.loadavg().map((x) => x.toFixed(1))}`);
  let child = null, profile = null;
  if (cmd === 'ffmpeg') {
    const url = `http://127.0.0.1:${srv.httpPort}/whip?label=${label}`;
    child = spawn('ffmpeg', ['-hide_banner', '-loglevel', 'info', '-re', '-f', 'lavfi', '-i', 'testsrc2=size=1280x720:rate=30', '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000',
      '-c:v', 'libx264', '-profile:v', 'baseline', '-preset', 'veryfast', '-tune', 'zerolatency', '-g', '60', '-c:a', 'libopus', '-ac', '2', '-t', '20', ...(ffActive ? ['-whip_flags', 'dtls_active'] : []), '-f', 'whip', url], { stdio: ['ignore', 'ignore', fs.openSync(path.join(out, `${label}.ffmpeg.log`), 'w')] });
  } else {
    profile = fs.mkdtempSync(path.join(os.tmpdir(), 'w0-chrome-'));
    const page = cmd === 'loopback' ? `loopback.html?which=${opt('which', 'all')}` : `capture.html?label=${label}`;
    child = spawn(CHROME, ['--headless=new', `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check',
      '--autoplay-policy=no-user-gesture-required', '--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream',
      '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows',
      '--remote-debugging-port=0', `http://127.0.0.1:${srv.httpPort}/${page}`], { stdio: 'ignore', detached: true });
  }
  let t;
  const timedOut = new Promise((r) => { t = setTimeout(() => { console.log('timeout'); srv.ev.timedOut = true; r(); }, timeoutMs); });
  await Promise.race([srv.done, timedOut, new Promise((r) => (child.on('exit', r)))]);
  if (cmd === 'ffmpeg') await new Promise((r) => setTimeout(r, 1500));
  clearTimeout(t);
  srv.close();
  // Stop only what we started: our child's own process group, then wait for it.
  try { process.kill(cmd === 'ffmpeg' ? child.pid : -child.pid, 'SIGTERM'); } catch {}
  await new Promise((r) => setTimeout(r, 2500));
  try { process.kill(cmd === 'ffmpeg' ? child.pid : -child.pid, 0); console.log('child group still alive, sending SIGKILL'); process.kill(cmd === 'ffmpeg' ? child.pid : -child.pid, 'SIGKILL'); } catch {}
  if (profile) trash(profile);
  console.log('events:', JSON.stringify(srv.ev));
  process.exit(0);
})();
