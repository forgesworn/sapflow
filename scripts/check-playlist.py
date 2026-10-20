#!/usr/bin/env python3
"""Checks an LL-HLS media playlist against Apple's part rules.

  python3 -I scripts/check-playlist.py URL [SECONDS]   poll a live playlist
  python3 -I scripts/check-playlist.py FILE...         check saved renders

Rules (RFC 8216bis 4.4.3.7 and 4.4.4.9, docs/reference.md):
  - PART-TARGET never changes while the stream runs;
  - no EXT-X-PART DURATION is above PART-TARGET;
  - every part but the last of its segment is at least 85% of PART-TARGET.
The last part listed after the final EXTINF is exempt: it may yet be the
last of its segment. Durations are compared as the decimals printed, as a
client reads them. Exits 1 if any render breaks a rule.
"""
import sys
import time
import urllib.request
from decimal import Decimal


def check(text):
    """Returns (part target, [(uri, duration)], [violations]) for one render."""
    target = None
    parts, run, bad = [], [], []
    for line in text.splitlines():
        if line.startswith("#EXT-X-PART-INF:"):
            for attr in line.split(":", 1)[1].split(","):
                k, _, v = attr.partition("=")
                if k == "PART-TARGET":
                    target = Decimal(v)
        elif line.startswith("#EXT-X-PART:"):
            attrs = dict(a.partition("=")[::2] for a in line.split(":", 1)[1].split(","))
            run.append((attrs.get("URI", "").strip('"'), Decimal(attrs["DURATION"])))
        elif line.startswith("#EXTINF:"):
            parts.append((run, False))
            run = []
    parts.append((run, True))
    if target is None:
        return None, [], ["no PART-TARGET"]
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
    return target, seen, bad


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


def main(args):
    if not args:
        print(__doc__, file=sys.stderr)
        return 2
    targets, durations, problems, count = [], {}, [], 0
    for text in renders(args):
        if "#EXT-X-PART" not in text:
            continue
        count += 1
        target, seen, bad = check(text)
        if target is not None and (not targets or targets[-1] != target):
            targets.append(target)
        for uri, d in seen:
            durations[uri] = d
        for b in bad:
            if b not in problems:
                problems.append(b)
    if len(targets) > 1:
        problems.insert(0, "PART-TARGET changed: " + " -> ".join(str(t) for t in targets))
    ds = sorted(durations.values())
    print(f"renders {count}, parts {len(ds)}, PART-TARGET {' -> '.join(map(str, targets)) or 'none'}")
    if ds:
        print(f"part durations: shortest {ds[0]}, longest {ds[-1]}")
    for p in problems[:20]:
        print("FAIL", p)
    if len(problems) > 20:
        print(f"FAIL ... {len(problems) - 20} more")
    if count == 0:
        print("FAIL no LL-HLS playlist fetched")
        return 1
    if not problems:
        print("ok   every render keeps the part rules")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
