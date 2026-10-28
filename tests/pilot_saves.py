#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Validate files saved through the isolated pilot GUI; optionally record evidence.

Usage: pilot_saves.py work-directory [output.json]
Requires ImageMagick identify for a real JPEG decode.
"""
import configparser
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import xml.etree.ElementTree as ET
import zipfile


def main():

    work = Path(sys.argv[1]).resolve()
    data = work / 'home/.local/share'
    files = {
        'krita': data / 'org.example.krita/saved-cycle.kra',
        'kdenlive': data / 'org.example.kdenlive/saved-cycle.kdenlive',
        'rawtherapee': data / 'org.example.rawtherapee/saved-cycle.jpg',
    }
    with zipfile.ZipFile(files['krita']) as z:
        assert z.testzip() is None
        assert z.read('mimetype').strip() == b'application/x-krita'
        ET.fromstring(z.read('maindoc.xml'))
        assert 'mergedimage.png' in z.namelist()
    assert ET.parse(files['kdenlive']).getroot().tag == 'mlt'
    decoded = subprocess.check_output(
        ['identify', '-format', '%m %wx%h', str(files['rawtherapee'])], text=True)
    assert decoded == 'JPEG 640x480', decoded
    params = configparser.ConfigParser()
    params.read(str(files['rawtherapee']) + '.out.pp3')
    assert abs(params.getfloat('Exposure', 'Compensation') - 1.91) < 0.01
    results = {app: {'path': str(path.relative_to(work)), 'bytes': path.stat().st_size,
                     'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
               for app, path in files.items()}
    text = json.dumps({'method': 'GUI saves on dedicated Xvfb; validated afterwards',
                       'files': results, 'rawtherapee_decode': decoded,
                       'rawtherapee_exposure_ev': 1.91}, indent=2) + '\n'
    if len(sys.argv) > 2:
        Path(sys.argv[2]).write_text(text)
    print(text, end='')


if __name__ == "__main__":
    main()
