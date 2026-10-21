#!/usr/bin/env python3
"""Collects the licence texts of every crate in the sapflow binary into
THIRD_PARTY_LICENSES.txt, for distributing binaries and container images.

Run from the repository root after changing dependencies:
    python3 scripts/third-party-licences.py
"""
import json
import pathlib
import subprocess

meta = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--format-version", "1", "--locked"], text=True))
pkgs = {p["id"]: p for p in meta["packages"]}
# Only what the server binary links: walk the resolve graph from it.
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
root = next(p["id"] for p in meta["packages"] if p["name"] == "sapflow-server")
seen, stack = set(), [root]
while stack:
    i = stack.pop()
    if i in seen:
        continue
    seen.add(i)
    for d in nodes[i]["deps"]:
        if any(k["kind"] in (None, "normal") for k in d["dep_kinds"]):
            stack.append(d["pkg"])

# The Apache-2.0 text, for crates that publish none: from the first
# dependency that ships it.
apache = next(
    f.read_text()
    for i in sorted(seen, key=lambda i: pkgs[i]["name"])
    for f in sorted(pathlib.Path(pkgs[i]["manifest_path"]).parent.glob("LICENSE-APACHE*"))
    if f.is_file()
)
out = ["Third-party software in sapflow, with its licences. A text that is",
       "identical to one printed earlier is referenced rather than repeated.\n"]
printed = {}
count = 0
for i in sorted(seen, key=lambda i: (pkgs[i]["name"], pkgs[i]["version"])):
    p = pkgs[i]
    if p["name"].startswith("sapflow-"):
        continue
    count += 1
    d = pathlib.Path(p["manifest_path"]).parent
    files = sorted(f for f in d.iterdir()
                   if f.is_file() and f.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE")))
    out.append("=" * 78)
    out.append(f"{p['name']} {p['version']}  ({p.get('license') or 'see files'})")
    if p.get("repository"):
        out.append(p["repository"])
    out.append("=" * 78)
    if files:
        for f in files:
            text = f.read_text(errors="replace").rstrip()
            if text in printed:
                out.append(f"--- {f.name}: same text as {printed[text]}")
            else:
                printed[text] = f"{p['name']} {f.name}"
                out.append(f"--- {f.name}")
                out.append(text)
    elif "Apache-2.0" in (p.get("license") or ""):
        out.append("(No licence file in the published crate; the Apache-2.0 text follows.)")
        out.append(apache.rstrip())
    else:
        out.append("(No licence file in the published crate.)")
    out.append("")
pathlib.Path("THIRD_PARTY_LICENSES.txt").write_text("\n".join(out) + "\n")
print(f"{count} crates")
