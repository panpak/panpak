#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Draw the benchmark charts (SVG) from the CSV files in docs/bench-data/.

Usage: python3 bench/make_charts.py [data-dir] [out-dir]

Standard library only. The SVG is written directly, so the same CSV files always give
the same bytes. Every chart is written twice, `<name>.svg` (light) and `<name>-dark.svg`
(dark), for a `<picture>` element with a `prefers-color-scheme: dark` source.
"""
import csv
from pathlib import Path
import sys
from xml.sax.saxutils import escape

ROOT = Path(__file__).resolve().parents[1]

# Series colors: light theme, dark theme. Blue, orange, teal, grey, purple.
PALETTE = [('#1f6feb', '#58a6ff'), ('#d9730d', '#f0883e'), ('#1a7f64', '#3fb68b'), ('#6e7781', '#8b949e'), ('#8250df', '#b392f0')]

THEMES = {
    'light': dict(bg='#ffffff', fg='#1f2328', muted='#59636e', grid='#d1d9e0', series=[c[0] for c in PALETTE]),
    'dark': dict(bg='#0d1117', fg='#e6edf3', muted='#9198a1', grid='#3d444d', series=[c[1] for c in PALETTE]),
}

STYLE = '''
  .bg { fill: %(bg)s; }
  .fg { fill: %(fg)s; font-family: -apple-system, "Segoe UI", Helvetica, Arial, sans-serif; }
  .muted { fill: %(muted)s; font-family: -apple-system, "Segoe UI", Helvetica, Arial, sans-serif; }
  .grid { stroke: %(grid)s; stroke-width: 1; }
%(series)s
'''


def read_csv(path):
    meta, lines = {}, []
    for line in Path(path).read_text().splitlines():
        if line.startswith('#'):
            k, _, v = line[1:].partition(':')
            meta[k.strip()] = v.strip()
        else:
            lines.append(line)
    return meta, list(csv.DictReader(lines))


def footer(meta):
    return f"{meta.get('cpu', '?').replace(' with Radeon Graphics', '')}, {meta.get('os', '?')}, kernel {meta.get('kernel', '?')}, {meta.get('date_utc', '?')[:10]}"


def bar_chart(title, subtitle, groups, series, fmt, note, theme='light'):
    """groups: [(label, [value or None per series])]; series: [label]. Horizontal bars, one scale."""
    left, right, top = 215, 110, 70
    bar_h, gap, group_gap = 18, 4, 18
    width = 760
    plot = width - left - right
    vmax = max(v for _, vals in groups for v in vals if v is not None)
    # Legend: items in rows that fit the width.
    rows, x = [[]], 16
    for i, s in enumerate(series):
        w = 18 + 7 * len(s) + 22
        if x + w > width - 16 and rows[-1]:
            rows.append([])
            x = 16
        rows[-1].append((i, s, x))
        x += w
    notes = [note] if isinstance(note, str) else note
    height = top + sum(len([v for v in vals if v is not None]) * (bar_h + gap) + group_gap for _, vals in groups) + 40 + 20 * len(rows) + 14 * len(notes)
    out = []
    t = THEMES[theme]
    css = STYLE % dict(bg=t['bg'], fg=t['fg'], muted=t['muted'], grid=t['grid'],
                       series='\n'.join(f'  .s{i} {{ fill: {c}; }}' for i, c in enumerate(t['series'])))
    out.append(f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-label="{escape(title)}">')
    out.append(f'<title>{escape(title)}</title>')
    out.append('<style>' + css + '</style>')
    out.append(f'<rect class="bg" x="0" y="0" width="{width}" height="{height}"/>')
    out.append(f'<text class="fg" x="16" y="26" font-size="16" font-weight="600">{escape(title)}</text>')
    out.append(f'<text class="muted" x="16" y="46" font-size="12">{escape(subtitle)}</text>')
    y = top
    out.append(f'<line class="grid" x1="{left}" y1="{top - 8}" x2="{left}" y2="{height - 28 - 14 * len(notes) - 20 * len(rows)}"/>')
    for label, vals in groups:
        shown = [(i, v) for i, v in enumerate(vals) if v is not None]
        gh = len(shown) * (bar_h + gap)
        out.append(f'<text class="fg" x="{left - 10}" y="{y + gh / 2 - gap / 2 + 4:.0f}" font-size="13" text-anchor="end">{escape(label)}</text>')
        for i, v in shown:
            w = max(1.0, plot * v / vmax)
            out.append(f'<rect class="s{i}" x="{left}" y="{y}" width="{w:.1f}" height="{bar_h}" rx="2"/>')
            out.append(f'<text class="fg" x="{left + w + 6:.1f}" y="{y + 13}" font-size="12">{escape(fmt(v))}</text>')
            y += bar_h + gap
        y += group_gap
    for r, items in enumerate(rows):
        ly = height - 16 - 14 * len(notes) - 20 * (len(rows) - r)
        for i, s, x in items:
            out.append(f'<rect class="s{i}" x="{x}" y="{ly}" width="12" height="12" rx="2"/>')
            out.append(f'<text class="fg" x="{x + 18}" y="{ly + 11}" font-size="12">{escape(s)}</text>')
    for k, n in enumerate(notes):
        out.append(f'<text class="muted" x="16" y="{height - 12 - 14 * (len(notes) - 1 - k)}" font-size="11">{escape(n)}</text>')
    out.append('</svg>')
    return '\n'.join(out) + '\n'


def ms(v):
    return f'{v * 1000:.0f} ms'


def mb(v):
    return f'{v / 1e6:.1f} MB'


def delta_chart(data_dir, theme):
    meta, rows = read_csv(data_dir / 'delta.csv')
    groups = []
    for r in rows:
        groups.append((f"{r['app']} {r['old_version']} to {r['new_version']}",
                       [float(r['appimage_new_bytes']), float(r['panpak_new_bytes']), float(r['downloaded_bytes'])]))
    return bar_chart('Update size: bytes downloaded for the new version',
                     'Local HTTP server counts the bytes it sends; one run per app (the size does not vary)', groups,
                     ['New AppImage (whole file)', 'New Panpak package (whole file)', 'Panpak update (bytes fetched)'], mb, footer(meta), theme)


def start_chart(data_dir, theme):
    meta, rows = read_csv(data_dir / 'start.csv')
    order = ['panpak-first-start', 'panpak-later-warm', 'panpak-later-cold', 'appimage-warm', 'appimage-cold']
    apps = list(dict.fromkeys(r['app'] for r in rows))
    groups = []
    for a in apps:
        by = {r['variant']: float(r['median_s']) for r in rows if r['app'] == a}
        groups.append((a, [by.get(k) for k in order]))
    return bar_chart('Start time of an exiting command: first start and later starts',
                     'Median of 5 first starts and of 10 later starts per bar; AppImage through userspace FUSE', groups,
                     ['Panpak, first start', 'Panpak, later (warm)', 'Panpak, later (cold)', 'AppImage warm', 'AppImage cold'], ms, footer(meta), theme)


def view_chart(data_dir, theme):
    meta, rows = read_csv(data_dir / 'view.csv')
    apps = [a for a in dict.fromkeys(r['app'] for r in rows) if not a.startswith('(')]
    groups = []
    for a in apps:
        by = {r['variant']: float(r['median_s']) for r in rows if r['app'] == a}
        groups.append((a, [by['view'], by['no-view']]))
    ctl = {r['variant']: float(r['median_s']) for r in rows if r['app'] == '(control)'}
    note = [footer(meta), f"Control: /bin/true takes {ms(ctl['plain-true'])} alone and {ms(ctl['wrapper-true'])} under the wrapper that disables the view."]
    return bar_chart('Cost of the isolated view: later starts with and without it',
                     'Median of 10 alternating runs per bar; the no-view side runs inside a namespace wrapper that makes bubblewrap unusable', groups,
                     ['With the view', 'Without the view (wrapper)'], ms, note, theme)


def resolution_chart(data_dir, theme):
    meta, rows = read_csv(data_dir / 'resolution.csv')
    apps = list(dict.fromkeys(r['app'] for r in rows))
    groups = []
    for a in apps:
        by = {r['variant']: float(r['median_s']) for r in rows if r['app'] == a}
        groups.append((a, [by['resolution-cache-on'], by['resolution-cache-off']]))
    return bar_chart('Library resolution at start (includes the symbol check)',
                     'Later starts with the resolution cache on and off (PANPAK_NO_ABI_CACHE=1); median of 10 alternating runs', groups,
                     ['Cache on (a hit)', 'Cache off (resolve every start)'], ms, footer(meta), theme)


def main():
    data_dir = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT / 'docs/bench-data'
    out_dir = Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else ROOT / 'docs/bench-data'
    out_dir.mkdir(parents=True, exist_ok=True)
    for name, fn in [('delta', delta_chart), ('start', start_chart), ('view', view_chart), ('resolution', resolution_chart)]:
        if (data_dir / f'{name}.csv').exists():
            for theme, suffix in (('light', ''), ('dark', '-dark')):
                (out_dir / f'{name}{suffix}.svg').write_text(fn(data_dir, theme))
                print('wrote', out_dir / f'{name}{suffix}.svg')


if __name__ == '__main__':
    main()
