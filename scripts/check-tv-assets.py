#!/usr/bin/env python3
"""Check the pinned third-party player and distributed licence files."""
from pathlib import Path
import hashlib
import json

root=Path(__file__).resolve().parents[1]/'web/tv/vendor'
pin=json.loads((root/'hls.build.json').read_text())
assert pin['version']=='1.7.3'
assert hashlib.sha256((root/'hls.light.min.mjs').read_bytes()).hexdigest()==pin['sha256']
assert (root/'hls.js-LICENSE').stat().st_size>1000
assert 'Apache License' in (root/'APACHE-2.0.txt').read_text()
print('TV player integrity and licence files verified')
