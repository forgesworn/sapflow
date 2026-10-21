#!/usr/bin/env node
// Plays an LL-HLS stream with hls.js (lowLatencyMode) in a headless browser
// and records how far behind the live edge the playhead sits. Several
// sources play at once, each in its own browser context, so that a loaded
// machine slows them alike.
//
//   PLAYWRIGHT_DIR=<playwright package dir> HLS_JS=<hls.min.js> \
//     node scripts/behind-caddy-player.mjs --src direct=URL/index.m3u8 --src front=URL/index.m3u8 [--seconds 180] [--runs 1] [--no-h3]
//
// Uses a Playwright package and an installed browser that are already on the
// machine (CHROME_CHANNEL, default chrome); nothing is downloaded. The page
// is an empty one served on 127.0.0.1:PLAYER_PORT (default 28944) and the
// player is loaded into it, so every request is cross-origin and relies on
// the server's CORS header, as a player on another site would. Certificate
// errors are ignored for this browser only.
//
// With --runs N the whole thing repeats with fresh players, so start-up
// differences (hls.js starts wherever the playlist happens to be) show as a
// spread and a summary of the runs follows. Reported per source and run,
// after the first 20 s (start-up):
//   hls.js latency   hls.latency: the live edge of the playlist as hls.js
//                    last loaded it, minus the playhead. Includes the
//                    playlist's reload interval; excludes the encoder, the
//                    network to the server and the server itself.
//   wall clock       Date.now() minus the programme date-time of the frame
//                    being shown (EXT-X-PROGRAM-DATE-TIME, stamped by the
//                    server when the segment started). Same clock for server
//                    and browser here; includes the part being filled.
//   buffer ahead     media buffered beyond the playhead.
//   per minute       the median hls.js latency in each minute: hls.js does
//                    not speed up to catch the edge, so every stall adds.
//   protocols        the HTTP version of every response, from the browser.
import fs from "node:fs";
import http from "node:http";
import { createRequire } from "node:module";
import os from "node:os";

const argv = process.argv.slice(2);
const all = (name) => argv.flatMap((a, i) => (a === `--${name}` ? [argv[i + 1]] : []));
const sources = all("src").map((s) => {
  const [label, ...rest] = s.split("=");
  return rest.length ? { label, url: rest.join("=") } : { label: "stream", url: s };
});
const seconds = Number(all("seconds")[0] ?? 180);
const runs = Number(all("runs")[0] ?? 1);
const noH3 = argv.includes("--no-h3");
if (!sources.length || !process.env.PLAYWRIGHT_DIR || !process.env.HLS_JS) {
  console.error(fs.readFileSync(new URL(import.meta.url), "utf8").split("\n").slice(1, 11).map((l) => l.replace(/^\/\/ ?/, "")).join("\n"));
  process.exit(2);
}
const { chromium } = createRequire(import.meta.url)(process.env.PLAYWRIGHT_DIR);

const pct = (xs, p) => [...xs].sort((a, b) => a - b)[Math.min(xs.length - 1, Math.floor(xs.length * p))];
const stats = (xs, unit = "s") =>
  xs.length
    ? `median ${pct(xs, 0.5).toFixed(2)} ${unit}, p10 ${pct(xs, 0.1).toFixed(2)}, p90 ${pct(xs, 0.9).toFixed(2)}, min ${Math.min(...xs).toFixed(2)}, max ${Math.max(...xs).toFixed(2)} (n=${xs.length})`
    : "no samples";
const load = () => `${os.loadavg()[0].toFixed(1)} on ${os.cpus().length} cores`;

const pageServer = http.createServer((_req, res) => res.end("<!doctype html><title>player</title>"));
await new Promise((r) => pageServer.listen(Number(process.env.PLAYER_PORT ?? 28944), "127.0.0.1", r));
const pageUrl = `http://127.0.0.1:${pageServer.address().port}/`;

const browser = await chromium.launch({
  channel: process.env.CHROME_CHANNEL || "chrome",
  headless: true,
  args: ["--autoplay-policy=no-user-gesture-required", ...(noH3 ? ["--disable-quic"] : [])],
});
let failed = false;
const results = {}; // label -> per-run medians

async function playOnce(run) {
  const loadAtStart = load();
  const t0 = Date.now();
  const players = [];
  const contexts = [];
  // The order alternates between runs so that being first or second shows up as a difference.
  for (const src of run % 2 ? sources : [...sources].reverse()) {
    const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
    contexts.push(ctx);
    const page = await ctx.newPage();
    const cdp = await ctx.newCDPSession(page);
    const protocols = {};
    const origin = new URL(src.url).origin;
    await cdp.send("Network.enable");
    cdp.on("Network.responseReceived", ({ response }) => {
      if (!response.url.startsWith(origin)) return;
      const key = `${response.protocol} ${/\.m3u8/.test(response.url) ? "playlist" : "media"}`;
      protocols[key] = (protocols[key] ?? 0) + 1;
    });
    await page.goto(pageUrl);
    await page.addScriptTag({ path: process.env.HLS_JS });
    const support = await page.evaluate(() => ({
      h264: MediaSource.isTypeSupported('video/mp4; codecs="avc1.64001f,mp4a.40.2"'),
      hls: Hls.version,
    }));
    if (!support.h264) throw new Error("this browser cannot play H.264 and AAC in MSE");
    players.push({ ...src, page, protocols, version: support.hls });
  }
  for (const p of players) {
    await p.page.evaluate((src) => {
      const video = document.createElement("video");
      video.muted = true;
      video.autoplay = true;
      video.playsInline = true;
      document.body.append(video);
      const w = (window.state = { samples: [], errors: {}, waits: 0, fatal: null });
      const hls = new Hls({ lowLatencyMode: true });
      hls.on(Hls.Events.ERROR, (_e, d) => {
        w.errors[d.details] = (w.errors[d.details] ?? 0) + 1;
        if (d.fatal) w.fatal = d.details;
      });
      video.addEventListener("waiting", () => w.waits++);
      hls.loadSource(src);
      hls.attachMedia(video);
      video.play().catch(() => {});
      setInterval(() => {
        const b = video.buffered;
        const end = b.length ? b.end(b.length - 1) : 0;
        w.samples.push({
          at: Date.now(),
          t: video.currentTime,
          latency: hls.latency,
          target: hls.targetLatency,
          wall: hls.playingDate ? (Date.now() - hls.playingDate.getTime()) / 1000 : null,
          ahead: end - video.currentTime,
          rate: video.playbackRate,
          paused: video.paused,
          hold: hls.levels?.[0]?.details?.partHoldBack ?? null,
        });
      }, 250);
    }, p.url);
  }

  const start = Date.now();
  while (Date.now() - start < seconds * 1000) {
    await new Promise((r) => setTimeout(r, Math.min(30000, seconds * 1000 - (Date.now() - start))));
    if (seconds > 60) console.log(`  ${Math.round((Date.now() - start) / 1000)} s, load average ${load()}`);
  }
  console.log(`run ${run}: started ${players.map((p) => p.label).join(" then ")}; hls.js ${players[0].version} in ${process.env.CHROME_CHANNEL || "chrome"} (headless), ${seconds} s, ${players.length} playing together; load average (1 min) ${loadAtStart} at the start, ${load()} at the end`);
  for (const p of players) {
    const st = await p.page.evaluate(() => {
      const q = document.querySelector("video").getVideoPlaybackQuality();
      return { ...window.state, quality: { total: q.totalVideoFrames, dropped: q.droppedVideoFrames } };
    });
    const s = st.samples.filter((x) => x.at - t0 > 20000 && !x.paused);
    const col = (k) => s.map((x) => x[k]).filter((v) => v !== null && Number.isFinite(v));
    const minutes = [];
    for (let m = 0; m * 60000 < Date.now() - t0; m++) {
      const inMinute = s.filter((x) => x.at - t0 >= m * 60000 && x.at - t0 < (m + 1) * 60000 && Number.isFinite(x.latency));
      if (inMinute.length > 40) minutes.push(pct(inMinute.map((x) => x.latency), 0.5).toFixed(2));
    }
    const wall = s.length > 1 ? (s[s.length - 1].at - s[0].at) / 1000 : 0;
    const media = s.length > 1 ? s[s.length - 1].t - s[0].t : 0;
    console.log(`${p.label}: ${p.url}`);
    console.log(`  hls.js latency:  ${stats(col("latency"))}`);
    console.log(`  wall clock:      ${stats(col("wall"))}`);
    console.log(`  buffer ahead:    ${stats(col("ahead"))}`);
    if (minutes.length > 1) console.log(`  hls.js latency, median of each minute after start-up: ${minutes.join(", ")} s`);
    console.log(`  hls.js target latency ${col("target")[0]?.toFixed(2)} s (PART-HOLD-BACK ${s[0]?.hold ?? "n/a"} s); playback rate ${stats(col("rate"), "x")}`);
    console.log(`  played ${media.toFixed(1)} s of media in ${wall.toFixed(1)} s; waiting events ${st.waits}; frames dropped ${st.quality.dropped} of ${st.quality.total}`);
    console.log(`  hls.js errors: ${JSON.stringify(st.errors)}${st.fatal ? " FATAL " + st.fatal : ""}`);
    console.log(`  responses by protocol: ${JSON.stringify(p.protocols)}`);
    if (st.fatal || !s.length) failed = true;
    (results[p.label] ??= []).push({ latency: pct(col("latency"), 0.5), wall: pct(col("wall"), 0.5), ahead: pct(col("ahead"), 0.5), dropped: st.quality.dropped, total: st.quality.total });
  }
  for (const ctx of contexts) await ctx.close();
}

try {
  for (let run = 1; run <= runs; run++) await playOnce(run);
  if (runs > 1) {
    console.log(`summary of ${runs} runs (each run's median, then the median, lowest and highest of those)`);
    for (const [label, rs] of Object.entries(results)) {
      const line = (k) => `${pct(rs.map((r) => r[k]), 0.5).toFixed(2)} s (${Math.min(...rs.map((r) => r[k])).toFixed(2)} to ${Math.max(...rs.map((r) => r[k])).toFixed(2)})`;
      console.log(`  ${label}: hls.js latency ${line("latency")}; wall clock ${line("wall")}; buffer ahead ${line("ahead")}; frames dropped ${rs.reduce((a, r) => a + r.dropped, 0)} of ${rs.reduce((a, r) => a + r.total, 0)}`);
    }
  }
  process.exitCode = failed ? 1 : 0;
} finally {
  await browser.close();
  pageServer.close();
}
