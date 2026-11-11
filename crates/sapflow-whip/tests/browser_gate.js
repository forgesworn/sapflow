'use strict';
// W3a gate 2: a real browser publishes fake camera and microphone to the
// whip_harness example on loopback, and the harness checks every milestone.
// Dependency-free Node 24. Stops only what it started, by PID; never pkill.
//
//   cargo build -p sapflow-whip --features testing --example whip_harness [--target <triple>]
//   node crates/sapflow-whip/tests/browser_gate.js --bin <harness> [--browser chrome|firefox] \
//     [--http 47201] [--udp 47202] [--record <dir>] [--deadline 40] [--loopback-flag]
//
// CHROME or FIREFOX picks the binary. Firefox is launched as
// scripts/whip-w0/browser.js does: headless, a temporary profile, fake media
// prefs in user.js. Profiles go to the Trash afterwards.
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const args = process.argv.slice(2);
const opt = (n, d) => { const i = args.indexOf('--' + n); return i >= 0 ? args[i + 1] : d; };
const has = (n) => args.includes('--' + n);
const bin = opt('bin');
const browser = opt('browser', 'chrome');
const http = +opt('http', 47201), udp = +opt('udp', 47202);
const deadline = +opt('deadline', 40);
const CHROME = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const FIREFOX = process.env.FIREFOX || path.join(os.homedir(), 'Applications/Firefox.app/Contents/MacOS/firefox');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function trash(p) {
  const dest = path.join(os.homedir(), '.Trash', path.basename(p) + '-' + Date.now());
  try { fs.renameSync(p, dest); } catch (e) { console.error('could not move to the Trash:', p, e.message); }
}

async function stopGroup(pid) {
  try { process.kill(-pid, 'SIGTERM'); } catch {}
  await sleep(2000);
  try { process.kill(-pid, 0); process.kill(-pid, 'SIGKILL'); } catch {}
}

function launch(url) {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), `whip-gate-${browser}-`));
  let child;
  if (browser === 'firefox') {
    if (!fs.existsSync(FIREFOX)) throw new Error('Firefox is not installed at ' + FIREFOX);
    const prefs = {
      'media.navigator.streams.fake': true, 'media.navigator.permission.disabled': true,
      'browser.shell.checkDefaultBrowser': false, 'app.update.enabled': false,
      'datareporting.policy.dataSubmissionEnabled': false, 'toolkit.telemetry.reportingpolicy.firstRun': false,
    };
    fs.writeFileSync(path.join(profile, 'user.js'), Object.entries(prefs).map(([k, v]) => `user_pref(${JSON.stringify(k)}, ${JSON.stringify(v)});`).join('\n') + '\n');
    child = spawn(FIREFOX, ['-headless', '-no-remote', '-profile', profile, url], { stdio: 'ignore', detached: true });
  } else {
    const flags = ['--headless=new', `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check',
      '--autoplay-policy=no-user-gesture-required', '--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream',
      '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows'];
    if (has('loopback-flag')) flags.push('--allow-loopback-in-peer-connection');
    child = spawn(CHROME, [...flags, url], { stdio: 'ignore', detached: true });
  }
  return { child, profile };
}

(async () => {
  if (!bin) { console.error('--bin <harness binary> is required'); process.exit(2); }
  const hargs = ['--http', `127.0.0.1:${http}`, '--udp', `127.0.0.1:${udp}`, '--deadline', String(deadline)];
  if (opt('record')) hargs.push('--record', opt('record'));
  const harness = spawn(bin, hargs, { stdio: ['ignore', 'pipe', 'pipe'] });
  let ready;
  const bound = new Promise((r) => { ready = r; });
  harness.stdout.on('data', (d) => { process.stdout.write(d); if (String(d).includes('bound')) ready(); });
  harness.stderr.on('data', (d) => process.stderr.write(d));
  const exited = new Promise((r) => harness.on('exit', (code) => r(code)));
  await Promise.race([bound, sleep(5000)]);
  let b;
  try { b = launch(`http://127.0.0.1:${http}/`); } catch (e) {
    console.log('launch failed:', e.message);
    harness.kill('SIGTERM');
    process.exit(3);
  }
  console.log(`browser: ${browser} started`);
  const code = await Promise.race([exited, sleep((deadline + 10) * 1000).then(() => 'timeout')]);
  if (code === 'timeout') harness.kill('SIGTERM');
  await stopGroup(b.child.pid);
  trash(b.profile);
  console.log(`gate: harness exit ${code}`);
  process.exit(code === 0 ? 0 : 1);
})();
