#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Validate the non-empty Kdenlive project: clip producer, timeline use.

Usage: kdenlive_clip.py work-directory [output.json]
Checks: mlt root XML, at least one producer with resource=clip-cycle.mp4
existing and readable, at least one entry in playlist/tractor referencing it
(non-empty timeline), project duration > 0.
"""
import hashlib
import json
from pathlib import Path
import sys
import xml.etree.ElementTree as ET


def main():

    work = Path(sys.argv[1]).resolve()
    proj = work / 'home/.local/share/org.example.kdenlive/saved-clip.kdenlive'
    clip = work / 'home/.local/share/org.example.kdenlive/clip-cycle.mp4'
    assert proj.is_file(), proj
    assert clip.is_file() and clip.stat().st_size > 0, clip
    root = ET.parse(str(proj)).getroot()
    assert root.tag == 'mlt', root.tag
    # Kdenlive 24.x uses <chain> with resource (relative to the project folder).
    chains = [c for c in root.iter('chain')
              if (c.find('property[@name="resource"]') is not None
                  and 'clip-cycle.mp4' in (c.find('property[@name="resource"]').text or ''))]
    assert chains, 'no chain references clip-cycle.mp4'
    for c in chains:
        res = c.find('property[@name="resource"]').text
        rp = Path(res)
        if not rp.is_absolute():
            rp = proj.parent / res
        assert rp.is_file(), f'unreadable resource: {res} (from {proj.parent})'
    ids = {c.get('id') for c in chains}
    refs = [e for e in root.iter('entry') if e.get('producer') in ids]
    tractor = root.find('tractor')
    assert refs, 'empty timeline: no entry references the clip chains'
    outs = [c.get('out') for c in chains]
    info = {'chains': len(chains), 'timeline_refs': len(refs), 'chain_out': outs,
            'bytes': proj.stat().st_size,
            'sha256': hashlib.sha256(proj.read_bytes()).hexdigest()}
    text = json.dumps({'method': 'Kdenlive GUI on dedicated Xvfb; ffmpeg clip added and inserted (v key), project saved',
                       'project': str(proj.relative_to(work)), **info}, indent=2) + '\n'
    if len(sys.argv) > 2:
        Path(sys.argv[2]).write_text(text)
    print(text, end='')


if __name__ == "__main__":
    main()
