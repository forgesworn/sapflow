#!/usr/bin/env python3
"""A4.1: WHIP against SRT under the same loss and jitter, both into sapflow.

The method of docs/srt-loss-method.md, with the owner's decision of 12
November 2026 to use its userspace proxy (srt-loss-proxy) for both
protocols instead of netem. One clip, encoded once: 30 s of 1280x720 at
30 fps (testsrc2), constrained baseline H.264 at 2.5 Mbit/s with no
B-frames and 1 s keyframes, so both senders send the same video; SRT's copy
carries AAC 128 kbit/s in MPEG-TS, WHIP's Opus 128 kbit/s. Neither sender
encodes, so neither can answer a keyframe request: both recover only by
retransmission, and by the next 1 s keyframe.

- SRT: ffmpeg -re (copy) into UDP, srt-live-transmit (libsrt) to sapflow's
  SRT listener through the proxy (--protocol srt: everything but the
  handshake impaired), latency 200 ms on both ends unless an arm sets
  `latency=N` (--arms), which srt-live-transmit asks for; sapflow's
  listener gives a caller asking for more what it asks, and the latency it
  accepted (its "SRT publisher accepted" line) is kept with the trial.
- WHIP: through the proxy (--protocol whip: only SRTP and SRTCP impaired),
  the answer's only candidate being the proxy. Sapflow's jitter wait is its
  default unless an arm sets SAPFLOW_WHIP_JITTER or the experimental
  SAPFLOW_WHIP_NACK_REPEAT (--arms). The default was 200 ms when the A4.1
  runs were made and is 400 ms since the owner's decision of 15 November
  2026, so a plain whip arm on a later build is no longer a 200 ms arm:
  set SAPFLOW_WHIP_JITTER=200 to repeat one. The publisher (--whip-publisher) is headless Chrome by default:
  a 1280x720 canvas at 30 fps (moving bars and a frame counter) and a
  WebAudio tone, at most 2.5 Mbit/s, which answers NACKs over RTX and
  keyframe requests. FFmpeg's WHIP muxer (ffmpeg: the clip, copied) sends
  the same RTP whatever is lost: it does not retransmit, so it is no fair
  match for SRT and is kept only to show that.

Each trial is a fresh sapflow, a fresh proxy (fixed delay --delay-ms each
way, --jitter-ms, random loss, a seed) and one publisher. Frames delivered
are the video packets in sapflow's recording (ffprobe -count_packets; one
per frame in MP4). Frames unrecovered are frames sent less frames
delivered: the clip's 900 for SRT, Chrome's own count (framesSent) for
WHIP; the summary also gives them against each protocol's clean mean.
Decoder error lines and sapflow's own counters are kept beside them.

    scripts/whip-loss.py --bin <sapflow> --proxy <srt-loss-proxy> --out DIR
        [--losses 1,5,10] [--seeds 5] [--clean 3] [--jitter-ms 20]
        [--delay-ms 30] [--protocols srt,whip] [--whip-publisher chrome|ffmpeg]
        [--profiles LOSS:JITTER[:keep],...] [--base 47300]
        [--arms NAME=PROTOCOL[+KEY=VALUE...],...] [--max-load 8]

--profiles replaces --losses and --jitter-ms with a list of profiles, each
a loss percentage and a jitter in ms, and `keep` for jitter that spreads the
delays without reordering (the proxy's --jitter-order keep): for example
0:20,5:0,5:20:keep. The clean runs (--clean) are added as before.

--arms replaces --protocols with named arms, each a protocol and any
environment for sapflow, run in the order given for each seed: for example
base=whip,srt=srt,nack30=whip+SAPFLOW_WHIP_NACK_REPEAT=30,
wait400=whip+SAPFLOW_WHIP_JITTER=400. An SRT arm can set its latency
instead: srt400=srt+latency=400. Each arm has its own clean runs. The
summary gives each arm against the SRT arm and, for WHIP arms, against the
first WHIP arm (the baseline), with a Welch t each. With more than one SRT
arm, "the SRT arm" is the first, and each further SRT arm gets its own
pair of columns (minus_NAME, welch_t_vs_NAME) at the end.

--max-load holds each trial until the one-minute load average is below it
(0, the default, never waits); the load at the start, the wait and the load
at the end are kept with the trial.

Writes DIR/trials.jsonl as each trial ends, then DIR/trials.tsv and
DIR/summary.tsv. Every child is the driver's own
and is stopped by its PID.
"""

import argparse
import json
import math
import os
import re
import secrets
import shutil
import signal
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import statistics
import subprocess
import sys
import time
from pathlib import Path

CLIP_SECONDS = 30
CLIP_FRAMES = 900
LATENCY_MS = 200
CHROME = os.environ.get("CHROME", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")

PAGE = """<!doctype html><meta charset="utf-8"><title>WHIP A4.1</title>
<script>
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
(async () => {
  const out = { errors: [] };
  try {
    const c = document.createElement('canvas'); c.width = 1280; c.height = 720;
    const g = c.getContext('2d');
    let n = 0;
    const draw = () => {
      n++;
      for (let i = 0; i < 16; i++) { g.fillStyle = `hsl(${(i * 23 + n * 3) % 360} 70% 50%)`; g.fillRect(i * 80, 0, 80, 720); }
      g.fillStyle = '#fff'; g.fillRect((n * 7) % 1180, 300, 100, 100);
      g.font = '96px monospace'; g.fillStyle = '#000'; g.fillText(String(n), 40, 120);
      requestAnimationFrame(draw);
    };
    draw();
    const ctx = new AudioContext({ sampleRate: 48000 }); await ctx.resume();
    const o = ctx.createOscillator(), dest = ctx.createMediaStreamDestination(); o.frequency.value = 440; o.connect(dest); o.start();
    const media = new MediaStream([...c.captureStream(30).getVideoTracks(), ...dest.stream.getAudioTracks()]);
    const pc = new RTCPeerConnection();
    let vsender = null;
    media.getTracks().forEach((t) => { const tr = pc.addTransceiver(t, { direction: 'sendonly', streams: [media] }); if (t.kind === 'video') vsender = tr.sender; });
    const p = vsender.getParameters(); p.degradationPreference = 'maintain-resolution';
    if (p.encodings && p.encodings[0]) p.encodings[0].maxBitrate = 2500000;
    await vsender.setParameters(p).catch((e) => out.errors.push('setParameters ' + e.message));
    await pc.setLocalDescription(await pc.createOffer());
    await Promise.race([new Promise((r) => { pc.onicegatheringstatechange = () => pc.iceGatheringState === 'complete' && r(); }), sleep(1500)]);
    const r = await fetch('WHIP_URL', { method: 'POST', headers: { 'Content-Type': 'application/sdp', Authorization: 'Bearer TOKEN' }, body: pc.localDescription.sdp });
    out.post = r.status; const location = r.headers.get('Location');
    if (r.status !== 201) throw new Error('POST ' + r.status + ' ' + await r.text());
    await pc.setRemoteDescription({ type: 'answer', sdp: await r.text() });
    for (let i = 0; i < 100 && pc.connectionState !== 'connected'; i++) await sleep(100);
    out.connection = pc.connectionState;
    await sleep(SECONDS * 1000);
    // Stop sending, then count what was sent.
    media.getTracks().forEach((t) => t.stop());
    await sleep(500);
    const s = await vsender.getStats();
    s.forEach((x) => { if (x.type === 'outbound-rtp' && x.kind === 'video') Object.assign(out, {
      framesEncoded: x.framesEncoded, framesSent: x.framesSent, keyFramesEncoded: x.keyFramesEncoded, nackCount: x.nackCount,
      pliCount: x.pliCount, packetsSent: x.packetsSent, retransmittedPacketsSent: x.retransmittedPacketsSent,
      frameWidth: x.frameWidth, framesPerSecond: x.framesPerSecond, qualityLimitationReason: x.qualityLimitationReason });
      if (x.type === 'remote-inbound-rtp' && x.kind === 'video') Object.assign(out, {
        roundTripTime: x.roundTripTime, totalRoundTripTime: x.totalRoundTripTime,
        roundTripTimeMeasurements: x.roundTripTimeMeasurements }); });
    const d = await fetch(location, { method: 'DELETE', headers: { Authorization: 'Bearer TOKEN' } });
    out.delete = d.status; pc.close();
  } catch (e) { out.errors.push(String(e && e.message || e)); }
  await fetch('/report', { method: 'POST', body: JSON.stringify(out) });
})();
</script>"""


def chrome_publish(d, whip_url, token, page_port):
    """Headless Chrome publishes for the clip's length; returns its report."""
    page = PAGE.replace("WHIP_URL", whip_url).replace("TOKEN", token).replace("SECONDS", str(CLIP_SECONDS))
    got = {}
    done = threading.Event()

    class H(BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200); self.send_header("content-type", "text/html"); self.end_headers()
            self.wfile.write(page.encode())

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("content-length", 0)))
            self.send_response(200); self.end_headers(); self.wfile.write(b"ok")
            got.update(json.loads(body)); done.set()

        def log_message(self, *a):
            pass

    httpd = ThreadingHTTPServer(("127.0.0.1", page_port), H)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    profile = tempfile.mkdtemp(prefix="whip-loss-chrome-")
    log = open(d / "chrome.log", "w")
    browser = subprocess.Popen([CHROME, "--headless=new", f"--user-data-dir={profile}", "--no-first-run",
                                "--no-default-browser-check", "--autoplay-policy=no-user-gesture-required",
                                "--allow-loopback-in-peer-connection", "--disable-background-timer-throttling",
                                "--disable-renderer-backgrounding", "--disable-backgrounding-occluded-windows",
                                "--disable-features=MacAppCodeSignClone", f"http://127.0.0.1:{page_port}/"],
                               stdout=log, stderr=log, start_new_session=True)
    try:
        done.wait(CLIP_SECONDS + 45)
    finally:
        stop(browser, group=True)
        httpd.shutdown(); httpd.server_close()
        shutil.rmtree(profile, ignore_errors=True)
        log.close()
    return got


def run(cmd, **kw):
    return subprocess.run(cmd, check=True, **kw)


def clips(out: Path):
    video = out / "video.mkv"
    if not video.exists():
        run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30",
             "-t", str(CLIP_SECONDS), "-pix_fmt", "yuv420p", "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency",
             "-profile:v", "baseline", "-bf", "0", "-g", "30", "-keyint_min", "30", "-sc_threshold", "0",
             "-b:v", "2500k", "-maxrate", "2500k", "-bufsize", "2500k", str(video)])
    tone = ["-f", "lavfi", "-i", f"sine=frequency=440:sample_rate=48000:duration={CLIP_SECONDS}"]
    if not (out / "src.ts").exists():
        run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-i", str(video), *tone, "-map", "0:v", "-map", "1:a",
             "-c:v", "copy", "-c:a", "aac", "-b:a", "128k", "-ac", "2", "-f", "mpegts", str(out / "src.ts")])
    if not (out / "src-whip.mkv").exists():
        run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-i", str(video), *tone, "-map", "0:v", "-map", "1:a",
             "-c:v", "copy", "-c:a", "libopus", "-b:a", "128k", "-ac", "2", "-ar", "48000", str(out / "src-whip.mkv")])


def stop(proc, sig=signal.SIGTERM, wait=6.0, group=False):
    if proc is None or proc.poll() is not None:
        return
    try:
        os.killpg(proc.pid, sig) if group else proc.send_signal(sig)
    except Exception:
        pass
    try:
        proc.wait(wait)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL) if group else proc.kill()
        except Exception:
            pass
        proc.wait()


def frames_in(files):
    """Video and audio packets (frames) in the recording files, and decoder error lines."""
    v = a = 0
    errors = 0
    for f in files:
        for sel, key in (("v", "v"), ("a", "a")):
            r = subprocess.run(["ffprobe", "-v", "error", "-count_packets", "-select_streams", sel, "-show_entries",
                                "stream=nb_read_packets", "-of", "csv=p=0", str(f)], capture_output=True, text=True)
            n = sum(int(x) for x in r.stdout.split() if x.strip().isdigit())
            if key == "v":
                v += n
            else:
                a += n
        d = subprocess.run(["ffmpeg", "-hide_banner", "-nostdin", "-v", "error", "-i", str(f), "-f", "null", "-"],
                           capture_output=True, text=True)
        errors += len([l for l in d.stderr.splitlines() if l.strip() and not l.startswith("[null @")
                       and "Last message repeated" not in l])
    return v, a, errors


def wait_for_load(limit):
    """Waits until the one-minute load is below the limit; returns the wait in s."""
    if not limit:
        return 0
    t0 = time.monotonic()
    while os.getloadavg()[0] >= limit:
        time.sleep(15)
    return round(time.monotonic() - t0)


def trial(args, arm, profile, seed, out):
    name, protocol, arm_env, latency = arm
    loss, jitter, keep = profile
    tag = f"{name}-l{loss}-j{jitter}{'k' if keep else ''}-s{seed}"
    d = out / "trials" / tag
    d.mkdir(parents=True, exist_ok=True)
    base = args.base
    port = {"hls": base, "srt": base + 1, "whip": base + 2, "media": base + 3, "proxy": base + 4, "udp": base + 5,
            "page": base + 6}
    user, pw = "loss", secrets.token_hex(12)
    env = {**os.environ,
           "SAPFLOW_HTTP_ADDR": f"127.0.0.1:{port['hls']}", "SAPFLOW_SRT_ADDR": f"127.0.0.1:{port['srt']}",
           "SAPFLOW_RTMP_ADDR": "off", "SAPFLOW_PUBLISH_USER": user, "SAPFLOW_PUBLISH_PASS": pw,
           "SAPFLOW_RECORDINGS": str(d / "rec"), "SAPFLOW_RECORD_MIN_FREE_GB": "0", "SAPFLOW_RECONNECT_GRACE": "2",
           "SAPFLOW_WHIP_ADDR": f"127.0.0.1:{port['whip']}", "SAPFLOW_WHIP_UDP_ADDR": f"127.0.0.1:{port['media']}",
           "SAPFLOW_WHIP_CANDIDATES": f"udp 127.0.0.1:{port['proxy']}",
           "SAPFLOW_WHIP_PUBLIC_URL": f"http://127.0.0.1:{port['whip']}",
           "SAPFLOW_WHIP_ALLOW_ORIGINS": f"http://127.0.0.1:{port['page']}",
           "RUST_LOG": "info,sapflow::whip=debug", **arm_env}
    waited = wait_for_load(args.max_load)
    row = {"arm": name, "protocol": protocol, "loss_pct": loss, "seed": seed, "jitter_ms": jitter,
           "srt_latency_ms": latency if protocol == "srt" else "",
           "jitter_order": "keep" if keep else "reorder", "env": " ".join(f"{k}={v}" for k, v in arm_env.items()),
           "delay_ms": args.delay_ms, "load": round(os.getloadavg()[0], 2), "load_wait_s": waited,
           "started": time.strftime("%H:%M:%S")}
    slog = open(d / "sapflow.log", "w")
    srv = subprocess.Popen([args.bin], env=env, stdout=slog, stderr=slog)
    procs = []
    try:
        time.sleep(1)
        target = port["srt"] if protocol == "srt" else port["media"]
        proxy = subprocess.Popen([args.proxy, "--listen", f"127.0.0.1:{port['proxy']}", "--target", f"127.0.0.1:{target}",
                                  "--loss", "none" if loss == 0 else f"random:{loss}", "--delay-ms", str(args.delay_ms),
                                  "--jitter-ms", str(jitter), "--jitter-order", "keep" if keep else "reorder",
                                  "--protocol", protocol,
                                  "--latency-ms", str(latency), "--seed", str(seed), "--stats", str(d / "proxy.txt")],
                                 stdout=subprocess.DEVNULL, stderr=open(d / "proxy.log", "w"))
        procs.append(proxy)
        time.sleep(0.5)
        if protocol == "srt":
            snd = subprocess.Popen(["srt-live-transmit", "-a:no", "-c:1316", f"udp://127.0.0.1:{port['udp']}",
                                    f"srt://127.0.0.1:{port['proxy']}?latency={latency}&streamid=publish:live/event-loss:{user}:{pw}"],
                                   stdout=open(d / "snd.log", "w"), stderr=subprocess.STDOUT)
            procs.append(snd)
            for _ in range(50):
                if "SRT target connected" in (d / "snd.log").read_text(errors="replace"):
                    break
                time.sleep(0.1)
            feeder = subprocess.Popen(["ffmpeg", "-hide_banner", "-loglevel", "error", "-re", "-i", str(out / "src.ts"),
                                       "-c", "copy", "-f", "mpegts", f"udp://127.0.0.1:{port['udp']}?pkt_size=1316"],
                                      stderr=open(d / "ff.log", "w"))
            procs.append(feeder)
            row["publisher_exit"] = feeder.wait(CLIP_SECONDS + 40)
            time.sleep(latency / 1000 + 2)
            stop(snd)
        elif args.whip_publisher == "chrome":
            report = chrome_publish(d, f"http://127.0.0.1:{port['whip']}/whip/live/event-loss", f"{user}:{pw}", port["page"])
            (d / "chrome-report.json").write_text(json.dumps(report, indent=1))
            row["publisher_exit"] = 0 if report and not report.get("errors") else 1
            row["frames_sent"] = report.get("framesSent")
            for k in ("keyFramesEncoded", "nackCount", "pliCount", "retransmittedPacketsSent", "packetsSent",
                      "roundTripTime"):
                row[f"chrome_{k}"] = report.get(k)
            row["errors"] = ";".join(report.get("errors", [])) if report else "no report"
        else:
            pub = subprocess.Popen(["ffmpeg", "-hide_banner", "-loglevel", "error", "-re", "-i", str(out / "src-whip.mkv"),
                                    "-c", "copy", "-f", "whip", "-whip_flags", "dtls_active", "-authorization", f"{user}:{pw}",
                                    f"http://127.0.0.1:{port['whip']}/whip/live/event-loss"], stderr=open(d / "ff.log", "w"))
            procs.append(pub)
            row["publisher_exit"] = pub.wait(CLIP_SECONDS + 40)
        # The recording finishes when the 2 s grace is over.
        time.sleep(4)
    except Exception as e:
        row["error"] = str(e)
    finally:
        for p in reversed(procs):
            stop(p)
        stop(srv)
        slog.close()
        time.sleep(0.5)
    row["load_end"] = round(os.getloadavg()[0], 2)
    recs = sorted((d / "rec").rglob("*.mp4")) if (d / "rec").exists() else []
    row["recordings"] = len(recs)
    row["video_frames"], row["audio_frames"], row["decode_errors"] = frames_in(recs)
    if protocol == "srt" or args.whip_publisher == "ffmpeg":
        row["frames_sent"] = CLIP_FRAMES
    if row.get("frames_sent") is not None:
        row["unrecovered"] = row["frames_sent"] - row["video_frames"]
    row["ready"] = "stream ready" in re.sub(r"\x1b\[[0-9;]*m", "", (d / "sapflow.log").read_text(errors="replace"))
    stats = {}
    if (d / "proxy.txt").exists():
        for line in (d / "proxy.txt").read_text().splitlines():
            k, _, v = line.partition("=")
            stats[k] = v
    row["proxy_dropped_c2s"] = stats.get("dropped_c2s", "")
    row["proxy_dropped_s2c"] = stats.get("dropped_s2c", "")
    row["proxy_retransmits"] = stats.get("rtp_repeats" if protocol == "whip" else "data_retx", "")
    row["proxy_reordered"] = stats.get("reordered_orig", "")
    text = (d / "sapflow.log").read_text(errors="replace")
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    listening = re.findall(r"WHIP listening.*?jitter_ms=(\d+) nack_repeat=(\S+)", text)
    if listening:
        row["sapflow_jitter_ms"], row["sapflow_nack_repeat"] = int(listening[-1][0]), listening[-1][1].strip('"')
    if protocol == "whip":
        last = re.findall(r"WHIP receive counters (.*)$", text, re.M)
        if last:
            for k, v in re.findall(r"(\w+)=(\d+)", last[-1]):
                if k in ("video_packets", "video_nacked", "video_recovered", "video_lost", "video_incomplete",
                         "video_skipped", "audio_lost"):
                    row[f"sapflow_{k}"] = int(v)
    else:
        accepted = re.findall(r"^.*SRT publisher accepted.*$", text, re.M)
        got = re.search(r"latency_ms=(\d+)", accepted[-1]) if accepted else None
        row["sapflow_srt_latency_ms"] = int(got.group(1)) if got else ""
        m = re.findall(r"SRT connection closed .*?received=(\d+) retransmitted=(\d+) lost=(\d+) dropped=(\d+)", text)
        if m:
            r_, t_, l_, dr = m[-1]
            row.update({"sapflow_srt_received": int(r_), "sapflow_srt_retransmitted": int(t_),
                        "sapflow_srt_lost": int(l_), "sapflow_srt_dropped": int(dr)})
    print(json.dumps(row), flush=True)
    with open(out / "trials.jsonl", "a") as f:
        f.write(json.dumps(row) + "\n")
    return row


def welch(a, b):
    if len(a) < 2 or len(b) < 2:
        return None
    va, vb = statistics.variance(a), statistics.variance(b)
    se = math.sqrt(va / len(a) + vb / len(b))
    return None if se == 0 else (statistics.mean(a) - statistics.mean(b)) / se


def parse_arms(text):
    """NAME=PROTOCOL[+KEY=VALUE...],... into (name, protocol, env, latency) tuples.

    latency=N (SRT arms only) is the SRT latency in ms; every other pair is
    sapflow's environment."""
    arms = []
    for item in text.split(","):
        name, _, rest = item.partition("=")
        protocol, *pairs = rest.split("+")
        if not name or protocol not in ("srt", "whip"):
            raise SystemExit(f"--arms: {item!r} is not NAME=srt|whip[+KEY=VALUE...]")
        env = {}
        latency = LATENCY_MS
        for pair in pairs:
            k, eq, v = pair.partition("=")
            if k == "latency" and eq:
                if protocol != "srt" or not v.isdigit() or not 20 <= int(v) <= 8000:
                    raise SystemExit(f"--arms: {item!r}: latency=N (20 to 8000 ms) is for SRT arms only")
                latency = int(v)
                continue
            if not eq or not k.startswith("SAPFLOW_"):
                raise SystemExit(f"--arms: {pair!r} is not SAPFLOW_KEY=VALUE")
            env[k] = v
        arms.append((name, protocol, env, latency))
    if len({a[0] for a in arms}) != len(arms):
        raise SystemExit("--arms: names must differ")
    return arms


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", required=True)
    ap.add_argument("--proxy", required=True)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--losses", default="1,5,10")
    ap.add_argument("--seeds", type=int, default=5)
    ap.add_argument("--clean", type=int, default=3)
    ap.add_argument("--jitter-ms", type=int, default=20)
    ap.add_argument("--delay-ms", type=int, default=30)
    ap.add_argument("--protocols", default="srt,whip")
    ap.add_argument("--arms", default=None, help="NAME=PROTOCOL[+KEY=VALUE...],... in place of --protocols")
    ap.add_argument("--max-load", type=float, default=0, help="start a trial only below this one-minute load")
    ap.add_argument("--whip-publisher", default="chrome", choices=["chrome", "ffmpeg"])
    ap.add_argument("--profiles", default=None, help="LOSS:JITTER[:keep],... in place of --losses and --jitter-ms")
    ap.add_argument("--base", type=int, default=47300)
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    if (args.out / "trials.jsonl").exists():
        raise SystemExit(f"{args.out / 'trials.jsonl'} exists: refusing to add to earlier evidence")
    clips(args.out)
    if args.arms:
        arms = parse_arms(args.arms)
        interleave = "fixed"
    else:
        arms = [(p, p, {}, LATENCY_MS) for p in args.protocols.split(",")]
        interleave = "flip"
    clean = (0, 0, False)
    if args.profiles:
        profiles = []
        for item in args.profiles.split(","):
            parts = item.split(":")
            profiles.append((int(parts[0]), int(parts[1]), len(parts) > 2 and parts[2] == "keep"))
    else:
        profiles = [(int(x), args.jitter_ms, False) for x in args.losses.split(",")]
    plan = []
    for s in range(1, args.clean + 1):
        for a in arms:
            plan.append((a, clean, s))
    for prof in profiles:
        for s in range(1, args.seeds + 1):
            # Back to back per seed: with --arms in the order given, else the
            # order flipping each seed.
            order = arms if interleave == "fixed" or s % 2 else list(reversed(arms))
            for a in order:
                plan.append((a, prof, s))
    rows = [trial(args, a, prof, s, args.out) for a, prof, s in plan]
    keys = sorted({k for r in rows for k in r},
                  key=lambda k: (k not in ("arm", "protocol", "loss_pct", "jitter_ms", "seed"), k))
    with open(args.out / "trials.tsv", "w") as f:
        f.write("\t".join(keys) + "\n")
        for r in rows:
            f.write("\t".join(str(r.get(k, "")) for k in keys) + "\n")
    key = lambda r: (r["loss_pct"], r["jitter_ms"], r["jitter_order"] == "keep")
    names = [a[0] for a in arms]
    srt = next((a[0] for a in arms if a[1] == "srt"), None)
    more_srt = [a[0] for a in arms if a[1] == "srt"][1:]
    baseline = next((a[0] for a in arms if a[1] == "whip"), None)
    fmt = lambda x, n=1: "" if x is None else round(x, n)
    with open(args.out / "summary.tsv", "w") as f:
        f.write("loss_pct\tjitter_ms\tjitter_order\tarm\tprotocol\truns\tunrecovered_mean\tunrecovered_sd\tunrecovered\t"
                "share_of_sent_pct\tvs_clean_mean\tminus_srt\twelch_t_vs_srt\tminus_baseline\twelch_t_vs_baseline\t"
                "never_ready\tload\tretransmits\treordered"
                + "".join(f"\tminus_{x}\twelch_t_vs_{x}" for x in more_srt) + "\n")
        order = ([clean] if args.clean else []) + [p for p in profiles if p != clean]
        groups = {(prof, n): [r for r in rows if r["arm"] == n and key(r) == prof] for prof in order for n in names}
        un_of = lambda g: [r.get("unrecovered") for r in g if r.get("unrecovered") is not None]
        base = {n: statistics.mean(un_of(groups[(clean, n)])) if un_of(groups.get((clean, n), [])) else 0.0
                for n in names}
        for prof in order:
            un = {n: un_of(groups[(prof, n)]) for n in names}
            for a in arms:
                n = a[0]
                xs, g = un[n], groups[(prof, n)]

                def against(other):
                    if other is None or other == n or not xs or not un.get(other):
                        return None, None
                    return statistics.mean(xs) - statistics.mean(un[other]), welch(xs, un[other])

                d_srt, t_srt = against(srt)
                d_base, t_base = against(baseline) if a[1] == "whip" else (None, None)
                sent = [r["frames_sent"] for r in g if r.get("frames_sent") and r.get("unrecovered") is not None]
                share = 100 * sum(xs) / sum(sent) if sent and sum(sent) else None
                loads = [r["load"] for r in g] + [r["load_end"] for r in g if "load_end" in r]
                f.write(f"{prof[0]}\t{prof[1]}\t{'keep' if prof[2] else 'reorder'}\t{n}\t{a[1]}\t{len(g)}\t"
                        f"{fmt(statistics.mean(xs)) if xs else ''}\t"
                        f"{fmt(statistics.stdev(xs)) if len(xs) > 1 else ''}\t{' '.join(str(x) for x in xs)}\t"
                        f"{fmt(share)}\t{fmt(statistics.mean(xs) - base[n]) if xs else ''}\t"
                        f"{fmt(d_srt)}\t{fmt(t_srt, 2)}\t{fmt(d_base)}\t{fmt(t_base, 2)}\t"
                        f"{sum(1 for r in g if not r.get('ready'))}\t"
                        f"{f'{min(loads)} to {max(loads)}' if loads else ''}\t"
                        f"{' '.join(str(r.get('proxy_retransmits', '')) for r in g)}\t"
                        f"{' '.join(str(r.get('proxy_reordered', '')) for r in g)}"
                        + "".join(f"\t{fmt(d)}\t{fmt(t, 2)}" for d, t in (against(x) for x in more_srt)) + "\n")
    print((args.out / "summary.tsv").read_text())
    return 0


if __name__ == "__main__":
    sys.exit(main())
