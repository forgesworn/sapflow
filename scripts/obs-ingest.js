'use strict';
// OBS (the macOS app) publishing SRT or RTMP to sapflow on loopback, with
// OBS's own x264 streaming defaults, as a broadcaster's OBS would: does the
// stream come up, what does the recording hold, and, with --restart-at, how
// OBS's automatic reconnect behaves when sapflow restarts under it (as at a
// redeploy). Dependency-free Node 24; needs ffmpeg, ffprobe and OBS. Stops
// only what it started, by PID.
//
//   node scripts/obs-ingest.js --bin <sapflow> --out <dir> --protocol srt|rtmp
//        [--seconds 60] [--obs-keyint 1] [--bitrate 2500] [--restart-at <s>]
//        [--base 47400] [--obs /Applications/OBS.app]
//   node scripts/obs-ingest.js --bin <sapflow> --out <dir> --protocol srt --app-key
//        [--app-key-wrong] [--seconds 60] [--restart-at <s>] ...
//   node scripts/obs-ingest.js --remote --out <dir> --protocol srt|rtmp
//        [--seconds 60] [--obs-keyint 1] [--bitrate 2500] < creds.json
//
// --remote streams to a real server instead (for a path across the
// internet). creds.json is {"server": "srt://...", "key": ""} or
// {"server": "rtmps://.../live", "key": "..."}, read on standard input so
// the credentials reach no command line; keep it mode 0600, outside the
// repository, or pipe it from a keychain. No local sapflow is started, and
// the run lasts --seconds; the server's own logs are the result.
//
// --app-key runs sapflow with per-stream SRT passphrases from the app
// (SAPFLOW_SRT_APP_KEYS=1, encryption required, no global passphrase): a
// stub publish-auth hook on loopback answers sapflow's SRT request with the
// stream's passphrase in the `sapflow-srt-passphrase` header, and OBS's URL
// carries only the path in its stream ID and that passphrase. With
// --app-key-wrong, OBS is given a different passphrase and should never be
// admitted.
//
// In every mode the test profile's files are overwritten before the changed
// config goes to the Trash, and every OBS log kept (in --out, and in the
// Trash copy) has the server, keys, URLs and IP addresses removed. Ctrl-C
// or SIGTERM ends the run early and still restores the config.
//
// As scripts/whip-obs.js:
//   1. refuses to run if OBS is already running (never touches the user's
//      session);
//   2. copies OBS's whole config directory to a scratch backup;
//   3. writes a dedicated profile and scene collection, "Ingest-OBS-test":
//      Advanced output, x264 CBR at --bitrate kbit/s with OBS's defaults
//      otherwise (veryfast, high profile, B-frames), keyframes every
//      --obs-keyint s (0 is OBS's own "auto"), 1280x720 at 30 fps, AAC 160k,
//      and one media source looping a generated clip (testsrc2 and a tone);
//      the custom service points at sapflow: SRT with v4v's 2000 ms latency
//      and AES-256, or RTMP with the credentials in the key;
//   4. starts sapflow (recording, `whole` HLS as the reference deployment),
//      then OBS with --profile, --collection and --startstreaming;
//   5. with --restart-at, stops sapflow that many seconds after the stream is
//      ready and starts it again 3 s later, then waits for OBS to reconnect;
//   6. after --seconds, ends OBS by its own PID, then sapflow;
//   7. puts the config directory back from the backup, whatever happened,
//      and diffs it against the backup.
//
// Results in --out: sapflow logs, the recordings, OBS's log and result.json.
const { spawn, spawnSync, execFileSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');

const args = process.argv.slice(2);
const opt = (n, d) => { const i = args.indexOf('--' + n); return i >= 0 ? args[i + 1] : d; };
const bin = opt('bin'), out = opt('out'), protocol = opt('protocol');
const remote = args.includes('--remote');
const appKeyWrong = args.includes('--app-key-wrong');
const appKey = args.includes('--app-key') || appKeyWrong;
if (appKey && (remote || protocol !== 'srt')) {
  console.error('--app-key needs a local sapflow (--bin) and --protocol srt'); process.exit(2);
}
if ((!bin && !remote) || !out || !['srt', 'rtmp'].includes(protocol)) {
  console.error('--bin (or --remote), --out and --protocol srt|rtmp are required'); process.exit(2);
}
const seconds = +opt('seconds', 60), base = +opt('base', 47400);
const obsKeyint = +opt('obs-keyint', 1), bitrate = +opt('bitrate', 2500);
const restartAt = opt('restart-at') === undefined ? null : +opt('restart-at');
let creds = null;
if (remote) {
  // A fixed message: a parse error would echo the start of the input.
  try { creds = JSON.parse(fs.readFileSync(0, 'utf8')); } catch {
    console.error('--remote reads {"server": ..., "key": ...} as JSON on standard input; that was not JSON'); process.exit(2);
  }
  const scheme = protocol === 'srt' ? /^srt:\/\// : /^rtmps?:\/\//;
  if (!creds || typeof creds.server !== 'string' || !scheme.test(creds.server)
    || (creds.key !== undefined && typeof creds.key !== 'string')) {
    console.error(`--remote needs "server", a string starting ${protocol === 'srt' ? 'srt://' : 'rtmp:// or rtmps://'}, and "key", a string, if any`); process.exit(2);
  }
  if (restartAt !== null) { console.error('--restart-at restarts a local sapflow; it does not apply to --remote'); process.exit(2); }
}
const obsApp = opt('obs', '/Applications/OBS.app');
const OBS = path.join(obsApp, 'Contents/MacOS/OBS');
const NAME = 'Ingest-OBS-test';
const CONFIG = path.join(os.homedir(), 'Library/Application Support/obs-studio');
const port = { hls: base, srt: base + 1, rtmp: base + 2, hook: base + 8, websocket: base + 9 };
// Where OBS sends SRT; another port puts a relay in between (for example to
// record its handshakes). The app-key mode only.
const obsSrtPort = +opt('obs-srt-port', port.srt);
const EVENT = 'event-obs';
const USER = 'obs', PASS = crypto.randomBytes(12).toString('hex');
const PASSPHRASE = 'obs-' + crypto.randomBytes(12).toString('hex');
// The stream's passphrase the stub hook hands sapflow, and what OBS is given.
const APP_KEY = 'obs-app-' + crypto.randomBytes(12).toString('hex');
const OBS_KEY = appKeyWrong ? 'obs-wrong-' + crypto.randomBytes(12).toString('hex') : APP_KEY;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
fs.mkdirSync(out, { recursive: true });

// Ctrl-C, SIGTERM or SIGHUP ends the run's waits (pause), so the cleanup
// still ends OBS and restores the config; without this, OBS (detached) would
// keep streaming with the key in its profile.
let interrupted = false;
const wakers = new Set();
for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.on(sig, () => {
    if (!interrupted) console.error(`${sig}: ending the run; restoring OBS's config, which takes up to a minute`);
    interrupted = true;
    for (const wake of wakers) wake();
  });
}
const pause = (ms) => new Promise((resolve, reject) => {
  if (interrupted) return reject(new Error('interrupted'));
  const wake = () => { clearTimeout(timer); wakers.delete(wake); reject(new Error('interrupted')); };
  const timer = setTimeout(() => { wakers.delete(wake); resolve(); }, ms);
  wakers.add(wake);
});

// Everything secret or identifying a log could hold: this run's own
// credentials, and in remote mode the server, its host, the key and every
// value in either. Removed by value first, then by shape.
function secrets() {
  const found = [PASS, PASSPHRASE, APP_KEY, OBS_KEY];
  if (creds) {
    const key = creds.key || '';
    found.push(creds.server, key, ...key.split(/[?&=:]/));
    try {
      const u = new URL(creds.server);
      found.push(u.host, u.hostname);
      for (const v of u.searchParams.values()) found.push(v, ...v.split(':'));
    } catch {}
  }
  return [...new Set(found)].filter((x) => x && x.length >= 6).sort((a, b) => b.length - a.length);
}
function redact(text) {
  for (const secret of secrets()) text = text.split(secret).join('<redacted>');
  return text
    .replace(/(srt|rtmps?):\/\/\S+/g, '$1://<redacted>')
    .replace(/(key|streamid|passphrase|pass)=\S+/gi, '$1=<redacted>')
    .replace(/\b\d{1,3}(\.\d{1,3}){3}\b/g, '<ip>');
}
function redactFile(file) {
  try { fs.writeFileSync(file, redact(fs.readFileSync(file, 'utf8'))); } catch {}
}
// The test profile held the server and key, maybe in OBS's backups too.
function blankProfile() {
  const pdir = path.join(CONFIG, 'basic/profiles', NAME);
  try { for (const f of fs.readdirSync(pdir)) fs.writeFileSync(path.join(pdir, f), ''); } catch {}
}

function trash(p) {
  const dest = path.join(os.homedir(), '.Trash', path.basename(p) + '-' + Date.now());
  fs.renameSync(p, dest); return dest;
}
function running(pid) { try { process.kill(pid, 0); return true; } catch { return false; } }
function obsPids() { const r = spawnSync('pgrep', ['-x', 'OBS']); return r.status === 0 ? r.stdout.toString().trim().split('\n').map(Number) : []; }
function fetch(url) {
  return new Promise((resolve) => {
    const req = http.get(url, { timeout: 1000 }, (res) => {
      let body = ''; res.on('data', (c) => { body += c; }); res.on('end', () => resolve(res.statusCode === 200 ? body : ''));
    });
    req.on('error', () => resolve('')); req.on('timeout', () => { req.destroy(); resolve(''); });
  });
}
const playlist = () => fetch(`http://127.0.0.1:${port.hls}/live/${EVENT}/stream.m3u8`);

function clip() {
  const f = path.join(out, 'obs-source.mp4');
  if (!fs.existsSync(f)) {
    const r = spawnSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=size=1280x720:rate=30',
      '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000', '-t', '60', '-pix_fmt', 'yuv420p', '-c:v', 'libx264',
      '-preset', 'veryfast', '-g', '30', '-b:v', '4000k', '-c:a', 'aac', '-b:a', '128k', f], { encoding: 'utf8' });
    if (r.status !== 0) throw new Error('clip: ' + r.stderr);
  }
  return f;
}

function service() {
  if (remote) return { server: creds.server, key: creds.key || '' };
  if (appKey) {
    // Only the path in the stream ID: the passphrase is the credential.
    const q = `streamid=publish:live/${EVENT}&latency=2000000&passphrase=${OBS_KEY}&pbkeylen=32`;
    return { server: `srt://127.0.0.1:${obsSrtPort}?${q}`, key: '' };
  }
  if (protocol === 'srt') {
    // OBS's SRT output is FFmpeg's libsrt: latency in microseconds.
    const q = `streamid=publish:live/${EVENT}:${USER}:${PASS}&latency=2000000&passphrase=${PASSPHRASE}&pbkeylen=32`;
    return { server: `srt://127.0.0.1:${port.srt}?${q}`, key: '' };
  }
  return { server: `rtmp://127.0.0.1:${port.rtmp}/live`, key: `${EVENT}?user=${USER}&pass=${PASS}` };
}

function writeProfile(source) {
  const pdir = path.join(CONFIG, 'basic/profiles', NAME);
  fs.mkdirSync(pdir, { recursive: true });
  fs.writeFileSync(path.join(pdir, 'basic.ini'), [
    '[General]', `Name=${NAME}`, '',
    '[Video]', 'BaseCX=1280', 'BaseCY=720', 'OutputCX=1280', 'OutputCY=720', 'FPSType=0', 'FPSCommon=30', '',
    '[Output]', 'Mode=Advanced', 'Reconnect=true', 'RetryDelay=2', 'MaxRetries=25', '',
    '[AdvOut]', 'Encoder=obs_x264', 'TrackIndex=1', 'ApplyServiceSettings=false', 'Track1Bitrate=160', '',
    '[Audio]', 'SampleRate=48000', 'ChannelSetup=Stereo', '',
  ].join('\n'));
  // OBS's x264 defaults for streaming, but CBR at --bitrate and keyframes
  // every --obs-keyint s (0: OBS's auto).
  fs.writeFileSync(path.join(pdir, 'streamEncoder.json'), JSON.stringify({
    rate_control: 'CBR', bitrate, keyint_sec: obsKeyint, preset: 'veryfast',
  }, null, 4));
  fs.writeFileSync(path.join(pdir, 'service.json'), JSON.stringify({ type: 'rtmp_custom', settings: { ...service(), use_auth: false } }, null, 4));
  const media = crypto.randomUUID(), scene = crypto.randomUUID();
  fs.writeFileSync(path.join(CONFIG, 'basic/scenes', NAME + '.json'), JSON.stringify({
    name: NAME, current_scene: 'Ingest scene', current_program_scene: 'Ingest scene', scene_order: [{ name: 'Ingest scene' }], groups: [], modules: {},
    sources: [
      { id: 'ffmpeg_source', name: 'Ingest clip', uuid: media, versioned_id: 'ffmpeg_source', settings: { local_file: source, is_local_file: true, looping: true, restart_on_activate: true, close_when_inactive: false },
        mixers: 255, enabled: true, muted: false, 'push-to-mute': false, 'push-to-talk': false, volume: 1.0, balance: 0.5, monitoring_type: 0, hotkeys: {}, filters: [] },
      { id: 'scene', name: 'Ingest scene', uuid: scene, versioned_id: 'scene', settings: { id_counter: 1, items: [{ name: 'Ingest clip', source_uuid: media, id: 1, visible: true, locked: false, pos: { x: 0, y: 0 }, scale: { x: 1, y: 1 }, rot: 0, align: 5, bounds_type: 0, bounds_align: 0, bounds: { x: 0, y: 0 }, crop_left: 0, crop_top: 0, crop_right: 0, crop_bottom: 0, scale_filter: 'disable', blend_method: 'default', blend_type: 'normal', show_transition: { duration: 0 }, hide_transition: { duration: 0 }, private_settings: {} }] },
        mixers: 0, enabled: true, muted: false, volume: 1.0, balance: 0.5, hotkeys: {}, filters: [] },
    ],
  }, null, 2));
}

let runs = 0;
function sapflow() {
  runs += 1;
  const env = {
    ...process.env,
    SAPFLOW_HTTP_ADDR: `127.0.0.1:${port.hls}`, SAPFLOW_SRT_ADDR: `127.0.0.1:${port.srt}`, SAPFLOW_RTMP_ADDR: `127.0.0.1:${port.rtmp}`,
    ...(appKey ? {
      SAPFLOW_SRT_APP_KEYS: '1', SAPFLOW_SRT_ENCRYPTION: 'required',
      SAPFLOW_PUBLISH_AUTH_URL: `http://127.0.0.1:${port.hook}/auth`,
    } : {
      SAPFLOW_PUBLISH_USER: USER, SAPFLOW_PUBLISH_PASS: PASS,
      SAPFLOW_SRT_PASSPHRASE: PASSPHRASE, SAPFLOW_SRT_PBKEYLEN: '32', SAPFLOW_SRT_ENCRYPTION: 'optional',
    }),
    SAPFLOW_RECORDINGS: path.join(out, 'recordings'), SAPFLOW_RECORD_MIN_FREE_GB: '0',
    SAPFLOW_SEGMENT_COUNT: '24', SAPFLOW_HLS_MODE: 'whole', SAPFLOW_RECONNECT_GRACE: '30', SAPFLOW_HEALTH_INTERVAL: '0',
    RUST_LOG: 'info',
  };
  const log = fs.openSync(path.join(out, `sapflow-${runs}.log`), 'w');
  return spawn(bin, [], { env, stdio: ['ignore', log, log] });
}

// The app's side of --app-key: admits SRT publishes of this run's path that
// offer to check a passphrase, and returns the passphrase; refuses the rest.
let hookAsked = 0, hookKeyed = 0;
function hook() {
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (c) => { body += c; if (body.length > 65536) req.destroy(); });
    req.on('end', () => {
      hookAsked += 1;
      let j = {};
      try { j = JSON.parse(body); } catch {}
      if (j.protocol === 'srt' && j.srtKey === 'accepted' && j.path === `live/${EVENT}`) {
        hookKeyed += 1;
        res.writeHead(200, { 'sapflow-srt-passphrase': APP_KEY });
      } else {
        res.writeHead(401);
      }
      res.end();
    });
  });
  server.listen(port.hook, '127.0.0.1');
  return server;
}

function stopChild(child, ms = 4000) {
  return new Promise((r) => {
    if (!child || child.exitCode !== null || child.signalCode !== null) return r();
    child.once('exit', () => r());
    try { child.kill('SIGTERM'); } catch { return r(); }
    setTimeout(() => { try { child.kill('SIGKILL'); } catch {} }, ms);
  });
}

async function waitReady(ms) {
  const until = Date.now() + ms;
  while (Date.now() < until) {
    if ((await playlist()).includes('#EXTINF')) return true;
    await pause(250);
  }
  return false;
}

function logFacts(file) {
  const text = fs.existsSync(file) ? fs.readFileSync(file, 'utf8').replace(/\x1b\[[0-9;]*m/g, '') : '';
  const lines = text.split('\n');
  const pick = (re) => lines.filter((l) => re.test(l)).map((l) => l.replace(/^\S+\s+/, '').slice(0, 300));
  return {
    ready_ms: [...text.matchAll(/stream ready path=\S+ ready_ms=(\d+)/g)].map((m) => +m[1]),
    connected: pick(/publisher connected/).length,
    srt_closed: pick(/SRT connection closed/),
    recordings: pick(/recording finished|recording marked incomplete/),
    accepted: pick(/SRT publisher accepted/),
    refused: pick(/publish refused|caller gave up/).slice(0, 10),
    warnings: pick(/\bWARN\b/).filter((l) => !/OPEN_PUBLISH|CALLBACK_URL|three quarters|receive buffer below/.test(l)).slice(0, 20),
    target: pick(/target duration fixed/),
  };
}

function probe(f) {
  const s = spawnSync('ffprobe', ['-v', 'error', '-show_entries', 'stream=codec_type,codec_name,profile,width,height,has_b_frames,r_frame_rate,start_time,duration',
    '-of', 'json', f], { encoding: 'utf8' });
  const p = spawnSync('ffprobe', ['-v', 'error', '-select_streams', 'v:0', '-show_entries', 'packet=pts_time,flags', '-of', 'csv=p=0', f], { encoding: 'utf8' });
  const rows = p.stdout.trim().split('\n').filter(Boolean).map((l) => l.split(',')).map(([t, flags]) => ({ t: +t, key: (flags || '').includes('K') }));
  const keys = rows.filter((r) => r.key).map((r) => r.t);
  const d = spawnSync('ffmpeg', ['-v', 'error', '-i', f, '-f', 'null', '-'], { encoding: 'utf8' });
  return {
    file: path.relative(out, f), streams: (JSON.parse(s.stdout || '{}').streams || []),
    video_frames: rows.length, keyframe_gaps_s: [...new Set(keys.slice(1).map((k, i) => Math.round((k - keys[i]) * 100) / 100))],
    decode_errors: d.stderr.split('\n').filter(Boolean).length,
  };
}

function analyse(info) {
  const facts = [];
  for (let i = 1; i <= runs; i++) facts.push(logFacts(path.join(out, `sapflow-${i}.log`)));
  const recDir = path.join(out, 'recordings');
  const recs = fs.existsSync(recDir) ? fs.readdirSync(recDir, { recursive: true }).map((f) => path.join(recDir, f)).filter((f) => f.endsWith('.mp4')).sort() : [];
  let obsLog = '';
  try { obsLog = fs.readFileSync(path.join(out, 'obs-own-log.txt'), 'utf8'); } catch {}
  const obsLines = obsLog.split('\n').filter((l) => /Reconnect|reconnect|dropped|Output .* stopping|Number of (lagged|skipped)|encoder|Stream output|Connecting|Connection to|failed/i.test(l))
    .map((l) => l.slice(0, 200)).slice(0, 40);
  return { runs: facts, recordings: recs.map(probe), obs_log: obsLines, ...info };
}

(async () => {
  if (obsPids().length) { console.log('OBS is already running (pid ' + obsPids() + '); not touching it'); process.exit(3); }
  const info = { protocol, seconds, obs_keyint: obsKeyint, bitrate, restart_at: restartAt, load: os.loadavg()[0] };
  info.obs_version = execFileSync('/usr/libexec/PlistBuddy', ['-c', 'Print CFBundleShortVersionString', path.join(obsApp, 'Contents/Info.plist')]).toString().trim();
  const source = clip();
  const backup = fs.mkdtempSync(path.join(os.tmpdir(), 'ingest-obs-backup-'));
  execFileSync('rsync', ['-a', CONFIG + '/', backup + '/']);
  let srv = null, obs = null, stub = null;
  info.app_key = appKey ? (appKeyWrong ? 'wrong' : 'right') : null;
  try {
    if (appKey) stub = hook();
    if (!remote) {
      srv = sapflow();
      await pause(1000);
      if (srv.exitCode !== null) throw new Error('sapflow exited at start');
    }
    writeProfile(source);
    if (obsPids().length) throw new Error('OBS started meanwhile; not launching');
    const fd = fs.openSync(path.join(out, 'obs.stdout.log'), 'w');
    const launched = Date.now();
    obs = spawn(OBS, ['--profile', NAME, '--collection', NAME, '--startstreaming', '--disable-shutdown-check', '--websocket_port', String(port.websocket)],
      { stdio: ['ignore', fd, fd], detached: true });
    info.obs_pid = obs.pid;
    info.launched_at = new Date(launched).toISOString();
    if (remote) {
      await pause(seconds * 1000);
      // Whether OBS lasted the run; the server's logs say whether it streamed.
      if (!running(obs.pid)) throw new Error('OBS exited before the end of the run');
      throw Object.assign(new Error('done'), { done: true });
    }
    info.ready = await waitReady(45000);
    info.first_playlist = (await playlist()).split('\n').filter((l) => /^#EXT-X-TARGETDURATION|^#EXTINF/.test(l)).slice(0, 4);
    info.ready_after_launch_s = info.ready ? Math.round((Date.now() - launched) / 100) / 10 : null;
    if (info.ready && restartAt !== null) {
      await pause(restartAt * 1000);
      const stopped = Date.now();
      await stopChild(srv);
      await pause(3000);
      srv = sapflow();
      info.back = await waitReady(60000);
      info.back_after_stop_s = info.back ? Math.round((Date.now() - stopped) / 100) / 10 : null;
    }
    if (info.ready) await pause(seconds * 1000);
    if (!remote) info.extinf_at_end = ((await playlist()).match(/#EXTINF:([\d.]+)/g) || []).map((x) => +x.slice(8));
  } catch (e) { if (!(e && e.done)) info.error = String(e && e.message || e); }
  finally {
    if (obs && running(obs.pid)) {
      try { process.kill(obs.pid, 'SIGTERM'); } catch {}
      for (let i = 0; i < 20 && running(obs.pid); i++) await sleep(500);
      if (running(obs.pid)) { try { process.kill(obs.pid, 'SIGTERM'); } catch {} }
      for (let i = 0; i < 60 && running(obs.pid); i++) await sleep(500);
    }
    if (obs) info.obs_ended = !running(obs.pid);
    await sleep(4000);
    await stopChild(srv);
    if (stub) {
      stub.close();
      info.hook = { asked: hookAsked, keyed: hookKeyed };
    }
    if (interrupted) info.error = 'interrupted';
    redactFile(path.join(out, 'obs.stdout.log'));
    if (obs && running(obs.pid)) {
      // Not restored, but the key is not left in the profile.
      blankProfile();
      info.restore = 'skipped: OBS still running';
    } else {
      const ldir = path.join(CONFIG, 'logs');
      let logs = [];
      try { logs = fs.readdirSync(ldir).filter((f) => f.endsWith('.txt')); } catch {}
      // Every log, so that the Trash copy holds no credentials either.
      for (const f of logs) redactFile(path.join(ldir, f));
      const newest = logs.map((f) => [f, fs.statSync(path.join(ldir, f)).mtimeMs]).sort((a, b) => b[1] - a[1])[0];
      if (newest) fs.copyFileSync(path.join(ldir, newest[0]), path.join(out, 'obs-own-log.txt'));
      blankProfile();
      const modified = trash(CONFIG);
      execFileSync('rsync', ['-a', backup + '/', CONFIG + '/']);
      const d = spawnSync('diff', ['-r', backup, CONFIG]);
      info.restore_diff_exit = d.status;
      info.modified_config_moved_to_trash = path.basename(modified);
      info.backup_moved_to_trash = path.basename(trash(backup));
    }
  }
  let result = info;
  try { result = analyse(info); } catch (e) { info.analyse_error = String(e); }
  fs.writeFileSync(path.join(out, 'result.json'), JSON.stringify(result, null, 1));
  console.log(JSON.stringify(result));
})();
