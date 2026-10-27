#!/usr/bin/env python3
"""Verify TV artwork, pinned RelaySwarm files and recipient-visible licences."""
from pathlib import Path
import hashlib
import json

root = Path(__file__).resolve().parents[1]
assets = root / 'web/tv'
for name in ['off-air', 'now-streaming']:
    pin = json.loads((assets / f'{name}.build.json').read_text())
    assert pin['model'] == 'gpt-image-2.5-sunburst'
    assert hashlib.sha256((assets / f'{name}.png').read_bytes()).hexdigest() == pin['sha256']
    assert (assets / pin['prompt']).is_file()
pins = json.loads((root / 'tools/tv-relayswarm/pins.json').read_text())
for name, pin in pins['assets'].items():
    if name.endswith('.tgz'):
        continue
    assert hashlib.sha256((assets / 'vendor' / name).read_bytes()).hexdigest() == pin['sha256']
assert (assets / 'LICENSE.txt').read_bytes() == (root / 'LICENSE').read_bytes()
assert 'MIT' in (assets / 'vendor/relayswarm-LICENSE').read_text()
notices = (assets / 'vendor/THIRD-PARTY-LICENCES.txt').read_text()
for name in ['@noble/hashes', '@noble/curves', '@noble/ciphers', '@scure/base', 'nostr-tools', 'ws']:
    assert name in notices
assert not (assets / 'vendor/hls.light.min.mjs').exists()
print('TV artwork, RelaySwarm integrity and distributed licence files verified')
