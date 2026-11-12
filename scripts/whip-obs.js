'use strict';
// A3.10 for WHIP: OBS's WHIP output against sapflow on loopback. Does it
// answer PLI with a keyframe, and does it change its sending rate on REMB
// or on receiver reports? Dependency-free Node 24; needs ffmpeg, ffprobe
// and OBS (macOS app). Stops only what it started, by PID.
//
//   node scripts/whip-obs.js --bin <sapflow> --out <dir> [--seconds 40]
//        [--keyframe-interval 2] [--obs-keyint 10] [--bitrate 2500]
//        [--feedback <SAPFLOW_TEST_WHIP_FEEDBACK value>] [--base 47300]
//        [--proxy <srt-loss-proxy> --loss <percent>] [--obs /Applications/OBS.app]
//
// In order, as W0's obs-run.js:
//   1. refuses to run if OBS is already running (never touches the user's
//      session);
//   2. copies OBS's whole config directory to a scratch backup;
//   3. writes a dedicated profile and scene collection, "A310-WHIP-test":
//      Advanced output, x264 CBR at --bitrate kbit/s, its own keyframe
//      interval --obs-keyint s, no B-frames, 1280x720 at 30 fps, and one
//      media source looping a generated clip (testsrc2 and a tone), with the
//      WHIP custom service pointing at sapflow with a bearer token;
//   4. starts sapflow (WHIP on 127.0.0.1, SAPFLOW_WHIP_KEYFRAME_INTERVAL,
//      sapflow::whip at debug, recording), then OBS with --profile,
//      --collection and --startstreaming;
//   5. after --seconds of media, ends OBS by its own PID (SIGTERM; OBS 32
//      takes the first as "stop streaming", so a second one closes it), then
//      sapflow;
//   6. puts the config directory back from the backup, whatever happened
//      before, and diffs it against the backup.
//
// --proxy and --loss put the loss proxy (crates/sapflow-bench, --protocol
// whip: STUN and DTLS pass, SRTP and SRTCP are dropped at random, 30 ms
// each way, seed 1) between OBS and sapflow, and advertise it as the only
// candidate, to see whether OBS answers NACKs: sapflow's receive counters
// (at debug) and the proxy's repeated RTP.
//
// --feedback passes SAPFLOW_TEST_WHIP_FEEDBACK to a sapflow built with
// --features test-feedback (loss:<fraction of 256>:<s> or
// remb:<bit/s>:<s>; see the server's whip_feedback module).
//
// Results in --out: sapflow.log, the recording, the OBS log, and
// result.json: each keyframe request (PLI) and each keyframe's arrival,
// from the server's debug log, the delay from each request to the next
// keyframe, the recording's keyframe times, and its video rate a second
// (from ffprobe's packet sizes).
const { spawn, spawnSync, execFileSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const args = process.argv.slice(2);
const opt = (n, d) => { const i = args.indexOf('--' + n); return i >= 0 ? args[i + 1] : d; };
const bin = opt('bin'), out = opt('out');
if (!bin || !out) { console.error('--bin and --out are required'); process.exit(2); }
const seconds = +opt('seconds', 40), base = +opt('base', 47300);
const keyframeInterval = opt('keyframe-interval', '2'), obsKeyint = +opt('obs-keyint', 10), bitrate = +opt('bitrate', 2500);
const feedback = opt('feedback');
const proxyBin = opt('proxy'), lossPct = opt('loss');
const obsApp = opt('obs', '/Applications/OBS.app');
const OBS = path.join(obsApp, 'Contents/MacOS/OBS');
const NAME = 'A310-WHIP-test';
const CONFIG = path.join(os.homedir(), 'Library/Application Support/obs-studio');
const port = { hls: base, whip: base + 1, media: base + 2, srt: base + 3, proxy: base + 4, websocket: base + 9 };
const EVENT = 'event-obs';
const TOKEN = 'obs:' + crypto.randomBytes(12).toString('hex');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
fs.mkdirSync(out, { recursive: true });

function trash(p) {
  const dest = path.join(os.homedir(), '.Trash', path.basename(p) + '-' + Date.now());
  fs.renameSync(p, dest); return dest;
}
function running(pid) { try { process.kill(pid, 0); return true; } catch { return false; } }
function obsPids() { const r = spawnSync('pgrep', ['-x', 'OBS']); return r.status === 0 ? r.stdout.toString().trim().split('\n').map(Number) : []; }

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

function writeProfile(server, source) {
  const pdir = path.join(CONFIG, 'basic/profiles', NAME);
  fs.mkdirSync(pdir, { recursive: true });
  fs.writeFileSync(path.join(pdir, 'basic.ini'), [
    '[General]', `Name=${NAME}`, '',
    '[Video]', 'BaseCX=1280', 'BaseCY=720', 'OutputCX=1280', 'OutputCY=720', 'FPSType=0', 'FPSCommon=30', '',
    '[Output]', 'Mode=Advanced', '',
    '[AdvOut]', 'Encoder=obs_x264', 'TrackIndex=1', 'ApplyServiceSettings=false', '',
    '[Audio]', 'SampleRate=48000', 'ChannelSetup=Stereo', '',
  ].join('\n'));
  fs.writeFileSync(path.join(pdir, 'streamEncoder.json'), JSON.stringify({
    rate_control: 'CBR', bitrate, keyint_sec: obsKeyint, preset: 'veryfast', profile: 'baseline', tune: 'zerolatency', x264opts: 'bframes=0',
  }, null, 4));
  fs.writeFileSync(path.join(pdir, 'service.json'), JSON.stringify({ type: 'whip_custom', settings: { server, bearer_token: TOKEN, service: 'WHIP' } }, null, 4));
  const media = crypto.randomUUID(), scene = crypto.randomUUID();
  fs.writeFileSync(path.join(CONFIG, 'basic/scenes', NAME + '.json'), JSON.stringify({
    name: NAME, current_scene: 'A310 scene', current_program_scene: 'A310 scene', scene_order: [{ name: 'A310 scene' }], groups: [], modules: {},
    sources: [
      { id: 'ffmpeg_source', name: 'A310 clip', uuid: media, versioned_id: 'ffmpeg_source', settings: { local_file: source, is_local_file: true, looping: true, restart_on_activate: true, close_when_inactive: false },
        mixers: 255, enabled: true, muted: false, 'push-to-mute': false, 'push-to-talk': false, volume: 1.0, balance: 0.5, monitoring_type: 0, hotkeys: {}, filters: [] },
      { id: 'scene', name: 'A310 scene', uuid: scene, versioned_id: 'scene', settings: { id_counter: 1, items: [{ name: 'A310 clip', source_uuid: media, id: 1, visible: true, locked: false, pos: { x: 0, y: 0 }, scale: { x: 1, y: 1 }, rot: 0, align: 5, bounds_type: 0, bounds_align: 0, bounds: { x: 0, y: 0 }, crop_left: 0, crop_top: 0, crop_right: 0, crop_bottom: 0, scale_filter: 'disable', blend_method: 'default', blend_type: 'normal', show_transition: { duration: 0 }, hide_transition: { duration: 0 }, private_settings: {} }] },
        mixers: 0, enabled: true, muted: false, volume: 1.0, balance: 0.5, hotkeys: {}, filters: [] },
    ],
  }, null, 2));
}

function sapflow() {
  const env = {
    ...process.env,
    SAPFLOW_HTTP_ADDR: `127.0.0.1:${port.hls}`, SAPFLOW_SRT_ADDR: `127.0.0.1:${port.srt}`, SAPFLOW_RTMP_ADDR: 'off',
    SAPFLOW_PUBLISH_USER: TOKEN.split(':')[0], SAPFLOW_PUBLISH_PASS: TOKEN.split(':')[1],
    SAPFLOW_RECORDINGS: path.join(out, 'recordings'), SAPFLOW_RECORD_MIN_FREE_GB: '0', SAPFLOW_RECONNECT_GRACE: '2',
    SAPFLOW_WHIP_ADDR: `127.0.0.1:${port.whip}`, SAPFLOW_WHIP_UDP_ADDR: `127.0.0.1:${port.media}`,
    SAPFLOW_WHIP_CANDIDATES: `udp 127.0.0.1:${proxyBin ? port.proxy : port.media}`, SAPFLOW_WHIP_PUBLIC_URL: `http://127.0.0.1:${port.whip}`,
    SAPFLOW_WHIP_KEYFRAME_INTERVAL: keyframeInterval,
    RUST_LOG: 'info,sapflow::whip=debug',
  };
  if (feedback) env.SAPFLOW_TEST_WHIP_FEEDBACK = feedback;
  const log = fs.openSync(path.join(out, 'sapflow.log'), 'w');
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

// Log lines' times (seconds since the session connected) for a message.
function times(text, message, t0) {
  const re = new RegExp(`^(\\S+)Z\\s+\\w+\\s+\\S+ ${message}(.*)$`);
  const outT = [];
  for (const line of text.split('\n')) {
    const m = re.exec(line.replace(/\x1b\[[0-9;]*m/g, ''));
    if (m) outT.push({ t: Math.round((Date.parse(m[1] + 'Z') - t0) / 1) / 1000, rest: m[2].trim() });
  }
  return outT;
}

function analyse() {
  const text = fs.readFileSync(path.join(out, 'sapflow.log'), 'utf8').replace(/\x1b\[[0-9;]*m/g, '');
  const conn = /^(\S+)Z\s+\w+\s+\S+ WHIP session connected/m.exec(text);
  const t0 = conn ? Date.parse(conn[1] + 'Z') : NaN;
  const requests = times(text, 'WHIP keyframe requested', t0);
  const keyframes = times(text, 'WHIP keyframe received', t0);
  // Each request, and the first keyframe after it.
  const answered = requests.map((r) => {
    const k = keyframes.find((x) => x.t >= r.t);
    return { request_s: r.t, next_keyframe_s: k ? k.t : null, delay_ms: k ? Math.round((k.t - r.t) * 1000) : null };
  });
  const gaps = keyframes.slice(1).map((k, i) => Math.round((k.t - keyframes[i].t) * 1000) / 1000);
  const recDir = path.join(out, 'recordings');
  const recs = fs.existsSync(recDir) ? fs.readdirSync(recDir, { recursive: true }).map((f) => path.join(recDir, f)).filter((f) => f.endsWith('.mp4')) : [];
  const recordings = recs.map((f) => {
    const p = spawnSync('ffprobe', ['-v', 'error', '-select_streams', 'v:0', '-show_entries', 'packet=pts_time,size,flags', '-of', 'csv=p=0', f], { encoding: 'utf8' });
    const rows = p.stdout.trim().split('\n').filter(Boolean).map((l) => l.split(',')).map(([t, size, flags]) => ({ t: +t, size: +size, key: (flags || '').includes('K') }));
    const perSecond = {};
    for (const r of rows) { const s = Math.floor(r.t); perSecond[s] = (perSecond[s] || 0) + r.size * 8; }
    return {
      file: path.basename(f), frames: rows.length,
      keyframes_s: rows.filter((r) => r.key).map((r) => Math.round(r.t * 1000) / 1000),
      video_kbps: Object.keys(perSecond).sort((a, b) => a - b).map((s) => [+s, Math.round(perSecond[s] / 1000)]),
    };
  });
  const counters = [...text.matchAll(/WHIP receive counters (.*)$/gm)].pop();
  const proxyStats = fs.existsSync(path.join(out, 'proxy.txt')) ? Object.fromEntries(fs.readFileSync(path.join(out, 'proxy.txt'), 'utf8').trim().split('\n').map((l) => l.split('='))) : null;
  return { counters: counters ? Object.fromEntries([...counters[1].matchAll(/(\w+)=(\d+)/g)].map((m) => [m[1], +m[2]])) : null, proxy: proxyStats,
    connected: conn ? conn[1] + 'Z' : null, requests: requests.length, keyframes_received: keyframes.length, answered, keyframe_gaps_s: gaps,
    aligned: (/WHIP timeline aligned alignment=(\S+)/.exec(text) || [])[1] || null, feedback_armed: /test feedback hooks armed/.test(text), recordings };
}

(async () => {
  if (obsPids().length) { console.log('OBS is already running (pid ' + obsPids() + '); not touching it'); process.exit(3); }
  const info = { seconds, keyframe_interval: keyframeInterval, obs_keyint: obsKeyint, bitrate, feedback: feedback || null, load: os.loadavg()[0] };
  info.obs_version = execFileSync('/usr/libexec/PlistBuddy', ['-c', 'Print CFBundleShortVersionString', path.join(obsApp, 'Contents/Info.plist')]).toString().trim();
  const source = clip();
  const backup = fs.mkdtempSync(path.join(os.tmpdir(), 'a310-obs-backup-'));
  execFileSync('rsync', ['-a', CONFIG + '/', backup + '/']);
  let srv = null, obs = null, proxy = null;
  try {
    srv = sapflow();
    await sleep(1000);
    if (proxyBin) {
      proxy = spawn(proxyBin, ['--listen', `127.0.0.1:${port.proxy}`, '--target', `127.0.0.1:${port.media}`, '--loss', `random:${lossPct || 5}`,
        '--delay-ms', '30', '--protocol', 'whip', '--seed', '1', '--stats', path.join(out, 'proxy.txt')], { stdio: ['ignore', 'ignore', fs.openSync(path.join(out, 'proxy.log'), 'w')] });
      info.proxy_loss_pct = +(lossPct || 5);
    }
    if (srv.exitCode !== null) throw new Error('sapflow exited at start');
    writeProfile(`http://127.0.0.1:${port.whip}/whip/live/${EVENT}`, source);
    if (obsPids().length) throw new Error('OBS started meanwhile; not launching');
    const fd = fs.openSync(path.join(out, 'obs.stdout.log'), 'w');
    obs = spawn(OBS, ['--profile', NAME, '--collection', NAME, '--startstreaming', '--disable-shutdown-check', '--websocket_port', String(port.websocket)],
      { stdio: ['ignore', fd, fd], detached: true });
    info.obs_pid = obs.pid;
    // Media for --seconds after the session connects (at most 30 s to connect).
    const until = Date.now() + 30000;
    while (Date.now() < until && !/WHIP session connected/.test(fs.readFileSync(path.join(out, 'sapflow.log'), 'utf8'))) await sleep(500);
    info.connected = /WHIP session connected/.test(fs.readFileSync(path.join(out, 'sapflow.log'), 'utf8'));
    if (info.connected) await sleep(seconds * 1000);
  } catch (e) { info.error = String(e && e.message || e); }
  finally {
    if (obs && running(obs.pid)) {
      try { process.kill(obs.pid, 'SIGTERM'); } catch {}
      for (let i = 0; i < 20 && running(obs.pid); i++) await sleep(500);
      info.second_sigterm = running(obs.pid);
      if (running(obs.pid)) { try { process.kill(obs.pid, 'SIGTERM'); } catch {} }
      for (let i = 0; i < 60 && running(obs.pid); i++) await sleep(500);
      info.obs_ended = !running(obs.pid);
    }
    await sleep(4000); // the recording finishes when the grace is over
    await stopChild(proxy);
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
      info.restore_diff = (d.stdout.toString() + d.stderr.toString()).slice(0, 2000);
      info.restore_rsync_dry_run = spawnSync('rsync', ['-an', '--itemize-changes', '--delete', '--checksum', backup + '/', CONFIG + '/']).stdout.toString().trim();
      info.modified_config_moved_to_trash = path.basename(modified);
      info.backup_moved_to_trash = path.basename(trash(backup));
    }
  }
  try { Object.assign(info, analyse()); } catch (e) { info.analyse_error = String(e); }
  fs.writeFileSync(path.join(out, 'result.json'), JSON.stringify(info, null, 1));
  const short = { ...info, recordings: (info.recordings || []).map((r) => ({ file: r.file, frames: r.frames, keyframes_s: r.keyframes_s })) };
  console.log(JSON.stringify(short));
})();
