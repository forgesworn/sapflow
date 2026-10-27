#!/usr/bin/env python3
"""Finite mutation fuzz target for the Python preview policy, not coverage-guided.

Run: python3 -B fuzz/tv_preview_fuzz.py [iterations]. Optional external engines
can import TestOneInput. No sockets, subprocesses, media or private data are used.
"""
from pathlib import Path
import random
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]/'scripts'))
from tv_preview_policy import checked_manifest, preview_manifest, media_target, MAX_MANIFEST


def TestOneInput(data):
    if len(data) > MAX_MANIFEST + 1:
        return
    try:
        result = checked_manifest(data)
    except (ValueError, UnicodeError):
        pass
    else:
        assert result is data and len(result) <= MAX_MANIFEST
        preview = preview_manifest(data)
        assert b'://' not in preview and b'\\' not in preview
        assert b'://' not in result and b'\\' not in result and b'//' not in result
    try:
        value = data.decode('utf-8')
    except UnicodeError:
        return
    try:
        accepted = media_target(value)
    except ValueError:
        return
    if accepted:
        assert value.startswith('/tv/live/event-tv/')
        assert '://' not in value and '..' not in value and '\\' not in value


def main():
    rng=random.Random(20261027)
    seeds=[b'#EXTM3U\nstream.m3u8\n', b'#EXTM3U\n#EXT-X-MAP:URI="012345abcdef_init.mp4"\n',
           b'#EXTM3U\n#EXT-X-PART:DURATION=0.2,URI="012345abcdef_part1.mp4"\n',
           b'/tv/live/event-tv/stream.m3u8?_HLS_msn=17&_HLS_part=1',
           b'#EXTM3U\n#EXT-X-KEY:URI="https://evil.example/"\n', b'\xff', b'#EXTM3U\n'+b'A'*MAX_MANIFEST]
    count=int(sys.argv[1]) if len(sys.argv)>1 else 20000
    for i in range(count):
        value=bytearray(rng.choice(seeds))
        for _ in range(rng.randrange(1,6)):
            pos=rng.randrange(len(value)+1)
            mode=rng.randrange(3)
            if mode==0: value[pos:pos]=rng.randbytes(rng.randrange(1,20))
            elif mode==1 and pos<len(value): value[pos]=rng.randrange(256)
            else: del value[pos:pos+rng.randrange(1,20)]
        TestOneInput(bytes(value))
    print(f'tv_preview mutation executions={count}; coverage_guided=false')


if __name__=='__main__':main()
