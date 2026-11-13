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
if (!bin || !out || !['srt', 'rtmp'].includes(protocol)) {
  console.error('--bin, --out and --protocol srt|rtmp are required'); process.exit(2);
}
const seconds = +opt('seconds', 60), base = +opt('base', 47400);
const obsKeyint = +opt('obs-keyint', 1), bitrate = +opt('bitrate', 2500);
const restartAt = opt('restart-at') === undefined ? null : +opt('restart-at');
const obsApp = opt('obs', '/Applications/OBS.app');
const OBS = path.join(obsApp, 'Contents/MacOS/OBS');
const NAME = 'Ingest-OBS-test';
const CONFIG = path.join(os.homedir(), 'Library/Application Support/obs-studio');
const port = { hls: base, srt: base + 1, rtmp: base + 2, websocket: base + 9 };
const EVENT = 'event-obs';
const USER = 'obs', PASS = crypto.randomBytes(12).toString('hex');
const PASSPHRASE = 'obs-' + crypto.randomBytes(12).toString('hex');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
fs.mkdirSync(out, { recursive: true });

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
    SAPFLOW_PUBLISH_USER: USER, SAPFLOW_PUBLISH_PASS: PASS,
    SAPFLOW_SRT_PASSPHRASE: PASSPHRASE, SAPFLOW_SRT_PBKEYLEN: '32', SAPFLOW_SRT_ENCRYPTION: 'optional',
    SAPFLOW_RECORDINGS: path.join(out, 'recordings'), SAPFLOW_RECORD_MIN_FREE_GB: '0',
    SAPFLOW_SEGMENT_COUNT: '24', SAPFLOW_HLS_MODE: 'whole', SAPFLOW_RECONNECT_GRACE: '30', SAPFLOW_HEALTH_INTERVAL: '0',
    RUST_LOG: 'info',
  };
  const log = fs.openSync(path.join(out, `sapflow-${runs}.log`), 'w');
  return spawn(bin, [], { env, stdio: ['ignore', log, log] });
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
    await sleep(250);
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
  const obsLines = obsLog.split('\n').filter((l) => /Reconnect|reconnect|dropped|Output .* stopping|Number of (lagged|skipped)|encoder|Stream output/i.test(l))
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
  let srv = null, obs = null;
  try {
    srv = sapflow();
    await sleep(1000);
    if (srv.exitCode !== null) throw new Error('sapflow exited at start');
    writeProfile(source);
    if (obsPids().length) throw new Error('OBS started meanwhile; not launching');
    const fd = fs.openSync(path.join(out, 'obs.stdout.log'), 'w');
    const launched = Date.now();
    obs = spawn(OBS, ['--profile', NAME, '--collection', NAME, '--startstreaming', '--disable-shutdown-check', '--websocket_port', String(port.websocket)],
      { stdio: ['ignore', fd, fd], detached: true });
    info.obs_pid = obs.pid;
    info.ready = await waitReady(45000);
    info.first_playlist = (await playlist()).split('\n').filter((l) => /^#EXT-X-TARGETDURATION|^#EXTINF/.test(l)).slice(0, 4);
    info.ready_after_launch_s = info.ready ? Math.round((Date.now() - launched) / 100) / 10 : null;
    if (info.ready && restartAt !== null) {
      await sleep(restartAt * 1000);
      const stopped = Date.now();
      await stopChild(srv);
      await sleep(3000);
      srv = sapflow();
      info.back = await waitReady(60000);
      info.back_after_stop_s = info.back ? Math.round((Date.now() - stopped) / 100) / 10 : null;
    }
    if (info.ready) await sleep(seconds * 1000);
    info.extinf_at_end = ((await playlist()).match(/#EXTINF:([\d.]+)/g) || []).map((x) => +x.slice(8));
  } catch (e) { info.error = String(e && e.message || e); }
  finally {
    if (obs && running(obs.pid)) {
      try { process.kill(obs.pid, 'SIGTERM'); } catch {}
      for (let i = 0; i < 20 && running(obs.pid); i++) await sleep(500);
      if (running(obs.pid)) { try { process.kill(obs.pid, 'SIGTERM'); } catch {} }
      for (let i = 0; i < 60 && running(obs.pid); i++) await sleep(500);
      info.obs_ended = !running(obs.pid);
    }
    await sleep(4000);
    await stopChild(srv);
    if (obs && running(obs.pid)) { info.restore = 'skipped: OBS still running'; }
    else {
      try {
        const ldir = path.join(CONFIG, 'logs');
        const newest = fs.readdirSync(ldir).filter((f) => f.endsWith('.txt')).map((f) => [f, fs.statSync(path.join(ldir, f)).mtimeMs]).sort((a, b) => b[1] - a[1])[0];
        if (newest) fs.copyFileSync(path.join(ldir, newest[0]), path.join(out, 'obs-own-log.txt'));
      } catch {}
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
