#!/usr/bin/env node
// Checks sapflow's LL-HLS output through a TLS and HTTP/2 front (Caddy)
// against the same stream served directly, over a running publisher.
// Driven by scripts/behind-caddy.sh; usable on its own against any Caddy.
//
//   node scripts/behind-caddy.mjs check    --direct URL --caddy URL [--ca FILE] [--pause-pid PID]
//   node scripts/behind-caddy.mjs blocking --direct URL --caddy URL [--ca FILE] [--samples 150] [--control]
//   node scripts/behind-caddy.mjs lag      --direct URL --caddy URL [--ca FILE] [--seconds 60] [--t0 EPOCH_MS]
//
// URL is the stream's directory, with no trailing file: the plain-HTTP one
// (http://host:8888/live/EVENT) and the one the front serves
// (https://host/live-hls/live/EVENT). The front is spoken to in HTTP/2 only
// (ALPN h2), so a front that does not negotiate it fails outright. --ca is
// the PEM file of the front's CA when it is not publicly trusted.
// --pause-pid stops the publisher for a few seconds, to hold one request for
// the server's whole limit. --control puts a second plain connection where
// the front would be, to show what the comparison measures with nothing in
// the way. Node 20 or later, no packages. Exits 1 when a check fails.
import fs from "node:fs";
import http from "node:http";
import http2 from "node:http2";
import { performance } from "node:perf_hooks";
import zlib from "node:zlib";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const nowMs = () => performance.timeOrigin + performance.now();

// ---------------------------------------------------------------- clients

class Target {
  constructor(name, base, ca) {
    this.name = name;
    this.url = new URL(base.replace(/\/+$/, "") + "/");
    this.ca = ca;
    this.agent = new http.Agent({ keepAlive: true, maxSockets: 32 });
    this.h2 = null;
    this.protocol = this.url.protocol === "https:" ? "h2" : "http/1.1";
  }

  session() {
    if (!this.h2 || this.h2.closed || this.h2.destroyed) {
      this.h2 = http2.connect(this.url.origin, { ca: this.ca });
      this.h2.on("error", () => {});
    }
    return this.h2;
  }

  // rel is resolved against the stream's directory ("../other/stream.m3u8" works).
  get(rel, { method = "GET", headers: extra = {}, timeout = 15000 } = {}) {
    const url = new URL(rel, this.url);
    // Players are browsers: they offer compression on every request.
    const headers = { "accept-encoding": "gzip, deflate, br", ...extra };
    const t0 = performance.now();
    return new Promise((resolve, reject) => {
      const chunks = [];
      let status = 0;
      let head = {};
      let ttfb = 0;
      const done = () => {
        const wire = Buffer.concat(chunks);
        const enc = head["content-encoding"];
        const decode = { gzip: zlib.gunzipSync, br: zlib.brotliDecompressSync, zstd: zlib.zstdDecompressSync }[enc];
        let body;
        try {
          body = decode ? decode(wire) : wire;
        } catch (e) {
          return fail(e);
        }
        resolve({
          status,
          headers: head,
          body,
          wire: wire.length,
          ttfb,
          total: performance.now() - t0,
          at: nowMs(),
          protocol: this.protocol,
        });
      };
      const timer = setTimeout(() => reject(new Error(`${this.name}: ${rel} timed out`)), timeout);
      const fail = (e) => {
        clearTimeout(timer);
        reject(new Error(`${this.name}: ${rel}: ${e.message}`));
      };
      if (this.url.protocol === "https:") {
        const s = this.session();
        const req = s.request({ ":method": method, ":path": url.pathname + url.search, ...headers });
        req.on("response", (h) => {
          status = h[":status"];
          head = h;
          ttfb = performance.now() - t0;
          this.protocol = s.alpnProtocol || "h2";
        });
        req.on("data", (c) => chunks.push(c));
        req.on("end", () => (clearTimeout(timer), done()));
        req.on("error", fail);
        req.end();
      } else {
        const req = http.request(url, { method, headers, agent: this.agent }, (res) => {
          status = res.statusCode;
          head = res.headers;
          ttfb = performance.now() - t0;
          res.on("data", (c) => chunks.push(c));
          res.on("end", () => (clearTimeout(timer), done()));
          res.on("error", fail);
        });
        req.on("error", fail);
        req.end();
      }
    });
  }

  close() {
    this.agent.destroy();
    if (this.h2 && !this.h2.destroyed) this.h2.close();
  }
}

// -------------------------------------------------------------- playlists

// Parts with the segment number and index they belong to, and the part a
// blocking reload should ask for next.
function parsePlaylist(text) {
  const out = { mediaSeq: 0, target: 0, parts: [], hint: null, map: null };
  let msn = null;
  let idx = 0;
  for (const line of text.split("\n")) {
    let m;
    if ((m = line.match(/^#EXT-X-MEDIA-SEQUENCE:(\d+)/))) out.mediaSeq = Number(m[1]);
    else if ((m = line.match(/^#EXT-X-TARGETDURATION:(\d+)/))) out.target = Number(m[1]);
    else if ((m = line.match(/^#EXT-X-MAP:URI="([^"]+)"/))) out.map = m[1];
    else if ((m = line.match(/^#EXT-X-PRELOAD-HINT:TYPE=PART,URI="([^"]+)"/))) out.hint = m[1];
    else if ((m = line.match(/^#EXT-X-PART:DURATION=([\d.]+),URI="([^"]+)"/))) {
      msn ??= out.mediaSeq;
      out.parts.push({ msn, idx: idx++, dur: Number(m[1]), uri: m[2] });
    } else if (line.startsWith("#EXTINF:")) {
      msn ??= out.mediaSeq;
      msn++;
      idx = 0;
    }
  }
  out.nextMsn = msn ?? out.mediaSeq;
  out.nextPart = idx;
  return out;
}

const hasPart = (pl, msn, part) =>
  pl.parts.some((p) => (p.msn === msn && p.idx === part) || (p.msn === msn + 1 && p.idx === 0));

const blockingPath = (pl) => `stream.m3u8?_HLS_msn=${pl.nextMsn}&_HLS_part=${pl.nextPart}`;

function* boxes(b, want, i = 0, end = b.length) {
  while (i + 8 <= end) {
    const size = b.readUInt32BE(i);
    const type = b.toString("latin1", i + 4, i + 8);
    if (size < 8) return;
    if (type === want) yield b.subarray(i + 8, i + size);
    if (["moov", "trak", "mdia", "moof", "traf"].includes(type)) yield* boxes(b, want, i + 8, i + size);
    i += size;
  }
}

const timescaleOf = (init) => {
  const md = boxes(init, "mdhd").next().value;
  return md.readUInt32BE(md[0] === 0 ? 12 : 20);
};

const decodeTimeOf = (part) => {
  const tf = boxes(part, "tfdt").next().value;
  return tf[0] === 1 ? Number(tf.readBigUInt64BE(4)) : tf.readUInt32BE(4);
};

// ------------------------------------------------------------------ stats

const pct = (xs, p) => {
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(s.length * p))];
};
const f = (x, d = 1) => (x === undefined ? "n/a" : x.toFixed(d));
const summary = (xs, d = 1, unit = "ms") =>
  xs.length
    ? `median ${f(pct(xs, 0.5), d)} ${unit}, p90 ${f(pct(xs, 0.9), d)}, p99 ${f(pct(xs, 0.99), d)}, max ${f(Math.max(...xs), d)} (n=${xs.length})`
    : "no samples";

// -------------------------------------------------------------- reporting

let failures = 0;
const say = (level, name, detail = "") => {
  if (level === "FAIL") failures++;
  console.log(`${level.padEnd(4)} ${name}${detail ? ": " + detail : ""}`);
};
const expect = (cond, name, detail = "") => say(cond ? "ok" : "FAIL", name, detail);

async function waitLive(t, seconds = 30) {
  const end = Date.now() + seconds * 1000;
  for (;;) {
    try {
      const r = await t.get("index.m3u8");
      if (r.status === 200) return;
    } catch {}
    if (Date.now() > end) throw new Error(`${t.name}: stream not live after ${seconds} s`);
    await sleep(250);
  }
}

// The next part of the stream, as the plain server reports it.
async function nextPartPath(direct) {
  const r = await direct.get("stream.m3u8");
  const pl = parsePlaylist(r.body.toString());
  return { pl, path: blockingPath(pl) };
}

// Block on the plain server until the part you asked for exists, so what
// follows starts just after a part appeared (about 190 ms before the next).
async function settle(direct) {
  const { pl, path } = await nextPartPath(direct);
  const r = await direct.get(path);
  return { pl: parsePlaylist(r.body.toString()), r };
}

// ------------------------------------------------------------------ check

const COMPARED = ["content-type", "cache-control", "access-control-allow-origin", "content-length"];

async function check(direct, front, pausePid) {
  await waitLive(direct);
  await waitLive(front);

  // 1. protocol
  await front.get("index.m3u8");
  expect(front.protocol === "h2", "HTTP/2 negotiated with the front", `ALPN ${front.protocol}`);

  // 2. multivariant playlist: same bytes and headers
  const [mvD, mvF] = await Promise.all([direct.get("index.m3u8"), front.get("index.m3u8")]);
  expect(mvF.status === 200 && mvD.body.equals(mvF.body), "multivariant playlist identical to direct", `${mvF.body.length} bytes`);
  for (const h of COMPARED) {
    expect(mvD.headers[h] === mvF.headers[h], `multivariant ${h} passed through`, String(mvF.headers[h]));
  }
  const added = Object.keys(mvF.headers).filter((h) => !h.startsWith(":") && !(h in mvD.headers));
  const dropped = Object.keys(mvD.headers).filter((h) => !(h in mvF.headers));
  say("note", "headers the front adds", added.map((h) => `${h}: ${mvF.headers[h]}`).join("; ") || "none");
  say("note", "headers the front drops", dropped.join(", ") || "none");

  // 3. media playlist: pairs fetched together just after a part appeared
  let same = 0;
  let newer = 0;
  let other = 0;
  for (let i = 0; i < 40; i++) {
    await settle(direct);
    const [a, b] = await Promise.all([direct.get("stream.m3u8"), front.get("stream.m3u8")]);
    const ta = a.body.toString();
    const tb = b.body.toString();
    if (ta === tb) same++;
    else {
      // A part appeared between the two answers: the newest part number differs.
      const newest = (t) => Math.max(...parsePlaylist(t).parts.map((p) => Number(p.uri.match(/part(\d+)/)?.[1] ?? -1)));
      if (newest(ta) !== newest(tb)) newer++;
      else other++;
    }
  }
  expect(other === 0, "media playlist identical to direct", `${same} of 40 byte-identical pairs, ${newer} differing only by a newer part, ${other} otherwise`);
  const mp = await front.get("stream.m3u8");
  const mpD = await direct.get("stream.m3u8");
  for (const h of ["content-type", "cache-control", "access-control-allow-origin"]) {
    expect(mpD.headers[h] === mp.headers[h], `media playlist ${h} passed through`, String(mp.headers[h]));
  }

  // 4. compression and media bodies
  const pl = parsePlaylist(mp.body.toString());
  const sample = {
    "multivariant playlist": ["index.m3u8", false],
    "media playlist": ["stream.m3u8", false],
    "init segment": [pl.map, true],
    "part": [pl.parts[pl.parts.length - 3].uri, true],
    "segment": [null, true],
  };
  const segUri = pl.map && pl.map.replace(/_init\.mp4$/, `_seg${pl.nextMsn - 2}.mp4`);
  sample.segment[0] = segUri;
  for (const [what, [rel, media]] of Object.entries(sample)) {
    const [a, b] = await Promise.all([direct.get(rel), front.get(rel)]);
    const enc = b.headers["content-encoding"];
    if (enc) say(media ? "FAIL" : "WARN", `${what} compressed by the front`, `content-encoding ${enc}, ${a.body.length} bytes became ${b.wire}`);
    else say("ok", `${what} not compressed`, `${b.wire} bytes, compression offered`);
    if (media) {
      expect(a.status === 200 && b.status === 200 && a.body.equals(b.body), `${what} bytes identical to direct`);
      for (const h of COMPARED) expect(a.headers[h] === b.headers[h], `${what} ${h} passed through`, String(b.headers[h]));
    }
  }

  // 5. preload hint: asked for before it exists, answered when it does
  const hintD = [];
  const hintF = [];
  let hintOk = 0;
  for (let i = 0; i < 12; i++) {
    const { pl: now } = await settle(direct);
    if (!now.hint) continue;
    const [a, b] = await Promise.all([direct.get(now.hint), front.get(now.hint)]);
    if (a.status === 200 && b.status === 200 && a.body.equals(b.body)) hintOk++;
    hintD.push(a.total);
    hintF.push(b.total);
  }
  expect(hintOk === hintD.length && hintD.length > 0, "preload-hinted part held until it exists, then identical", `${hintOk} of ${hintD.length}`);
  say("note", "preload-hint response time, direct", summary(hintD));
  say("note", "preload-hint response time, front", summary(hintF));

  // 6. refusals, and CORS preflight
  const cases = [
    ["unknown stream", "../no-such-event/stream.m3u8", 404],
    ["unknown chunk", "no-such-chunk.mp4", 404],
    ["bad _HLS_msn", "stream.m3u8?_HLS_msn=abc", 400],
    ["_HLS_part without _HLS_msn", "stream.m3u8?_HLS_part=1", 400],
    ["_HLS_msn far ahead", "stream.m3u8?_HLS_msn=99999&_HLS_part=0", 400],
  ];
  for (const [what, rel, want] of cases) {
    const [a, b] = await Promise.all([direct.get(rel), front.get(rel)]);
    expect(
      a.status === want && b.status === want && b.body.length === a.body.length && b.headers["access-control-allow-origin"] === a.headers["access-control-allow-origin"],
      `${what} answered ${want} as direct`,
      `front ${b.status}, ${b.body.length} bytes, allow-origin ${b.headers["access-control-allow-origin"]}`,
    );
  }
  const pf = { origin: "https://example.test", "access-control-request-method": "GET" };
  const [oa, ob] = await Promise.all([direct.get("stream.m3u8", { method: "OPTIONS", headers: pf }), front.get("stream.m3u8", { method: "OPTIONS", headers: pf })]);
  expect(
    oa.status === ob.status && oa.headers["access-control-allow-methods"] === ob.headers["access-control-allow-methods"],
    "CORS preflight answered as direct",
    `${ob.status}, allow-methods ${ob.headers["access-control-allow-methods"]}`,
  );

  // 7. a request held for the server's whole limit (3 x TARGETDURATION) is
  // not cut short by the front: the server's own 503 arrives. Needs the
  // publisher paused so the next segment cannot close, hence --pause-pid.
  if (!pausePid) {
    say("note", "held-request check skipped", "pass --pause-pid PID of the publisher to run it");
    return;
  }
  process.kill(pausePid, "SIGSTOP");
  try {
    await sleep(300);
    const fresh = (await nextPartPath(direct)).pl;
    const slow = `stream.m3u8?_HLS_msn=${fresh.nextMsn + 1}&_HLS_part=0`;
    const [sa, sb] = await Promise.all([direct.get(slow, { timeout: 60000 }), front.get(slow, { timeout: 60000 })]);
    expect(
      sa.status === 503 && sb.status === 503 && Math.abs(sa.total - sb.total) < 500,
      "request held for 3 x TARGETDURATION ends in the server's 503 through the front",
      `direct ${f(sa.total / 1000, 2)} s, front ${f(sb.total / 1000, 2)} s, status ${sa.status}/${sb.status}`,
    );
  } finally {
    process.kill(pausePid, "SIGCONT");
  }
}

// --------------------------------------------------------------- blocking

async function blocking(direct, front, samples, control) {
  await waitLive(direct);
  await waitLive(front);
  const label = control ? "second plain connection (control)" : `front (${front.protocol})`;
  const rows = [];
  for (let i = 0; i < samples; i++) {
    const { pl } = await settle(direct);
    const path = blockingPath(pl);
    // Land at different points in the 200 ms part interval, as viewers do.
    await sleep(Math.random() * 150);
    // Alternate which is sent first so neither is favoured by a fraction of a millisecond.
    const reqs = i % 2 ? [front, direct] : [direct, front];
    const rs = await Promise.all(reqs.map((t) => t.get(path)));
    const [rd, rf] = i % 2 ? [rs[1], rs[0]] : rs;
    const ok = (r) => r.status === 200 && hasPart(parsePlaylist(r.body.toString()), pl.nextMsn, pl.nextPart);
    rows.push({ d: rd.total, f: rf.total, okD: ok(rd), okF: ok(rf), statusF: rf.status });
  }
  const n = rows.length;
  console.log(`blocking reload for the next part, ${n} samples (plain HTTP/1.1 and ${label} asked together)`);
  console.log(`  answers carrying the requested part: direct ${rows.filter((r) => r.okD).length}/${n}, other ${rows.filter((r) => r.okF).length}/${n}`);
  console.log(`  direct response time:  ${summary(rows.map((r) => r.d))}`);
  console.log(`  other response time:   ${summary(rows.map((r) => r.f))}`);
  const diff = rows.map((r) => r.f - r.d);
  console.log(`  other minus direct:    ${summary(diff, 2)}`);
  const early = rows.filter((r) => !r.okF).length;
  expect(early === 0, "every blocking reload through the other path carried the requested part", `${n - early}/${n}`);
}

// -------------------------------------------------------------------- lag

async function lag(direct, front, seconds, t0, control) {
  await waitLive(direct);
  await waitLive(front);
  const seen = { direct: new Map(), front: new Map() };
  const until = Date.now() + seconds * 1000;
  const info = { tfdt: new Map(), dur: new Map() };
  const joined = new Set();
  const pending = [];
  // The part's own timestamps say when its media was captured; fetched as
  // each part appears, since old parts leave the playlist's window.
  const first = parsePlaylist((await direct.get("stream.m3u8")).body.toString());
  const timescale = timescaleOf((await direct.get(first.map)).body);

  const follow = async (t, key) => {
    let path = "stream.m3u8";
    let joining = true;
    while (Date.now() < until) {
      let r;
      try {
        r = await t.get(path);
      } catch {
        await sleep(50);
        path = "stream.m3u8";
        continue;
      }
      if (r.status !== 200) {
        await sleep(50);
        path = "stream.m3u8";
        continue;
      }
      const pl = parsePlaylist(r.body.toString());
      for (const p of pl.parts) {
        if (seen[key].has(p.uri)) continue;
        seen[key].set(p.uri, r.at);
        info.dur.set(p.uri, p.dur);
        // Parts already listed when we joined say nothing about delay.
        if (joining) joined.add(p.uri);
        else if (key === "direct" && t0 !== null) {
          pending.push(direct.get(p.uri).then((d) => d.status === 200 && info.tfdt.set(p.uri, decodeTimeOf(d.body))).catch(() => {}));
        }
      }
      joining = false;
      path = blockingPath(pl);
    }
  };
  await Promise.all([follow(direct, "direct"), follow(front, "front")]);
  await Promise.all(pending);
  const startOf = (u) => info.tfdt.get(u) / timescale;

  const live = [...seen.direct.keys()].filter((u) => !joined.has(u));
  const common = live.filter((u) => seen.front.has(u)).slice(8); // 8: the encoder's start-up
  const missed = live.filter((u) => !seen.front.has(u)).length;
  const other = control ? "second plain connection (control)" : `front (${front.protocol})`;
  console.log(`part availability over ${seconds} s: ${live.length} parts appeared directly, ${missed} of them never seen through the other path`);
  if (t0 !== null) {
    const lagOf = (m) => common.filter((u) => info.tfdt.has(u)).map((u) => (m.get(u) - (t0 + (startOf(u) + info.dur.get(u)) * 1000)) / 1000);
    const a = lagOf(seen.direct);
    const b = lagOf(seen.front);
    console.log(`  after the encoder sent the part's last frame, direct: ${summary(a, 2, "s")}`);
    console.log(`  after the encoder sent the part's last frame, ${other}: ${summary(b, 2, "s")}`);
    const third = Math.floor(a.length / 3);
    const med = (xs) => pct(xs, 0.5);
    console.log(`  direct, median over the run's first, middle and last third: ${f(med(a.slice(0, third)), 2)}, ${f(med(a.slice(third, 2 * third)), 2)}, ${f(med(a.slice(2 * third)), 2)} s; lowest ${f(Math.min(...a), 2)} s`);
  }
  const diff = common.map((u) => seen.front.get(u) - seen.direct.get(u));
  console.log(`  other path minus direct, per part:  ${summary(diff, 2)}`);
}

// ------------------------------------------------------------------- main

function parseArgs(argv) {
  const a = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--control") a.control = true;
    else if (argv[i].startsWith("--")) a[argv[i].slice(2)] = argv[++i];
    else a._.push(argv[i]);
  }
  return a;
}

const args = parseArgs(process.argv.slice(2));
const mode = args._[0];
if (!["check", "blocking", "lag"].includes(mode) || !args.direct || (!args.caddy && !args.control)) {
  console.error(fs.readFileSync(new URL(import.meta.url), "utf8").split("\n").slice(1, 18).map((l) => l.replace(/^\/\/ ?/, "")).join("\n"));
  process.exit(2);
}
const ca = args.ca ? fs.readFileSync(args.ca) : undefined;
const direct = new Target("direct", args.direct, ca);
const front = args.control ? new Target("control", args.direct, ca) : new Target("front", args.caddy, ca);
try {
  if (mode === "check") await check(direct, front, args["pause-pid"] ? Number(args["pause-pid"]) : 0);
  else if (mode === "blocking") await blocking(direct, front, Number(args.samples ?? 150), !!args.control);
  else await lag(direct, front, Number(args.seconds ?? 60), args.t0 === undefined ? null : Number(args.t0), !!args.control);
} catch (e) {
  console.error(`FAIL ${e.message}`);
  failures++;
}
direct.close();
front.close();
process.exit(failures ? 1 : 0);
