#!/usr/bin/env python3
"""Retrieve only the pinned public RelaySwarm release, with digest checks."""
from pathlib import Path
import hashlib
import json
import subprocess
import tarfile

root = Path(__file__).resolve().parents[2]
pins = json.loads((root / 'tools/tv-relayswarm/pins.json').read_text())
destination = root / 'target/tv-relayswarm'
destination.mkdir(parents=True, exist_ok=True)
for name, asset in pins['assets'].items():
    path = destination / name
    if not path.exists():
        subprocess.run(['curl', '--fail', '--location', '--retry', '2', '--output', str(path), asset['url']], check=True)
    if hashlib.sha256(path.read_bytes()).hexdigest() != asset['sha256']:
        raise SystemExit(f'Pinned release digest mismatch: {name}')
with tarfile.open(destination / f"relayswarm-{pins['version']}.tgz") as archive:
    archive.extractall(destination / 'release', filter='data')
print('Pinned RelaySwarm release verified')
