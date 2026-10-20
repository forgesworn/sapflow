#!/usr/bin/env python3
"""Checks an LL-HLS media playlist against Apple's part rules.

  python3 -I scripts/check-playlist.py URL [SECONDS]   poll a live playlist
  python3 -I scripts/check-playlist.py FILE...         check saved renders

Rules (RFC 8216bis 4.4.3.1, 4.4.3.3, 4.4.3.7, 4.4.4.9; docs/reference.md),
the same as sapflow_hls::playlist::RuleCheck:
  - TARGETDURATION and PART-TARGET never change while the stream runs;
  - every EXTINF, rounded to the nearest second, is at most TARGETDURATION;
  - no EXT-X-PART DURATION is above PART-TARGET;
  - every part but the last of its segment is at least 85% of PART-TARGET;
  - EXT-X-DISCONTINUITY never sits among a segment's parts, and each media
    sequence number keeps one discontinuity sequence number across renders.
The last part listed after the final EXTINF is exempt: it may yet be the
last of its segment. Durations are compared as the decimals printed, as a
client reads them. Exits 1 if any render breaks a rule.
"""
import sys
import time
import urllib.request
from decimal import Decimal


def check(text):
    """One render: (headers, [(uri, duration)], {msn: dseq}, [extinf], [violations])."""
    head = {}
    parts, run, bad, dseqs, extinfs = [], [], [], {}, []
    msn = dseq = None
    for line in text.splitlines():
        for tag in ("#EXT-X-TARGETDURATION:", "#EXT-X-MEDIA-SEQUENCE:", "#EXT-X-DISCONTINUITY-SEQUENCE:"):
            if line.startswith(tag):
                head[tag] = int(line[len(tag):])
        if line.startswith("#EXT-X-PART-INF:"):
            for attr in line.split(":", 1)[1].split(","):
                k, _, v = attr.partition("=")
                if k == "PART-TARGET":
                    head["PART-TARGET"] = Decimal(v)
        elif line.startswith("#EXT-X-PART:"):
            attrs = dict(a.partition("=")[::2] for a in line.split(":", 1)[1].split(","))
            run.append((attrs.get("URI", "").strip('"'), Decimal(attrs["DURATION"])))
        elif line == "#EXT-X-DISCONTINUITY":
            if msn is None:
                msn = head.get("#EXT-X-MEDIA-SEQUENCE:", 0)
                dseq = head.get("#EXT-X-DISCONTINUITY-SEQUENCE:", 0)
            if run:
                bad.append(f"EXT-X-DISCONTINUITY among the parts of segment {msn}")
            dseq += 1
        elif line.startswith("#EXTINF:"):
            if msn is None:
                msn = head.get("#EXT-X-MEDIA-SEQUENCE:", 0)
                dseq = head.get("#EXT-X-DISCONTINUITY-SEQUENCE:", 0)
            d = Decimal(line[len("#EXTINF:"):].rstrip(","))
            extinfs.append(d)
            target = head.get("#EXT-X-TARGETDURATION:")
            if target is not None and int(d + Decimal("0.5")) > target:
                bad.append(f"segment {msn} lasts {d}, over TARGETDURATION {target}")
            dseqs[msn] = dseq
            msn += 1
            parts.append((run, False))
            run = []
    parts.append((run, True))
    target = head.get("PART-TARGET")
    if target is None:
        return head, [], dseqs, extinfs, ["no PART-TARGET"]
    seen = []
    for run, open_segment in parts:
        for i, (uri, d) in enumerate(run):
            seen.append((uri, d))
            if d > target:
                bad.append(f"{uri} lasts {d}, over PART-TARGET {target}")
            last = i == len(run) - 1
            if not last and d < target * Decimal("0.85"):
                where = "open segment" if open_segment else "segment"
                bad.append(f"{uri} lasts {d}, under 85% of PART-TARGET {target} (not last of its {where})")
    return head, seen, dseqs, extinfs, bad


def renders(args):
    if args[0].startswith(("http://", "https://")):
        url = args[0]
        end = time.monotonic() + float(args[1] if len(args) > 1 else 30)
        while time.monotonic() < end:
            try:
                with urllib.request.urlopen(url, timeout=5) as r:
                    yield r.read().decode()
            except OSError as e:
                print(f"fetch failed: {e}", file=sys.stderr)
            time.sleep(0.1)
    else:
        for path in args:
            with open(path) as f:
                yield f.read()


def changes(values):
    out = []
    for v in values:
        if not out or out[-1] != v:
            out.append(v)
    return out


def main(args):
    if not args:
        print(__doc__, file=sys.stderr)
        return 2
    part_targets, targets, durations, problems, count = [], [], {}, [], 0
    dseq_of, extinfs, discontinuities = {}, set(), set()
    for text in renders(args):
        if "#EXT-X-PART" not in text:
            continue
        count += 1
        head, seen, dseqs, ext, bad = check(text)
        part_targets.append(head.get("PART-TARGET"))
        targets.append(head.get("#EXT-X-TARGETDURATION:"))
        extinfs.update(ext)
        for uri, d in seen:
            durations[uri] = d
        for msn, d in dseqs.items():
            if dseq_of.setdefault(msn, d) != d:
                bad.append(f"segment {msn} moved from discontinuity sequence {dseq_of[msn]} to {d}")
            if msn - 1 in dseq_of and dseq_of[msn - 1] != d:
                discontinuities.add(msn)
        for b in bad:
            if b not in problems:
                problems.append(b)
    part_targets, targets = changes(part_targets), changes(targets)
    if len(part_targets) > 1:
        problems.insert(0, "PART-TARGET changed: " + " -> ".join(map(str, part_targets)))
    if len(targets) > 1:
        problems.insert(0, "TARGETDURATION changed: " + " -> ".join(map(str, targets)))
    ds = sorted(durations.values())
    print(f"renders {count}, parts {len(ds)}, segments {len(dseq_of)}, discontinuities {len(discontinuities)}")
    print(f"TARGETDURATION {' -> '.join(map(str, targets)) or 'none'}, PART-TARGET {' -> '.join(map(str, part_targets)) or 'none'}")
    if ds:
        print(f"part durations: shortest {ds[0]}, longest {ds[-1]}; longest EXTINF {max(extinfs) if extinfs else 'none'}")
    for p in problems[:20]:
        print("FAIL", p)
    if len(problems) > 20:
        print(f"FAIL ... {len(problems) - 20} more")
    if count == 0:
        print("FAIL no LL-HLS playlist fetched")
        return 1
    if not problems:
        print("ok   every render keeps the rules")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
