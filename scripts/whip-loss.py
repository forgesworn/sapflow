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
  handshake impaired), latency 200 ms on both ends.
- WHIP: through the proxy (--protocol whip: only SRTP and SRTCP impaired),
  the answer's only candidate being the proxy. Sapflow's jitter wait is its
  compiled default, 200 ms (SAPFLOW_WHIP_JITTER is a proposal, not a
  setting). The publisher (--whip-publisher) is headless Chrome by default:
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
        [--base 47300]

Writes DIR/trials.tsv and DIR/summary.tsv. Every child is the driver's own
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
      frameWidth: x.frameWidth, framesPerSecond: x.framesPerSecond, qualityLimitationReason: x.qualityLimitationReason }); });
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


def trial(args, protocol, loss, seed, out):
    tag = f"{protocol}-{'clean' if loss == 0 else f'loss{loss}'}-s{seed}"
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
           "RUST_LOG": "info,sapflow::whip=debug"}
    row = {"protocol": protocol, "loss_pct": loss, "seed": seed, "jitter_ms": 0 if loss == 0 else args.jitter_ms,
           "delay_ms": args.delay_ms, "load": round(os.getloadavg()[0], 2)}
    slog = open(d / "sapflow.log", "w")
    srv = subprocess.Popen([args.bin], env=env, stdout=slog, stderr=slog)
    procs = []
    try:
        time.sleep(1)
        target = port["srt"] if protocol == "srt" else port["media"]
        proxy = subprocess.Popen([args.proxy, "--listen", f"127.0.0.1:{port['proxy']}", "--target", f"127.0.0.1:{target}",
                                  "--loss", "none" if loss == 0 else f"random:{loss}", "--delay-ms", str(args.delay_ms),
                                  "--jitter-ms", str(0 if loss == 0 else args.jitter_ms), "--protocol", protocol,
                                  "--latency-ms", str(LATENCY_MS), "--seed", str(seed), "--stats", str(d / "proxy.txt")],
                                 stdout=subprocess.DEVNULL, stderr=open(d / "proxy.log", "w"))
        procs.append(proxy)
        time.sleep(0.5)
        if protocol == "srt":
            snd = subprocess.Popen(["srt-live-transmit", "-a:no", "-c:1316", f"udp://127.0.0.1:{port['udp']}",
                                    f"srt://127.0.0.1:{port['proxy']}?latency={LATENCY_MS}&streamid=publish:live/event-loss:{user}:{pw}"],
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
            time.sleep(LATENCY_MS / 1000 + 2)
            stop(snd)
        elif args.whip_publisher == "chrome":
            report = chrome_publish(d, f"http://127.0.0.1:{port['whip']}/whip/live/event-loss", f"{user}:{pw}", port["page"])
            (d / "chrome-report.json").write_text(json.dumps(report, indent=1))
            row["publisher_exit"] = 0 if report and not report.get("errors") else 1
            row["frames_sent"] = report.get("framesSent")
            for k in ("keyFramesEncoded", "nackCount", "pliCount", "retransmittedPacketsSent", "packetsSent"):
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
    text = (d / "sapflow.log").read_text(errors="replace")
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    if protocol == "whip":
        last = re.findall(r"WHIP receive counters (.*)$", text, re.M)
        if last:
            for k, v in re.findall(r"(\w+)=(\d+)", last[-1]):
                if k in ("video_nacked", "video_recovered", "video_lost", "video_incomplete", "video_skipped"):
                    row[f"sapflow_{k}"] = int(v)
    else:
        m = re.findall(r"SRT connection closed .*?received=(\d+) retransmitted=(\d+) lost=(\d+) dropped=(\d+)", text)
        if m:
            r_, t_, l_, dr = m[-1]
            row.update({"sapflow_srt_received": int(r_), "sapflow_srt_retransmitted": int(t_),
                        "sapflow_srt_lost": int(l_), "sapflow_srt_dropped": int(dr)})
    print(json.dumps(row), flush=True)
    return row


def welch(a, b):
    if len(a) < 2 or len(b) < 2:
        return None
    va, vb = statistics.variance(a), statistics.variance(b)
    se = math.sqrt(va / len(a) + vb / len(b))
    return None if se == 0 else (statistics.mean(a) - statistics.mean(b)) / se


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
    ap.add_argument("--whip-publisher", default="chrome", choices=["chrome", "ffmpeg"])
    ap.add_argument("--base", type=int, default=47300)
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    clips(args.out)
    protocols = args.protocols.split(",")
    losses = [int(x) for x in args.losses.split(",")]
    plan = []
    for s in range(1, args.clean + 1):
        for p in protocols:
            plan.append((p, 0, s))
    for loss in losses:
        for s in range(1, args.seeds + 1):
            # Back to back per seed, the order flipping each seed.
            for p in (protocols if s % 2 else list(reversed(protocols))):
                plan.append((p, loss, s))
    rows = [trial(args, p, loss, s, args.out) for p, loss, s in plan]
    keys = sorted({k for r in rows for k in r}, key=lambda k: (k not in ("protocol", "loss_pct", "seed"), k))
    with open(args.out / "trials.tsv", "w") as f:
        f.write("\t".join(keys) + "\n")
        for r in rows:
            f.write("\t".join(str(r.get(k, "")) for k in keys) + "\n")
    with open(args.out / "summary.tsv", "w") as f:
        f.write("loss_pct\tprotocol\truns\tunrecovered_mean\tunrecovered_sd\tunrecovered\tvs_clean_mean\t"
                "whip_minus_srt\twelch_t\tnever_ready\n")
        groups = {}
        for loss in [0] + losses:
            for p in protocols:
                groups[(loss, p)] = [r for r in rows if r["protocol"] == p and r["loss_pct"] == loss]
        clean = {p: statistics.mean([r.get("unrecovered", 0) for r in groups[(0, p)]]) if groups[(0, p)] else 0.0
                 for p in protocols}
        for loss in [0] + losses:
            un = {p: [r.get("unrecovered") for r in groups[(loss, p)] if r.get("unrecovered") is not None] for p in protocols}
            both = len(protocols) == 2 and un.get("whip") and un.get("srt")
            t = welch(un["whip"], un["srt"]) if both else None
            diff = statistics.mean(un["whip"]) - statistics.mean(un["srt"]) if both else None
            for p in protocols:
                xs = un[p]
                f.write(f"{loss}\t{p}\t{len(groups[(loss, p)])}\t{round(statistics.mean(xs), 1) if xs else ''}\t"
                        f"{round(statistics.stdev(xs), 1) if len(xs) > 1 else ''}\t{' '.join(str(x) for x in xs)}\t"
                        f"{round(statistics.mean(xs) - clean[p], 1) if xs else ''}\t"
                        f"{round(diff, 1) if diff is not None else ''}\t{round(t, 2) if t is not None else ''}\t"
                        f"{sum(1 for r in groups[(loss, p)] if not r.get('ready'))}\n")
    print((args.out / "summary.tsv").read_text())
    return 0


if __name__ == "__main__":
    sys.exit(main())
