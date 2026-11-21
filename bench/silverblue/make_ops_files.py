#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The files for the operations of PROCEDURE.md section 6: make them on the host, copy the folder to the virtual machines (`scp -r ops <machine>:`), and put its files in the user folders as the procedure says.

Usage: python3 make_ops_files.py <folder>

  Documents/ops/a.txt, b.txt   two text files in one folder (the test of the second document that must stay inaccessible)
  Pictures/photo.png           a picture of 640x400 pixels (GIMP: open, Invert, Export As)
  Documents/report.docx        a document with one paragraph (ONLYOFFICE: open, type, save, save as)
Nothing here is a real document of anyone: the files are made from nothing, so that the same bytes are on both machines.
"""
import struct
import sys
from pathlib import Path
import zipfile
import zlib


def png(w, h):
    rows = b''.join(b'\x00' + bytes(((x * 255 // w) & 255, (y * 255 // h) & 255, ((x + y) * 255 // (w + h)) & 255)[i % 3] for x in range(w) for i in range(3)) for y in range(h))

    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(rows, 9)) + chunk(b'IEND', b'')


def docx(path, text):
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
        z.writestr('[Content_Types].xml', '<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>')
        z.writestr('_rels/.rels', '<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>')
        z.writestr('word/document.xml', f'<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>')


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    root = Path(sys.argv[1])
    (root / 'Documents/ops').mkdir(parents=True, exist_ok=True)
    (root / 'Pictures').mkdir(parents=True, exist_ok=True)
    (root / 'Documents/ops/a.txt').write_text('first document\nline two\nline three\n')
    (root / 'Documents/ops/b.txt').write_text('second document, in the same folder\n')
    (root / 'Pictures/photo.png').write_bytes(png(640, 400))
    docx(root / 'Documents/report.docx', 'A report for the test of the operations.')
    print(f'made {sum(1 for p in root.rglob("*") if p.is_file())} files in {root}')


if __name__ == '__main__':
    main()
