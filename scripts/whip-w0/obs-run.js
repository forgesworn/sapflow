'use strict';
// OBS WHIP capture (A0.1, A0.3, A0.5). Usage, through run.js:
//   node run.js obs udp|tcp --host <lan-ip> --out <dir> [--range 47000-47199] [--port N] [--obs /Applications/OBS.app]
// What it does, in order:
//   1. refuses to run if OBS is already running (we never touch the user's session);
//   2. copies OBS's whole config directory to a scratch backup;
//   3. makes a dedicated profile and scene collection, "W0-WHIP-test", whose service is the
//      WHIP custom service pointing at our fake endpoint with a bearer token;
//   4. launches OBS with --profile, --collection and --startstreaming and waits for the DTLS
//      flight (or a timeout);
//   5. ends OBS by its own PID (SIGTERM, then waits);
//   6. puts the config directory back from the backup and diffs it against the backup.
// The new files are in the config directory only until step 6.
const { spawn, execFileSync, spawnSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { start } = require('./fake-whip.js');

const NAME = 'W0-WHIP-test';
const CONFIG = path.join(os.homedir(), 'Library/Application Support/obs-studio');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const uuid = () => crypto.randomUUID();

function trash(p) {
  const dest = path.join(os.homedir(), '.Trash', path.basename(p) + '-' + Date.now());
  fs.renameSync(p, dest); return dest;
}
function running(pid) { try { process.kill(pid, 0); return true; } catch { return false; } }
function obsPids() { const r = spawnSync('pgrep', ['-x', 'OBS']); return r.status === 0 ? r.stdout.toString().trim().split('\n').map(Number) : []; }

function writeProfile(server, token) {
  const pdir = path.join(CONFIG, 'basic/profiles', NAME);
  fs.mkdirSync(pdir, { recursive: true });
  fs.writeFileSync(path.join(pdir, 'basic.ini'), [
    '[General]', `Name=${NAME}`, '', '[Video]', 'BaseCX=1280', 'BaseCY=720', 'OutputCX=1280', 'OutputCY=720', 'FPSType=0', 'FPSCommon=30', '',
    '[Output]', 'Mode=Simple', '', '[SimpleOutput]', 'VBitrate=2500', 'ABitrate=128', 'StreamEncoder=x264', 'Preset=veryfast', '', '[Audio]', 'SampleRate=48000', 'ChannelSetup=Stereo', '',
  ].join('\n'));
  fs.writeFileSync(path.join(pdir, 'service.json'), JSON.stringify({ type: 'whip_custom', settings: { server, bearer_token: token, service: 'WHIP' } }, null, 4));
  const colour = uuid(), scene = uuid();
  fs.writeFileSync(path.join(CONFIG, 'basic/scenes', NAME + '.json'), JSON.stringify({
    name: NAME, current_scene: 'W0 scene', current_program_scene: 'W0 scene', scene_order: [{ name: 'W0 scene' }], groups: [], modules: {},
    sources: [
      { id: 'color_source_v3', name: 'W0 colour', uuid: colour, versioned_id: 'color_source_v3', settings: { color: 4278222848, width: 1280, height: 720 }, mixers: 0, enabled: true, muted: false, 'push-to-mute': false, 'push-to-talk': false, volume: 1.0, balance: 0.5, monitoring_type: 0, hotkeys: {}, filters: [] },
      { id: 'scene', name: 'W0 scene', uuid: scene, versioned_id: 'scene', settings: { id_counter: 1, items: [{ name: 'W0 colour', source_uuid: colour, id: 1, visible: true, locked: false, pos: { x: 0, y: 0 }, scale: { x: 1, y: 1 }, rot: 0, align: 5, bounds_type: 0, bounds_align: 0, bounds: { x: 0, y: 0 }, crop_left: 0, crop_top: 0, crop_right: 0, crop_bottom: 0, scale_filter: 'disable', blend_method: 'default', blend_type: 'normal', show_transition: { duration: 0 }, hide_transition: { duration: 0 }, private_settings: {} }] }, mixers: 0, enabled: true, muted: false, volume: 1.0, balance: 0.5, hotkeys: {}, filters: [] },
    ],
  }, null, 2));
}

async function main({ opt, range, out, mode }) {
  const obsApp = opt('obs', '/Applications/OBS.app');
  const bin = path.join(obsApp, 'Contents/MacOS/OBS');
  if (obsPids().length) { console.log('OBS is already running (pid ' + obsPids() + '); not touching it'); return; }
  fs.mkdirSync(out, { recursive: true });
  const info = {};
  info.obsVersion = execFileSync('/usr/libexec/PlistBuddy', ['-c', 'Print CFBundleShortVersionString', path.join(obsApp, 'Contents/Info.plist')]).toString().trim();
  const backup = fs.mkdtempSync(path.join(os.tmpdir(), 'w0-obs-backup-'));
  execFileSync('rsync', ['-a', CONFIG + '/', backup + '/']);
  info.backup = 'made'; info.backupFiles = execFileSync('find', [backup, '-type', 'f']).toString().trim().split('\n').length;
  const label = `obs-${mode}`;
  const srv = await start({ mode, host: opt('host'), advertise: opt('advertise'), outDir: out, label, relayPort: +opt('port', 0), range, udpPort: 0 });
  console.log(`label=${label} http=${srv.httpPort} udp=${srv.udpPort} tcp-relay=${srv.tcpPort} load=${os.loadavg().map((x) => x.toFixed(1))}`);
  writeProfile(`http://127.0.0.1:${srv.httpPort}/whip?label=${label}`, 'w0-test-token');
  const log = path.join(out, `${label}.obs.stdout.log`);
  const fd = fs.openSync(log, 'w');
  const wsPort = range ? range[1] : 4455; // obs-websocket stays inside our range
  const child = spawn(bin, ['--profile', NAME, '--collection', NAME, '--startstreaming', '--disable-shutdown-check', '--websocket_port', String(wsPort), ...(opt('obs-extra') ? opt('obs-extra').split(' ') : [])], { stdio: ['ignore', fd, fd], detached: true });
  info.pid = child.pid;
  let exited = false; child.on('exit', (c, s) => { exited = true; info.exit = { code: c, signal: s }; });
  const deadline = Date.now() + (+opt('timeout', 45)) * 1000;
  let settled = 0;
  while (Date.now() < deadline && !exited) {
    await sleep(1000);
    if (srv.ev.dtls.datagrams > 0 || srv.ev.stun.valid > 0) { if (++settled >= 8) break; } // 8 s to collect the whole first flight
  }
  info.startedAndWasAlive = !exited;
  info.obsPidsDuring = obsPids();
  console.log('events:', JSON.stringify({ stun: srv.ev.stun, dtls: srv.ev.dtls, whip: srv.ev.whipRequests.length, patch: srv.ev.whipPatch, tcp: srv.ev.tcpConnections }));
  srv.close();
  // End OBS by its own PID: SIGTERM, then wait.
  // OBS 32 treats the first SIGTERM as "stop streaming" and stays open; a second one closes it.
  if (!exited) { try { process.kill(child.pid, 'SIGTERM'); } catch {} }
  for (let i = 0; i < 30 && running(child.pid); i++) await sleep(500);
  info.secondSigtermNeeded = running(child.pid);
  if (running(child.pid)) { try { process.kill(child.pid, 'SIGTERM'); } catch {} }
  for (let i = 0; i < 60 && running(child.pid); i++) await sleep(500);
  info.obsEndedAfterSigterm = !running(child.pid);
  info.obsPidsAfter = obsPids().filter((p) => p === child.pid);
  if (running(child.pid)) { console.log('OBS still alive after 30 s'); info.stillAlive = true; }
  else {
    // Keep OBS's own log in scratch (not the repository), then restore the config exactly.
    try {
      const ldir = path.join(CONFIG, 'logs'); const newest = fs.readdirSync(ldir).filter((f) => f.endsWith('.txt')).map((f) => [f, fs.statSync(path.join(ldir, f)).mtimeMs]).sort((a, b) => b[1] - a[1])[0];
      if (newest) fs.copyFileSync(path.join(ldir, newest[0]), path.join(out, `${label}.obs-own-log.txt`));
    } catch {}
    const modified = trash(CONFIG);
    execFileSync('rsync', ['-a', backup + '/', CONFIG + '/']);
    const d = spawnSync('diff', ['-r', backup, CONFIG]);
    info.restoreDiffExit = d.status; info.restoreDiff = d.stdout.toString() + d.stderr.toString();
    const meta = spawnSync('rsync', ['-an', '--itemize-changes', '--delete', '--checksum', backup + '/', CONFIG + '/']);
    info.restoreRsyncDryRun = meta.stdout.toString().trim();
    info.modifiedConfigMovedTo = path.basename(modified);
    info.backupMovedTo = path.basename(trash(backup));
  }
  info.events = srv.ev;
  fs.writeFileSync(path.join(out, `${label}-run.json`), JSON.stringify(info, null, 2));
  console.log(JSON.stringify({ ...info, events: undefined }));
}
module.exports = { main };
