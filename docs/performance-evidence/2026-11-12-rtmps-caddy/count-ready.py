import sys, urllib.request
n = int(sys.argv[1]); ok = 0
for i in range(1, n + 1):
    try:
        with urllib.request.urlopen(f"http://cap-srv:8888/live/event-cost-{i}/stream.m3u8", timeout=1) as r:
            ok += b"#EXTINF" in r.read()
    except Exception:
        pass
print(ok)
