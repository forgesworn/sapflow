#!/usr/bin/env python3
"""Collect licence choices for the pinned WASM's normal Cargo dependency graph.

This reads release sources and cached registry metadata; it never builds Rust.
Run after fetch.py. Dual-licensed packages select MIT where offered.
"""
from pathlib import Path
import hashlib
import json
import os
import re
import subprocess
import sys
import tomllib

root = Path(__file__).resolve().parents[2]
here = root / 'tools/tv-relayswarm'
manifest = root / 'target/tv-relayswarm/release/package/core/Cargo.toml'
tree = subprocess.check_output(['cargo', 'tree', '--locked', '--manifest-path', str(manifest),
    '-p', 'relayswarm-wasm', '--target', 'wasm32-unknown-unknown', '--edges', 'normal',
    '--prefix', 'none', '--format', '{p}'], text=True)
registry = Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo')) / 'registry/src'
packages = {}
for line in tree.splitlines():
    match = re.match(r'([^ ]+) v([^ ]+)', line)
    if match and not match[1].startswith('relayswarm-'):
        packages[(match[1], match[2])] = None
notices = ['RelaySwarm 0.4.4 WebAssembly core runtime dependencies\n'
           'MIT is selected where the upstream package offers a dual licence.\n']
inventory = []
for name, version in sorted(packages):
    directory = next(registry.glob(f'*/{name}-{version}'))
    package = tomllib.loads((directory / 'Cargo.toml').read_text())['package']
    expression = package['license']
    if 'MIT' in re.split(r'\s+|/', expression):
        choice = 'MIT'
        files = ['LICENSE-MIT', 'LICENSE']
    elif expression == 'BSD-3-Clause':
        choice, files = expression, ['LICENSE']
    elif expression == 'Zlib':
        choice, files = expression, ['LICENSE']
    elif expression == 'Apache-2.0 OR BSL-1.0':
        choice, files = 'BSL-1.0', ['LICENSE-BOOST']
    else:
        raise SystemExit(f'Review WASM dependency licence: {name} {expression}')
    licence = next((directory / file for file in files if (directory / file).exists())).read_text()
    notices.append(f'\n{name} {version}\nSelected licence: {choice}\n\n{licence}\n')
    inventory.append({'name': name, 'version': version, 'selectedLicence': choice,
                      'noticeSha256': hashlib.sha256(licence.encode()).hexdigest()})
pins = json.loads((here / 'pins.json').read_text())
receipt = {'wasmSha256': pins['assets']['relayswarm-core-0.4.4.wasm']['sha256'], 'packages': inventory}
notice_text = '\n'.join(line.rstrip() for line in '\n'.join(notices).splitlines()).rstrip() + '\n'
outputs = {'wasm-LICENCES.txt': notice_text, 'wasm-licences.json': json.dumps(receipt, indent=2) + '\n'}
for name, content in outputs.items():
    path = here / name
    if '--check' in sys.argv:
        if path.read_text() != content:
            raise SystemExit(f'WASM dependency notices differ: {name}')
    else:
        path.write_text(content)
print(f'{len(inventory)} pinned WASM dependency licence choices verified')
