#!/usr/bin/env python3
"""How many SRT and RTMP streams a sapflow in a small Linux container takes,
and what happens at the edge (docs/benchmarks.md, "Ingest memory in a small
container"). Starts sapflow alone in a container with the reference
deployment's limits (1 CPU, 384 MiB, 128 pids) and settings, then a second
container of publishers (scripts/ingest-capacity-publishers.py), and samples
the server's cgroup once a second: memory, out-of-memory kills, CPU
throttling and UDP receive-buffer drops. Writes SCRATCH/out/LABEL/
{samples.tsv,result.json,server.log,events.jsonl}. Only touches containers
named cap-*, the network capnet and the volume cap-rec.

    scripts/ingest-capacity-linux.sh SCRATCH LABEL [options] -- [publisher options]

runs it with a Linux build; --mem 1g, --env KEY=VALUE for the server, and
after -- the publishers' --profile, --srt, --rtmp, --heavy-rtmp, --step,
--hold and --reconnect."""
import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

D = shutil.which("docker") or sys.exit("docker is not on PATH")
IMG = "sapflow-cost-linux:local"
# The reference deployment's settings that bear on memory (2026-11-12), with
# open publishing in place of its auth URL and no callbacks.
REFERENCE_ENV = {
    "SAPFLOW_OPEN_PUBLISH": "1",
    "SAPFLOW_SRT_ADDR": "0.0.0.0:8890",
    "SAPFLOW_RTMP_ADDR": "0.0.0.0:1935",
    "SAPFLOW_HTTP_ADDR": "0.0.0.0:8888",
    "SAPFLOW_SEGMENT_COUNT": "24",
    "SAPFLOW_RECONNECT_GRACE": "90",
    "SAPFLOW_RECORDINGS": "/recordings",
    "SAPFLOW_HLS_MODE": "whole",
    "SAPFLOW_SRT_ENCRYPTION": "optional",
    "SAPFLOW_SRT_PASSPHRASE": "capacity-test-passphrase",
    "SAPFLOW_SRT_PBKEYLEN": "32",
    "SAPFLOW_HEALTH_INTERVAL": "0",
    "RUST_LOG": "info",
}


def d(*args, **kw):
    return subprocess.run([D, *args], capture_output=True, text=True, **kw)


def sample():
    r = d("exec", "cap-srv", "sh", "-c",
          "cat /sys/fs/cgroup/memory.current; cat /sys/fs/cgroup/memory.peak 2>/dev/null || echo -1;"
          " awk '$1==\"oom_kill\"{print $2}' /sys/fs/cgroup/memory.events;"
          " awk '$1==\"usage_usec\"{print $2}' /sys/fs/cgroup/cpu.stat;"
          " awk '$1==\"anon\"||$1==\"file\"{print $2}' /sys/fs/cgroup/memory.stat;"
          " ps -o rss= -p 1;"
          " awk '$1==\"nr_throttled\"||$1==\"throttled_usec\"{print $2}' /sys/fs/cgroup/cpu.stat;"
          " awk '/^Udp: [0-9]/{print $6}' /proc/net/snmp")
    if r.returncode != 0:
        return None
    v = r.stdout.split()
    if len(v) < 10 or not all(x.lstrip("-").isdigit() for x in v[:10]):
        return None  # the container died while being sampled
    return dict(cur=int(v[0]), peak=int(v[1]), oom_kill=int(v[2]), cpu_us=int(v[3]),
                anon=int(v[4]), file=int(v[5]), rss_kb=int(v[6]), throttled=int(v[7]),
                throttled_us=int(v[8]), udp_rcvbuf_err=int(v[9]))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--scratch", required=True, type=Path)
    ap.add_argument("--label", required=True)
    ap.add_argument("--mem", default="384m")
    ap.add_argument("--bin", default=None, help="server binary (default SCRATCH/target/release/sapflow)")
    ap.add_argument("--image", default=IMG, help="image with python3, procps and srt-tools")
    ap.add_argument("--cpus", default="1")
    ap.add_argument("--env", action="append", default=[], help="extra server KEY=VALUE")
    ap.add_argument("pub", nargs=argparse.REMAINDER, help="-- then ingest-capacity-publishers.py options")
    a = ap.parse_args()
    out = a.scratch / "out" / a.label
    out.mkdir(parents=True, exist_ok=True)
    env = dict(REFERENCE_ENV)
    for kv in a.env:
        k, v = kv.split("=", 1)
        env[k] = v
    for c in ("cap-pub", "cap-srv"):
        d("rm", "-f", c)
    d("volume", "rm", "cap-rec")
    if d("network", "inspect", "capnet").returncode != 0:
        d("network", "create", "capnet", check=True)
    srv = [D, "run", "-d", "--name", "cap-srv", "--network", "capnet",
           "--cpus", a.cpus, "--memory", a.mem, "--memory-swap", a.mem,
           "--pids-limit", "128", "--ulimit", "nofile=65536:65536",
           "-v", f"{a.bin or (str(a.scratch) + '/target/release/sapflow')}:/usr/local/bin/sapflow:ro",
           "-v", "cap-rec:/recordings"]
    for k, v in env.items():
        srv += ["-e", f"{k}={v}"]
    subprocess.run([*srv, a.image, "/usr/local/bin/sapflow"], check=True, capture_output=True)
    pub_args = [x for x in a.pub if x != "--"]
    subprocess.run([D, "run", "-d", "--name", "cap-pub", "--network", "capnet",
                    "-v", f"{a.scratch}/clips:/clips", "-v", f"{out}:/out",
                    "-v", f"{a.scratch}/ffmpeg:/ff:ro", "-v", f"{Path(__file__).parent}:/s:ro",
                    "-v", "cap-rec:/rec:ro", "-e", "PATH=/ff/bin:/usr/local/bin:/usr/bin:/bin",
                    a.image, "python3", "-B", "/s/ingest-capacity-publishers.py",
                    "--passphrase", env["SAPFLOW_SRT_PASSPHRASE"],
                    *pub_args], check=True, capture_output=True)
    t0 = time.monotonic()
    rows = []
    with open(out / "samples.tsv", "w") as f:
        f.write("t\tcur_mib\tpeak_mib\tanon_mib\tfile_mib\trss_mib\toom_kill\tcpu_pct\tnr_throttled\tthrottled_ms\tudp_rcvbuf_err\n")
        last = None
        while d("inspect", "-f", "{{.State.Running}}", "cap-pub").stdout.strip() == "true":
            s = sample()
            t = time.monotonic() - t0
            if s:
                cpu = "" if not last else round((s["cpu_us"] - last[1]["cpu_us"]) / 1e4 / (t - last[0]), 1)
                f.write(f"{t:.1f}\t{s['cur']/2**20:.1f}\t{s['peak']/2**20:.1f}\t{s['anon']/2**20:.1f}"
                        f"\t{s['file']/2**20:.1f}\t{s['rss_kb']/1024:.1f}\t{s['oom_kill']}\t{cpu}\t{s['throttled']}"
                        f"\t{s['throttled_us']//1000}\t{s['udp_rcvbuf_err']}\n")
                f.flush()
                rows.append((t, s))
                last = (t, s)
            else:
                f.write(f"{t:.1f}\tserver-gone\n")
                f.flush()
            time.sleep(1)
    state = d("inspect", "-f", "{{.State.Status}} {{.State.OOMKilled}} {{.State.ExitCode}} {{.RestartCount}}",
              "cap-srv").stdout.strip()
    (out / "server.log").write_text(d("logs", "cap-srv").stdout + d("logs", "cap-srv").stderr)
    pub_log = d("logs", "cap-pub").stdout.strip().splitlines()
    (out / "pub.json").write_text(pub_log[-1] if pub_log else "{}")
    rec = d("run", "--rm", "-v", "cap-rec:/rec:ro", a.image, "sh", "-c",
            "find /rec -type f | wc -l; du -sm /rec | cut -f1").stdout.split()
    final = rows[-1][1] if rows else {}
    result = dict(label=a.label, bin=a.bin, env={k: v for k, v in env.items() if "PASSPHRASE" not in k},
                  mem=a.mem, cpus=a.cpus, server_state=state,
                  peak_mib=round(max((r[1]["peak"] for r in rows), default=0) / 2**20, 1),
                  max_cur_mib=round(max((r[1]["cur"] for r in rows), default=0) / 2**20, 1),
                  max_anon_mib=round(max((r[1]["anon"] for r in rows), default=0) / 2**20, 1),
                  oom_kill=final.get("oom_kill"), recordings=rec,
                  udp_rcvbuf_err=final.get("udp_rcvbuf_err"), nr_throttled=final.get("throttled"),
                  throttled_ms=(final.get("throttled_us") or 0) // 1000,
                  pub=json.loads(pub_log[-1]) if pub_log and pub_log[-1].startswith("{") else pub_log[-5:],
                  load=" ".join(f"{x:.2f}" for x in os.getloadavg()))
    (out / "result.json").write_text(json.dumps(result, indent=1))
    print(json.dumps(result))
    for c in ("cap-pub", "cap-srv"):
        d("rm", "-f", c)
    d("volume", "rm", "cap-rec")


if __name__ == "__main__":
    main()
