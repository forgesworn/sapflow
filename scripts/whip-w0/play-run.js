'use strict';
// A0.2 playback matrix. Through run.js:
//   node run.js play --assets ~/whip-w0-assets --vendor <dir with hls.min.js> --out <dir> --range 47000-47199
//        [--browsers chrome,firefox,safari] [--seconds 14] [--only <label-substring>]
// Each case is one fresh browser launch (temporary profile) on one page load.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { launchChrome, launchFirefox, launchSafari, sleep } = require('./browser.js');
const { startPlayServer } = require('./play-server.js');

const ASSETS = {
  'a-av': '/assets/av/master.m3u8', 'b-audio': '/assets/audio/master.m3u8', 'c-recording': '/assets/recording/recording.mp4', 'd-av-preskip312': '/assets/av-preskip312/master.m3u8',
};
function cases(browser) {
  const c = [];
  for (const a of ['a-av', 'b-audio', 'd-av-preskip312']) c.push({ browser, player: 'hlsjs', asset: a });
  c.push({ browser, player: 'native', asset: 'c-recording' });
  for (const a of ['a-av', 'b-audio']) c.push({ browser, player: 'native', asset: a }); // native HLS: Safari's path; informative for the others
  return c;
}

async function main({ opt, range, out }) {
  const assetsDir = path.resolve(opt('assets', path.join(os.homedir(), 'whip-w0-assets')));
  const vendorDir = path.resolve(opt('vendor'));
  const seconds = +opt('seconds', 14);
  const only = opt('only', '');
  const browsers = opt('browsers', 'chrome,firefox').split(',');
  fs.mkdirSync(out, { recursive: true });
  const srv = await startPlayServer({ assetsDir, vendorDir, outDir: out, range });
  console.log('play server on', srv.port);
  const summary = [];
  for (const b of browsers) {
    for (const c of cases(b)) {
      const label = `${b}-${c.player}-${c.asset}`;
      if (only && !label.includes(only)) continue;
      const load0 = os.loadavg()[0];
      const url = `http://127.0.0.1:${srv.port}/play.html?src=${encodeURIComponent(ASSETS[c.asset])}&player=${c.player}&label=${label}&seconds=${seconds}${b === 'safari' ? '&gesture=1' : ''}`;
      let h;
      try { h = b === 'chrome' ? launchChrome(url) : b === 'firefox' ? launchFirefox(url) : await launchSafari(url, { range }); }
      catch (e) { console.log(label, 'launch failed:', e.message); summary.push({ label, launchError: e.message }); continue; }
      if (b === 'safari') { await sleep(2000); await h.click('#go'); }
      const ok = await srv.waitFor(label, (seconds + 40) * 1000);
      const load1 = os.loadavg()[0];
      await h.stop();
      let r = null; try { r = JSON.parse(fs.readFileSync(path.join(out, label + '.json'), 'utf8')); } catch {}
      const a = r && r.audio || {};
      const row = { label, finished: ok, load: [+load0.toFixed(1), +load1.toFixed(1)], advanced10: r && r.currentTimeAdvancedAtLeast10, tEnd: r && r.currentTimeEnd, mediaError: r && r.mediaError, errors: r && r.errors, hlsFatal: r && r.hlsEvents.filter((e) => e[0] === 'hls-error' && e[4]).length,
        audioBytes: r && r.audioDecodedBytesEnd, audible: a.audible, firstAudibleMs: a.firstAudibleMs, hz: [a.steadyLeftHz, a.steadyRightHz], stepFirst: a.maxStepFirstBlocks, stepSteady: a.maxStepSteady, clicks: a.clicks && a.clicks.map((c) => c[1] + 'ms').join(' '), silentAfter: a.silentBlocksAfterOnset };
      console.log(JSON.stringify(row)); summary.push(row);
    }
  }
  fs.writeFileSync(path.join(out, 'play-summary.json'), JSON.stringify({ requestsSample: srv.requests.slice(0, 20), summary }, null, 1));
  srv.close();
}
module.exports = { main };
