#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A printable checklist of the operations of PROCEDURE.md section 6: one row per operation, for each app and for each machine.

Usage: python3 make_checklist.py <out.html>

Open the page in a browser and print it (A4, 100 %). Each row says what to do with the mouse, what must happen, and has boxes for "worked", "failed" and a line for what was seen.
A row that fails with Panpak because a permission of the Flatpak is missing is a hole to close (principle 3): write the exact operation in RESULTS.md. The files are the ones of `make_ops_files.py`.
"""
import html
import sys

APPS = [
    ('GIMP 3.2.8', [
        ('Start GIMP from Activities (type its name).', 'The main window appears.'),
        ('File, Open. In the dialog choose Pictures, photo.png, Open.', 'The picture opens (640 by 400). Note: is the dialog the one of the desktop (portal) or the app\'s own.'),
        ('Colors, Invert.', 'The colours of the picture invert.'),
        ('File, Export As. Name it photo-inverted.png, folder Pictures, Export, Export again.', 'No error. In Files the file is in Pictures.'),
        ('Close GIMP, start it again. File, Open Recent.', 'photo.png is listed; it opens.'),
        ('File, Open, press Ctrl+L, type the path ~/outside/photo-outside.png, Enter.', 'It opens, or the app says it cannot see the file (write which).'),
        ('In Files: right click Pictures/photo.png, Open With, GIMP.', 'GIMP opens that picture.'),
        ('A graphics tablet: not testable on the virtual machines. Write "no device".', 'Nothing to do; the permission is recorded by the script.'),
    ]),
    ('Kdenlive 26.08.3', [
        ('Start Kdenlive from Activities. If a welcome or setup dialog appears, accept the defaults.', 'The main window appears.'),
        ('Project, new project (default profile).', 'An empty project.'),
        ('Project Bin, Add Clip. Choose Videos, clip.mp4.', 'The clip is in the bin with a picture.'),
        ('Drag the clip to the timeline. Press the space bar.', 'The picture plays in the monitor.'),
        ('Project, Add Title Clip. Type a text, Close, drag it to the timeline.', 'The title is on the timeline.'),
        ('Project, Render. Format MP4, file ~/Videos/render.mp4. Render to File.', 'It ends without an error. In Files the file is in Videos and has a size.'),
        ('File, Save As, ~/Videos/test.kdenlive. Close Kdenlive, start it, File, Open Recent.', 'The project opens with its clips.'),
        ('Project Bin, Add Clip, Ctrl+L, type ~/outside/clip-outside.mp4, Enter.', 'The clip is added, or the app says it cannot see the file (write which).'),
        ('Project, Record Monitor (or the capture panel). Look for a camera or a capture device.', 'A device is listed, or none: the machines have none, write "no device".'),
    ]),
    ('ONLYOFFICE 10.0.0', [
        ('Start ONLYOFFICE from Activities.', 'The start screen appears (Home).'),
        ('Open local file. Choose Documents, report.docx.', 'The document opens. Note: the dialog of the desktop or the app\'s own.'),
        ('Click at the end of the text, type: second line. Ctrl+S.', 'It saves in place, with no dialog (a "Save as" dialog is a failure of this row: write it).'),
        ('File, Save As, report2.docx in Documents.', 'The file exists in Documents.'),
        ('File, Save As, report3.docx in Downloads.', 'The file exists in Downloads.'),
        ('Close ONLYOFFICE, start it, Open recent (on the start screen).', 'report.docx is listed; it opens.'),
        ('Insert, Image, From File. Choose Pictures, photo.png.', 'The picture is in the document.'),
        ('Open local file, Ctrl+L, type ~/outside/report-outside.docx, Enter.', 'It opens, or the app cannot see the file (write which).'),
        ('On the start screen click the cloud connectors (Connect to cloud).', 'The page loads (no account is needed).'),
    ]),
    ('qBittorrent 5.3.0', [
        ('Start qBittorrent from Activities.', 'The main window appears.'),
        ('Add a torrent link (the chain-link icon). Paste the magnet link of a small, legal file, the same on both machines: write it here ______________________', 'The torrent is listed and starts.'),
        ('Wait for the end of the download.', 'It is in Downloads (in Files). A notification may appear: write it.'),
        ('Look at the top bar of the desktop while it runs.', 'A tray icon, or none: write which.'),
    ]),
    ('Text Editor (the one of Fedora on the Flatpak machine, ours on the Panpak machine)', [
        ('In Files open Documents, ops. Right click a.txt, Open With, Text Editor (on the Panpak machine choose the entry that is ours).', 'a.txt opens with the right name in the title.'),
        ('Change a line. Ctrl+S.', 'It saves in place, or an error, or "Save as": write which. Claude reads the file after.'),
        ('Save As a2.txt, same folder.', 'The dialog: the desktop\'s or the app\'s own. The file exists.'),
        ('Save As in Downloads.', 'The same.'),
        ('Close the app. Open a.txt from the recent files of the app, then from Files.', 'It opens; can it be saved.'),
        ('With a.txt open, Open (Ctrl+O): what does the dialog show? Is b.txt reachable without choosing it?', 'THE LINE THAT MATTERS: b.txt must stay out of reach unless the person chooses it. Write what you see.'),
    ]),
]
MACHINES = ['silverblue-flatpak (Flatpak)', 'silverblue-panpak (Panpak)']

CSS = """
body{font:11pt/1.3 system-ui,sans-serif;margin:1.2cm;color:#000;background:#fff}
h1{font-size:15pt;margin:0 0 .2cm}h2{font-size:13pt;margin:.5cm 0 .15cm;break-after:avoid}
p.note{font-size:9pt;margin:.1cm 0 .3cm}
table{border-collapse:collapse;width:100%;break-inside:auto}tr{break-inside:avoid}
th,td{border:1px solid #000;padding:3px 5px;vertical-align:top;font-size:9.5pt}th{background:#eee}
td.n{width:.7cm;text-align:center}td.box{width:3.4cm;white-space:nowrap}td.seen{width:5cm}
.machine{break-before:page}.machine:first-of-type{break-before:auto}
@media print{body{margin:0}}
"""


def page():
    out = ['<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Step 9b operations checklist</title>',
           '<meta name="viewport" content="width=device-width,initial-scale=1"><style>' + CSS + '</style></head><body>']
    for m in MACHINES:
        out.append(f'<section class="machine"><h1>Operations: {html.escape(m)}</h1>')
        out.append('<p class="note">Date: __________ &nbsp; Person: __________ &nbsp; Files: ~/Documents/ops, ~/Pictures, ~/Videos, ~/outside (made by make_ops_files.py). '
                   'Mark one box per row and write what you saw. A step that fails because a permission is missing is a hole to close: write the exact step.</p>')
        for app, rows in APPS:
            out.append(f'<h2>{html.escape(app)}</h2><table><tr><th>#</th><th>What to do</th><th>What must happen</th><th>Result</th><th>What I saw</th></tr>')
            for i, (what, expect) in enumerate(rows, 1):
                out.append(f'<tr><td class="n">{i}</td><td>{html.escape(what)}</td><td>{html.escape(expect)}</td><td class="box">&#9744; worked<br>&#9744; failed</td><td class="seen"></td></tr>')
            out.append('</table>')
        out.append('</section>')
    out.append('</body></html>')
    return '\n'.join(out)


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    open(sys.argv[1], 'w').write(page())
    print(f'wrote {sys.argv[1]}')


if __name__ == '__main__':
    main()
