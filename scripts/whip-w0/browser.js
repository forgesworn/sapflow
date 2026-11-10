'use strict';
// Launchers for the browsers other than Chrome. Each returns { pid, exited, stop(), ... }.
// Stops only what it started, by PID. Never pkill.
const { spawn, spawnSync } = require('node:child_process');
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
  // W0_SAFARI_AGENT=1: run the driver as a LaunchAgent in the owner's GUI session (an ssh-started driver cannot reach Safari).
  // The agent is booted out and its plist moved to the Trash by stop().
  let child, plist = null;
  const label = 'dev.w0.safaridriver', uid = String(process.getuid());
  if (process.env.W0_SAFARI_AGENT) {
    plist = path.join(os.tmpdir(), label + '.plist');
    fs.writeFileSync(plist, `<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict><key>Label</key><string>${label}</string><key>ProgramArguments</key><array><string>${SAFARIDRIVER}</string><string>-p</string><string>${port}</string></array><key>RunAtLoad</key><true/><key>StandardOutPath</key><string>${log}</string><key>StandardErrorPath</key><string>${log}</string></dict></plist>\n`);
    const r = spawnSync('launchctl', ['bootstrap', `gui/${uid}`, plist]);
    if (r.status !== 0) throw new Error('launchctl bootstrap failed: ' + r.stderr);
    child = new (require('node:events'))(); child.pid = null;
    await sleep(2000);
  } else {
    child = spawn(SAFARIDRIVER, ['-p', String(port)], { stdio: ['ignore', fs.openSync(log, 'w'), fs.openSync(log, 'a')], detached: true });
  }
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
      if (plist) {
        spawnSync('launchctl', ['bootout', `gui/${uid}/${label}`]);
        await sleep(500);
        h.agentUnloaded = spawnSync('launchctl', ['print', `gui/${uid}/${label}`]).status !== 0;
        trash(plist);
      } else await stopGroup(child.pid, 'safaridriver');
      try { trash(log); } catch {}
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
  const s = await call('POST', '/session', { capabilities: { alwaysMatch: { browserName: 'safari', ...(process.env.W0_SAFARI_NOFILTER ? { 'webkit:WebRTC': { DisableICECandidateFiltering: true } } : {}) } } });
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
