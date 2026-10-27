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
ident = json.loads((assets / 'loading-ident.build.json').read_text())
assert ident['model'] == 'seedance_2_5' and ident['audio'] is False
assert ident['sourceArtworkSha256'] == hashlib.sha256((assets / 'off-air.png').read_bytes()).hexdigest()
assert ident['sha256'] == hashlib.sha256((assets / 'loading-ident.mp4').read_bytes()).hexdigest()
assert (assets / ident['prompt']).is_file() and (assets / 'loading-ident.mp4').stat().st_size < 2*1024*1024
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
inventory = json.loads((root / 'tools/tv-relayswarm/wasm-licences.json').read_text())
assert inventory['wasmSha256'] == pins['assets']['relayswarm-core-0.4.4.wasm']['sha256']
for package in inventory['packages']:
    assert package['name'] + ' ' + package['version'] in notices
assert 'BSD-3-Clause' in notices and 'BSL-1.0' in notices
assert not (assets / 'vendor/hls.light.min.mjs').exists()
print('TV artwork, RelaySwarm integrity and distributed licence files verified')
