"""Bounded allowlist for the single Sapflow TV stream used by private preview.

This deliberately accepts the emitted Sapflow playlist subset only. It does
not fetch URLs, rewrite arbitrary HLS or support encrypted/steered playlists.
"""

import re
from urllib.parse import parse_qsl, urlsplit

MAX_MANIFEST = 65536
MEDIA_NAME = re.compile(r"[a-f0-9]{12}_(?:init[0-9]{0,20}|(?:seg|part)[0-9]{1,20})\.mp4")
PLAYLIST_NAME = re.compile(r"(?:index|stream)\.m3u8")
ROOT = "/tv/live/event-tv/"
TAGS = frozenset({"#EXTM3U", "#EXT-X-VERSION", "#EXT-X-INDEPENDENT-SEGMENTS",
                  "#EXT-X-STREAM-INF", "#EXT-X-TARGETDURATION", "#EXT-X-SERVER-CONTROL",
                  "#EXT-X-PART-INF", "#EXT-X-MEDIA-SEQUENCE", "#EXT-X-DISCONTINUITY-SEQUENCE",
                  "#EXT-X-MAP", "#EXT-X-PART", "#EXT-X-PRELOAD-HINT", "#EXTINF",
                  "#EXT-X-GAP", "#EXT-X-PROGRAM-DATE-TIME", "#EXT-X-DISCONTINUITY", "#EXT-X-ENDLIST"})
URI_TAGS = frozenset({"#EXT-X-MAP", "#EXT-X-PART", "#EXT-X-PRELOAD-HINT"})


def media_name(value):
    return value == "gap.mp4" or bool(MEDIA_NAME.fullmatch(value) or PLAYLIST_NAME.fullmatch(value))


def media_target(value):
    if len(value) > 2048 or any(ord(c) < 33 or ord(c) > 126 for c in value):
        return False
    parsed = urlsplit(value)
    if parsed.scheme or parsed.netloc or parsed.fragment or not parsed.path.startswith(ROOT):
        return False
    name = parsed.path[len(ROOT):]
    if not media_name(name):
        return False
    if parsed.query and not PLAYLIST_NAME.fullmatch(name):
        return False
    pairs = parse_qsl(parsed.query, keep_blank_values=True, max_num_fields=3)
    if len({key for key, _ in pairs}) != len(pairs):
        return False
    for key, val in pairs:
        if key in ("_HLS_msn", "_HLS_part"):
            if not re.fullmatch(r"[0-9]{1,20}", val):
                return False
        elif key == "_HLS_skip":
            if val not in ("YES", "v2"):
                return False
        else:
            return False
    return True


def checked_manifest(data):
    if len(data) > MAX_MANIFEST:
        raise ValueError("manifest budget exceeded")
    text = data.decode("ascii")
    if not text.startswith("#EXTM3U\n") or any(not 32 <= ord(c) <= 126 and c != "\n" for c in text):
        raise ValueError("unexpected playlist framing")
    for line in text.splitlines():
        if not line:
            continue
        if "://" in line or "\\" in line or "//" in line:
            raise ValueError("unexpected network reference")
        if not line.startswith("#"):
            if not media_name(line):
                raise ValueError("external or unknown playlist resource")
            continue
        tag = line.partition(":")[0]
        if tag not in TAGS:
            raise ValueError("unsupported playlist feature")
        if tag in URI_TAGS:
            matches = re.findall(r'(?:[:,])URI="([^"\n]*)"', line)
            if len(matches) != 1 or line.count("URI") != 1 or not MEDIA_NAME.fullmatch(matches[0]):
                raise ValueError("unsafe playlist resource")
        elif "URI" in line or "://" in line or "\\" in line:
            raise ValueError("unexpected playlist reference")
    return data


def preview_manifest(data):
    checked_manifest(data)
    # Tor setup latency can exceed a 200 ms part's useful loading window. The
    # preview deliberately uses complete segments; public playback stays LL-HLS.
    omit = (b'#EXT-X-SERVER-CONTROL:', b'#EXT-X-PART-INF:', b'#EXT-X-PART:', b'#EXT-X-PRELOAD-HINT:')
    lines = [line for line in data.splitlines() if not line.startswith(omit)]
    if b'#EXT-X-STREAM-INF:' not in data:
        lines.insert(1, b'#EXT-X-START:TIME-OFFSET=-6.0,PRECISE=NO')
    return b'\n'.join(lines) + b'\n'
