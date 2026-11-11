'use strict';
// Phone drivers for W0. Runs on the host the phones are attached to. Written from the RFCs and observed
// traffic, like the rest of this tool. Nothing is installed on a phone. Usage:
//   ANDROID_SERIAL=<serial> node phone-run.js android probe
//   ANDROID_SERIAL=<serial> node phone-run.js android capture udp|tcp --host <lan-ip> --range 47200-47399 --out <dir> [--port N]
//   ANDROID_SERIAL=<serial> node phone-run.js android loopback --host <lan-ip> --range .. --out <dir> [--runs ramp,ramp-a,ramp-b,ramp-a,ramp-b,opus]
//   ANDROID_SERIAL=<serial> node phone-run.js android play --vendor <hls.js dir> --range .. --out <dir>
//   ANDROID_SERIAL=<serial> node phone-run.js android rotate --host <lan-ip> --range .. --out <dir>
//   node phone-run.js ios capture|play --device <devicectl id> --host <lan-ip> --range .. --out <dir>
// The Android browser is app.vanadium.browser unless PHONE_BROWSER says otherwise. The page and the WHIP POST go over
// `adb reverse` (a secure context for the camera); media goes over Wi-Fi to --host. Every reverse is removed on exit,
// the display rotation settings are restored, and the phone is sent to its home screen. The phone is never woken or
// unlocked: a locked or dozing phone makes the run stop with exit code 3.
const { execFileSync, spawn } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { start } = require('./fake-whip.js');
const { startPlayServer } = require('./play-server.js');
const { parseRange } = require('./ports.js');

const args = process.argv.slice(2);
const [target, cmd] = args;
const opt = (n, d) => { const i = args.indexOf('--' + n); return i > 0 ? args[i + 1] : d; };
const mode = ['udp', 'tcp'].includes(args[2]) ? args[2] : 'udp';
const range = parseRange(opt('range', '47200-47399'));
const out = path.resolve(opt('out', './w0-phone-out'));
const host = opt('host');
const BROWSER = process.env.PHONE_BROWSER || 'app.vanadium.browser';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (...a) => console.log(new Date().toISOString().slice(11, 19), ...a);

// ---------- Android helpers
const adbOut = (...a) => execFileSync('adb', a, { encoding: 'utf8', timeout: 30000 });
const sh = (cmdline) => adbOut('shell', cmdline);
const reversed = new Set();
function reverse(port) { adbOut('reverse', `tcp:${port}`, `tcp:${port}`); reversed.add(port); }
function unreverseAll() { for (const p of reversed) { try { adbOut('reverse', '--remove', `tcp:${p}`); } catch (e) { log('reverse remove failed', p, e.message); } } reversed.clear(); }
function state() {
  const power = /mWakefulness=(\w+)/.exec(sh('dumpsys power'))[1];
  const keyguard = /isKeyguardShowing=(\w+)/.exec(sh('dumpsys window'))[1] === 'true';
  const top = (/topResumedActivity=ActivityRecord\{\S+ \S+ (\S+)/.exec(sh('dumpsys activity activities')) || [])[1] || null;
  return { power, keyguard, top };
}
function notes() { // thermal and load notes from the phone, read only
  const bat = sh('dumpsys battery');
  const thermal = (/Thermal Status: (\d+)/.exec(sh('dumpsys thermalservice')) || [])[1] || null;
  let load = null; try { load = sh('cat /proc/loadavg').trim().split(' ').slice(0, 3).join(' '); } catch {}
  return { batteryTempTenthsC: +(/temperature: (\d+)/.exec(bat) || [])[1] || null, batteryLevel: +(/level: (\d+)/.exec(bat) || [])[1] || null, thermalStatus: thermal, loadavg: load };
}
function usable() {
  const s = state();
  if (s.power !== 'Awake' || s.keyguard) { log('phone is not awake and unlocked:', JSON.stringify(s)); const e = new Error('phone locked or asleep'); e.code = 'LOCKED'; throw e; }
  return s;
}
function open(url) { sh(`am start -a android.intent.action.VIEW -d '${url}' ${BROWSER}`); }
const screenSize = () => { const m = /(\d+)x(\d+)/.exec(sh('wm size')); return [+m[1], +m[2]]; };
function tapFraction(fx, fy) { const [w, h] = screenSize(); sh(`input tap ${Math.round(w * fx)} ${Math.round(h * fy)}`); }
function home() { try { sh('input keyevent KEYCODE_HOME'); } catch {} }

// Answers the camera and microphone prompts by reading the UI tree (uiautomator, read only) and tapping the button.
// Prefers a one-time grant, so no permission is left behind on the phone. Logs only the matched button and the prompt text.
function startTapper() {
  let stop = false; const taps = [];
  const prefs = [/^Only this time$/i, /^Allow this time$/i, /^While using the app$/i, /^Allow while visiting the site$/i, /^Allow$/i];
  (async () => {
    while (!stop) {
      try {
        const xml = adbOut('exec-out', 'uiautomator', 'dump', '/dev/stdout');
        const nodes = [...xml.matchAll(/<node [^>]*?text="([^"]*)"[^>]*?bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"/g)].map((m) => ({ text: m[1], b: m.slice(2).map(Number) }));
        const prompt = nodes.filter((n) => /camera|microphone|record|take pictures|use your/i.test(n.text) && n.text.length < 120).map((n) => n.text);
        const go = nodes.find((n) => /^(start|play)$/.test(n.text) && n.b[2] - n.b[0] > 200); // the page's gesture button (loopback and play pages)
        if (go) { sh(`input tap ${(go.b[0] + go.b[2]) >> 1} ${(go.b[1] + go.b[3]) >> 1}`); taps.push({ t: Date.now(), button: go.text }); log('tapped page button', go.text); }
        if (prompt.length) {
          for (const p of prefs) {
            const hit = nodes.find((n) => p.test(n.text));
            if (hit) { sh(`input tap ${(hit.b[0] + hit.b[2]) >> 1} ${(hit.b[1] + hit.b[3]) >> 1}`); taps.push({ t: Date.now(), button: hit.text, prompt: prompt.slice(0, 3) }); log('tapped', hit.text, '|', prompt[0]); break; }
          }
        }
      } catch (e) { /* a dump can fail while the screen changes */ }
      await sleep(1500);
    }
  })();
  return { taps, stop() { stop = true; } };
}

async function waitDone(srv, ms, label) {
  let t; const timeout = new Promise((r) => { t = setTimeout(() => { log(label, 'timeout'); srv.ev && (srv.ev.timedOut = true); r(); }, ms); });
  await Promise.race([srv.done, timeout]); clearTimeout(t);
}

async function androidCapture() {
  const label = `vanadium-${mode}`;
  const srv = await start({ mode, host, outDir: out, label, relayPort: +opt('port', 47399), range });
  log(`label=${label} http=${srv.httpPort} udp=${srv.udpPort} relay=${srv.tcpPort}`);
  reverse(srv.httpPort);
  const tap = startTapper(), before = notes();
  open(`http://localhost:${srv.httpPort}/capture.html?label=${label}`);
  await waitDone(srv, 60000, label);
  tap.stop(); srv.ev.phoneNotes = { before, after: notes() }; srv.ev.prompts = tap.taps;
  srv.close(); log('events', JSON.stringify(srv.ev));
}

async function androidLoopback() {
  const runs = opt('runs', 'ramp,ramp-a,ramp-b,ramp-a,ramp-b,opus').split(',');
  let n = 0;
  for (const which of runs) {
    usable();
    const label = `loop${++n}-${which}`, dir = path.join(out, label);
    const srv = await start({ mode: 'udp', host, outDir: dir, label, range });
    reverse(srv.httpPort);
    const tap = startTapper(), before = notes();
    const isOpus = which === 'opus';
    open(`http://localhost:${srv.httpPort}/loopback.html?which=${which}${isOpus ? '&gesture=1' : '&source=camera'}`);
    if (isOpus) { await sleep(6000); tapFraction(0.5, 0.3); await sleep(4000); tapFraction(0.5, 0.3); }
    log(label, 'started', JSON.stringify(before));
    await waitDone(srv, isOpus ? 240000 : 150000, label);
    tap.stop(); const after = notes();
    fs.writeFileSync(path.join(dir, 'phone-notes.json'), JSON.stringify({ before, after, prompts: tap.taps }, null, 1));
    srv.close(); try { adbOut('reverse', '--remove', `tcp:${srv.httpPort}`); reversed.delete(srv.httpPort); } catch {}
    log(label, 'done', JSON.stringify(after)); await sleep(3000);
  }
}

async function androidPlay() {
  const vendorDir = path.resolve(opt('vendor')), assetsDir = path.resolve(opt('assets', path.join(os.homedir(), 'whip-w0-assets')));
  const srv = await startPlayServer({ assetsDir, vendorDir, outDir: out, range });
  reverse(srv.port);
  const A = { 'a-av': '/assets/av/master.m3u8', 'b-audio': '/assets/audio/master.m3u8', 'c-recording': '/assets/recording/recording.mp4', 'd-av-preskip312': '/assets/av-preskip312/master.m3u8' };
  const cases = [['hlsjs', 'a-av'], ['hlsjs', 'b-audio'], ['hlsjs', 'd-av-preskip312'], ['native', 'c-recording'], ['native', 'a-av'], ['native', 'b-audio']];
  const seconds = 14;
  for (const [player, asset] of cases) {
    usable();
    const label = `vanadium-${player}-${asset}`;
    open(`http://localhost:${srv.port}/play.html?src=${encodeURIComponent(A[asset])}&player=${player}&label=${label}&seconds=${seconds}&gesture=1&quiet=1`);
    await sleep(5000); tapFraction(0.5, 0.3);
    let ok = await srv.waitFor(label, 6000);
    if (!ok) { tapFraction(0.5, 0.3); ok = await srv.waitFor(label, (seconds + 40) * 1000); }
    log(label, ok ? 'finished' : 'TIMEOUT', JSON.stringify(notes()));
    await sleep(1500);
  }
  fs.writeFileSync(path.join(out, 'play-requests-sample.json'), JSON.stringify(srv.requests.slice(0, 20)));
  srv.close();
}

async function androidRotate() {
  const orig = { accel: sh('settings get system accelerometer_rotation').trim(), user: sh('settings get system user_rotation').trim() };
  log('original rotation settings', JSON.stringify(orig));
  fs.mkdirSync(out, { recursive: true }); fs.writeFileSync(path.join(out, 'rotation-originals.json'), JSON.stringify(orig));
  const srv = await start({ mode: 'udp', host, outDir: out, label: 'vanadium-rotate', range });
  reverse(srv.httpPort);
  const tap = startTapper(), before = notes();
  const seen = (f) => fs.existsSync(path.join(out, f));
  const until = async (f, ms) => { for (let t = 0; t < ms / 500; t++) { if (seen(f)) return true; await sleep(500); } return false; };
  const rec = { orig, steps: [] };
  try {
    open(`http://localhost:${srv.httpPort}/rotate.html?settle=7000`);
    rec.p0 = await until('rot-p0.json', 60000); log('p0', rec.p0);
    if (rec.p0) {
      sh('settings put system accelerometer_rotation 0'); sh('settings put system user_rotation 1'); rec.steps.push('accelerometer_rotation=0 user_rotation=1');
      rec.p1 = await until('rot-p1.json', 60000); log('p1', rec.p1);
      sh('settings put system user_rotation 0'); rec.steps.push('user_rotation=0');
      rec.p2 = await until('rot-p2.json', 60000); log('p2', rec.p2);
    }
    await waitDone(srv, 20000, 'rotate');
  } finally {
    // Restore exactly. A key that read "null" is deleted again.
    for (const [k, v] of [['accelerometer_rotation', orig.accel], ['user_rotation', orig.user]]) {
      if (v === 'null') sh(`settings delete system ${k}`); else sh(`settings put system ${k} ${v}`);
    }
    rec.restored = { accel: sh('settings get system accelerometer_rotation').trim(), user: sh('settings get system user_rotation').trim() };
    log('restored', JSON.stringify(rec.restored));
  }
  tap.stop(); rec.phoneNotes = { before, after: notes() }; rec.prompts = tap.taps;
  fs.writeFileSync(path.join(out, 'rotate-run.json'), JSON.stringify(rec, null, 1));
  srv.close();
}

// ---------- iPhone: Safari opened with `devicectl device process launch --payload-url`. Synthetic media, plain http.
function iosOpen(device, url) {
  const r = execFileSync('xcrun', ['devicectl', 'device', 'process', 'launch', '--device', device, '--payload-url', url, 'com.apple.mobilesafari'], { encoding: 'utf8', timeout: 60000 });
  return r.trim().split('\n').slice(-3).join(' | ');
}
async function iosRun() {
  const device = opt('device');
  if (cmd === 'capture') {
    const label = 'safari-ios-udp';
    const srv = await start({ mode: 'udp', host, httpHost: host, outDir: out, label, range });
    log(`label=${label} http=${srv.httpPort} udp=${srv.udpPort}`);
    try { log('launch:', iosOpen(device, `http://${host}:${srv.httpPort}/capture.html?label=${label}&synthetic=1`)); } catch (e) { srv.ev.launchError = String(e.stderr || e.message).slice(0, 600); log('launch failed', srv.ev.launchError); srv.close(); return; }
    await waitDone(srv, 75000, label); srv.close(); log('events', JSON.stringify(srv.ev));
  } else if (cmd === 'play') {
    const vendorDir = path.resolve(opt('vendor', os.tmpdir())), assetsDir = path.resolve(opt('assets', path.join(os.homedir(), 'whip-w0-assets')));
    const srv = await startPlayServer({ assetsDir, vendorDir, outDir: out, range, host });
    log('play server on', srv.port);
    try { log('launch:', iosOpen(device, `http://${host}:${srv.port}/ios-play.html?seconds=12`)); } catch (e) { log('launch failed', String(e.stderr || e.message).slice(0, 600)); srv.close(); return; }
    const ok = await srv.waitFor('ios', 150000); log(ok ? 'finished' : 'TIMEOUT'); srv.close();
  }
}

(async () => {
  fs.mkdirSync(out, { recursive: true });
  try {
    if (target === 'ios') await iosRun();
    else if (target === 'android') {
      if (cmd === 'probe') { console.log(JSON.stringify({ state: state(), notes: notes(), size: screenSize(), browser: sh(`dumpsys package ${BROWSER}`).split('\n').filter((l) => /versionName/.test(l)).map((l) => l.trim()) })); return; }
      usable();
      if (cmd === 'capture') await androidCapture();
      else if (cmd === 'loopback') await androidLoopback();
      else if (cmd === 'play') await androidPlay();
      else if (cmd === 'rotate') await androidRotate();
      else throw new Error('unknown command ' + cmd);
    }
  } catch (e) { console.log('error:', e.message); process.exitCode = e.code === 'LOCKED' ? 3 : 1; }
  finally { if (target === 'android' && cmd !== 'probe') { unreverseAll(); home(); } process.exit(process.exitCode || 0); }
})();
