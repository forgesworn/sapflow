'use strict';
// Launchers for the browsers other than Chrome. Each returns { pid, exited, stop(), ... }.
// Stops only what it started, by PID. Never pkill.
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const ports = require('./ports.js');

const FIREFOX = process.env.FIREFOX || path.join(os.homedir(), 'Applications/Firefox.app/Contents/MacOS/firefox');
const SAFARIDRIVER = '/usr/bin/safaridriver';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function trash(p) {
  const dest = path.join(os.homedir(), '.Trash', path.basename(p) + '-' + Date.now());
  try { fs.renameSync(p, dest); } catch (e) { console.error('could not move to Trash:', p, e.message); }
}
async function stopGroup(pid, label) {
  try { process.kill(-pid, 'SIGTERM'); } catch {}
  await sleep(2500);
  try { process.kill(-pid, 0); console.log(label + ': group still alive, SIGKILL'); process.kill(-pid, 'SIGKILL'); } catch {}
}

function launchFirefox(url, { extraPrefs = {} } = {}) {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'w0-firefox-'));
  const prefs = {
    'media.navigator.streams.fake': true, 'media.navigator.permission.disabled': true,
    'media.autoplay.default': 0, 'media.autoplay.blocking_policy': 0,
    'browser.shell.checkDefaultBrowser': false, 'app.update.enabled': false, 'app.update.auto': false,
    'datareporting.policy.dataSubmissionEnabled': false, 'toolkit.telemetry.reportingpolicy.firstRun': false,
    ...extraPrefs,
  };
  fs.writeFileSync(path.join(profile, 'user.js'), Object.entries(prefs).map(([k, v]) => `user_pref(${JSON.stringify(k)}, ${JSON.stringify(v)});`).join('\n') + '\n');
  const child = spawn(FIREFOX, ['-headless', '-no-remote', '-profile', profile, url], { stdio: 'ignore', detached: true });
  return {
    child, pid: child.pid, profile, prefs,
    exited: new Promise((r) => child.on('exit', r)),
    async stop() { await stopGroup(child.pid, 'firefox'); trash(profile); },
  };
}

// Safari through safaridriver (W3C WebDriver). Opens its own automation window and
// ends it again with DELETE /session. Throws an error whose .safariAutomationDisabled
// is true when remote automation is not enabled (needs `sudo safaridriver --enable`, the owner's job).
async function launchSafari(url, { range } = {}) {
  const port = await ports.inRange(range, (p) => new Promise((res, rej) => { // find a free port, then release it for safaridriver
    const s = ports.net.createServer(); s.once('error', rej); s.listen(p, '127.0.0.1', () => { const q = s.address().port; s.close(() => res(q)); });
  }));
  const log = path.join(os.tmpdir(), `w0-safaridriver-${process.pid}.log`);
  // W0_SAFARI_ASUSER=1 starts the driver in the owner's GUI session (launchctl asuser), for ssh sessions that cannot reach Safari.
  const cmd = process.env.W0_SAFARI_ASUSER ? ['/bin/launchctl', ['asuser', String(process.getuid()), SAFARIDRIVER, '-p', String(port)]] : [SAFARIDRIVER, ['-p', String(port)]];
  const child = spawn(cmd[0], cmd[1], { stdio: ['ignore', fs.openSync(log, 'w'), fs.openSync(log, 'a')], detached: true });
  const base = `http://127.0.0.1:${port}`;
  const call = async (method, p, body) => {
    const r = await fetch(base + p, { method, headers: { 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const j = await r.json().catch(() => ({}));
    return { status: r.status, value: j.value };
  };
  let session = null;
  const h = {
    child, pid: child.pid, port, call, log,
    exited: new Promise((r) => child.on('exit', r)),
    async stop() {
      if (session) { try { await call('DELETE', '/session/' + session); h.sessionEnded = true; } catch (e) { h.sessionEnded = 'delete failed: ' + e.message; } session = null; }
      await sleep(500);
      await stopGroup(child.pid, 'safaridriver'); trash(log);
    },
    async script(src, args = []) { return call('POST', `/session/${session}/execute/sync`, { script: src, args }); },
    async click(selector) {
      const f = await call('POST', `/session/${session}/element`, { using: 'css selector', value: selector });
      const id = f.value && (f.value['element-6066-11e4-a52e-4f735466cecf'] || Object.values(f.value)[0]);
      if (!id) return { error: 'element not found', f };
      return call('POST', `/session/${session}/element/${id}/click`, {});
    },
  };
  for (let i = 0; i < 40; i++) { try { if ((await fetch(base + '/status')).ok) break; } catch {} await sleep(250); }
  const s = await call('POST', '/session', { capabilities: { alwaysMatch: { browserName: 'safari' } } });
  if (s.status !== 200 || !s.value || !s.value.sessionId) {
    const e = new Error('safari session failed: ' + JSON.stringify(s));
    e.safariAutomationDisabled = /enable|remote automation|not allowed/i.test(JSON.stringify(s));
    await h.stop(); throw e;
  }
  session = s.value.sessionId; h.sessionId = session; h.capabilities = s.value.capabilities;
  const nav = await call('POST', `/session/${session}/url`, { url });
  h.navigate = nav;
  return h;
}

module.exports = { launchFirefox, launchSafari, trash, stopGroup, sleep };

// Chrome, headless, same flags as run.js. Used by the A0.2 playback runs.
const CHROME = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
function launchChrome(url) {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'w0-chrome-'));
  const child = spawn(CHROME, ['--headless=new', `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check',
    '--autoplay-policy=no-user-gesture-required', '--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream',
    '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows', url], { stdio: 'ignore', detached: true });
  return { child, pid: child.pid, profile, exited: new Promise((r) => child.on('exit', r)), async stop() { await stopGroup(child.pid, 'chrome'); trash(profile); } };
}
module.exports.launchChrome = launchChrome;
