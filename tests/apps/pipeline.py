# SPDX-License-Identifier: MIT OR Apache-2.0
"""What the harness does to one app: get a package (download and `panpak convert` for an AppImage, `panpak build` for a recipe; both cached), install it in an
isolated HOME, and start it on a display to time the first window. Every step returns what it took, so that `run.py` can print the time of each phase."""
import hashlib
import json
import os
import re
import shutil
import statistics
import struct
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import isolation  # noqa: E402
import display  # noqa: E402
import ed25519  # noqa: E402

BASE_ENV = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LANG": "C.UTF-8"}


class StepError(Exception):
    pass


def sh(cmd, **kw):
    kw.setdefault("capture_output", True)
    kw.setdefault("text", True)
    kw.setdefault("timeout", 3600)
    return subprocess.run([str(c) for c in cmd], **kw)


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


class Ctx:
    """Where things are: the folder of the run, the binaries, the signing key."""

    def __init__(self, work, bin_dir, downloads=None):
        self.work = Path(work)
        self.bin = Path(bin_dir)
        self.panpak, self.stub = self.bin / "panpak", self.bin / "panpak-stub"
        for b in (self.panpak, self.stub):
            if not b.exists():
                raise StepError(f"{b} is missing: build it (`cargo build --release --target x86_64-unknown-linux-musl`) or give --bin")
        self.downloads = Path(downloads) if downloads else self.work / "dl"
        self.pkgs = self.work / "pkg"
        self.runs = self.work / "runs"
        self.xdg_cache = self.work / "xdg-cache"
        for d in (self.work, self.pkgs, self.runs, self.downloads):
            d.mkdir(parents=True, exist_ok=True)
        self.keyfile = self.work / "harness.key"
        if not self.keyfile.exists():
            old = os.umask(0o077)
            try:
                r = sh([self.panpak, "keygen", "-o", self.keyfile], timeout=60)
            finally:
                os.umask(old)
            if r.returncode != 0:
                raise StepError("panpak keygen failed: " + r.stderr.strip())
        self.digests = (sha256_file(self.panpak), sha256_file(self.stub))

    def seed(self):
        return bytes.fromhex(self.keyfile.read_text().strip())


def cache_key(ctx, app, source_digest):
    """What a cached package depends on: the source (the AppImage's hash or the recipe's text), the two binaries and the options of the app."""
    h = hashlib.sha256()
    for part in (source_digest, *ctx.digests, json.dumps([app.convert_opts, app.permissions, app.id], sort_keys=True)):
        h.update(part.encode() + b"\0")
    return h.hexdigest()[:24]


# ---- AppImage: download, convert, permissions ------------------------------------------------------------------

def fetch(ctx, app):
    """The AppImage, downloaded once (curl) and checked against the hash of the config when there is one. Returns (path, notes)."""
    d = ctx.downloads / app.name
    d.mkdir(parents=True, exist_ok=True)
    path = d / app.url.rsplit("/", 1)[1].replace("%2B", "+")
    notes = []
    if not path.exists():
        r = sh(["curl", "-sSL", "--fail", "--retry", "2", "-m", "1500", "-o", str(path) + ".part", app.url], timeout=1700)
        if r.returncode != 0:
            raise StepError(f"download of {app.url} failed: {r.stderr.strip()[-200:]}")
        os.rename(str(path) + ".part", path)
        path.chmod(0o755)
    got = sha256_file(path)
    if app.sha256 and got != app.sha256:
        raise StepError(f"{path.name}: SHA-256 is {got}, the list of apps says {app.sha256}")
    if not app.sha256:
        notes.append(f"the download was not checked against a hash (sha256 {got})")
    return path, got, notes


def read_footer(d):
    magic, ver, flags, ioff, ilen, moff, mlen, root = struct.unpack("<8sIIQQQQ32s", d[-80:])
    return magic, ver, flags, ioff, ilen, moff, mlen, root


def add_permissions(src, dst, extra, seed):
    """Add a [permissions] block to a converted package and sign the manifest again. Lines that `convert` wrote itself (x11 for an xcb-only app) are kept
    unless the block says something else about the same key."""
    d = Path(src).read_bytes()
    magic, ver, flags, ioff, ilen, moff, mlen, root = read_footer(d)
    man = d[moff:moff + mlen].decode()
    m = re.search(r"\[permissions\]\n((?:[^\[\n][^\n]*\n)*)", man)
    if m:
        keep = [ln for ln in m.group(1).splitlines() if ln.split("=")[0].strip() not in extra]
        extra = extra.replace("[permissions]\n", "[permissions]\n" + "".join(ln + "\n" for ln in keep), 1)
        man = man[:m.start()] + man[m.end():]
    newman = (man + "\n" + extra).encode()
    out = d[:moff] + newman + ed25519.sign(seed, newman + root) + struct.pack("<8sIIQQQQ32s", magic, ver, flags, ioff, ilen, moff, len(newman), root)
    Path(dst).write_bytes(out)
    Path(dst).chmod(0o755)


def bad_links(path):
    """Symbolic links of the AppImage that the runtime refuses (absolute, or leaving the root), read with `unsquashfs -ll`. Empty without squashfs-tools."""
    if not shutil.which("unsquashfs"):
        return []
    with open(path, "rb") as f:
        h = f.read(64)
    shoff, = struct.unpack_from("<Q", h, 0x28)
    shentsize, shnum = struct.unpack_from("<HH", h, 0x3A)
    off = shoff + shentsize * shnum
    out = sh(["unsquashfs", "-o", off, "-ll", path], timeout=1200).stdout
    bad = []
    for ln in out.splitlines():
        m = re.match(r"^l\S+\s+\S+\s+\d+\s+\S+\s+\S+\s+squashfs-root/(.*) -> (.*)$", ln)
        if not m:
            continue
        p, t = m.group(1), m.group(2)
        norm = os.path.normpath(os.path.join(os.path.dirname(p), t))
        if t.startswith("/") or norm.startswith(".."):
            bad.append((p, t))
    return bad


def convert(ctx, app, key, fetched):
    """AppImage to package: `convert`, then the permissions of the app, then `doctor`; a library that nothing provides is allowed by name and the conversion runs again
    (up to 8 times), as a packager would. `fetched` is what `fetch` returned. Returns (package path, notes)."""
    src, _digest, notes = fetched
    links = bad_links(src)
    if links:
        raise StepError(f"{src.name} has symbolic links that the runtime refuses ({len(links)}, for example {links[0][0]} -> {links[0][1]}): remove them from the AppImage first")
    pkg = ctx.pkgs / f"{app.name}-{key}.panpak"
    base = ctx.pkgs / f"{app.name}-{key}.base"
    allow = []
    for _ in range(8):
        cmd = [ctx.panpak, "convert", src, "--id", app.id, "--stub", ctx.stub, "--secret-file", ctx.keyfile, "-o", base] + app.convert_opts + [x for a in allow for x in ("--allow-missing", a)]
        r = sh(cmd, env={**BASE_ENV, "HOME": str(ctx.work)})
        if r.returncode != 0:
            raise StepError("panpak convert failed: " + (r.stdout + r.stderr).strip()[-400:])
        add_permissions(base, pkg, app.permissions, ctx.seed())
        d = sh([ctx.panpak, "doctor", pkg], env={**BASE_ENV, "HOME": str(ctx.work)})
        m = re.search(r"unresolved libraries: (\S+) missing", d.stdout + d.stderr)
        if not m or m.group(1) in allow:
            break
        allow.append(m.group(1))
    base.unlink(missing_ok=True)
    if allow:
        notes.append(f"allow_missing added by the harness: {', '.join(allow)}")
    return pkg, notes


# ---- recipe: panpak build ----------------------------------------------------------------------------------------

def build(ctx, app, key):
    """Recipe to package with `panpak build` (it has its own caches of the image and the source, in the cache folder of the harness)."""
    d = ctx.work / "recipes" / app.name
    d.mkdir(parents=True, exist_ok=True)
    (d / "panpak.toml").write_text(app.recipe_text)
    pkg = ctx.pkgs / f"{app.name}-{key}.panpak"
    ctx.xdg_cache.mkdir(parents=True, exist_ok=True)
    env = {**BASE_ENV, "HOME": str(ctx.work), "XDG_CACHE_HOME": str(ctx.xdg_cache)}
    t0 = time.monotonic()
    r = sh([ctx.panpak, "build", d, "--secret-file", ctx.keyfile, "--stub", ctx.stub, "-o", pkg], env=env)
    if r.returncode != 0:
        raise StepError("panpak build failed: " + (r.stdout + r.stderr).strip()[-600:])
    notes = [ln.split("panpak build: ", 1)[1] for ln in (r.stdout + r.stderr).splitlines() if "peak memory" in ln or "the build used" in ln]
    return pkg, notes + [f"panpak build took {time.monotonic() - t0:.0f} s"]


def ensure_package(ctx, app, use_cache=True):
    """The package of an app, from the cache when nothing it depends on changed. Returns (path, seconds, cached, notes)."""
    t0 = time.monotonic()
    if app.source:
        fetched = None
        digest = hashlib.sha256(app.recipe_text.encode()).hexdigest()
    else:
        fetched = fetch(ctx, app)  # downloaded once; its hash is part of the key
        digest = fetched[1]
    key = cache_key(ctx, app, digest)
    pkg = ctx.pkgs / f"{app.name}-{key}.panpak"
    if use_cache and pkg.exists():
        return pkg, 0.0, True, (fetched[2] if fetched else [])
    t1 = time.monotonic()
    pkg, notes = build(ctx, app, key) if app.source else convert(ctx, app, key, fetched)
    return pkg, time.monotonic() - t1, False, notes


# ---- install and start ---------------------------------------------------------------------------------------------

class Run:
    """One app in an isolated HOME inside the work folder."""

    def __init__(self, ctx, app):
        self.ctx, self.app = ctx, app
        self.home = ctx.runs / app.name / "h"
        isolation.require_isolated_home(self.home, ctx.work)
        self.launcher = self.home / ".local/bin" / app.id

    def prepare(self):
        isolation.safe_rmtree(self.home.parent, self.ctx.work)
        for d in ("Documents", "Music", "Pictures", "Downloads", "Videos", ".config"):
            (self.home / d).mkdir(parents=True, exist_ok=True)
        (self.home / ".config/user-dirs.dirs").write_text(
            'XDG_DOCUMENTS_DIR="$HOME/Documents"\nXDG_MUSIC_DIR="$HOME/Music"\nXDG_PICTURES_DIR="$HOME/Pictures"\nXDG_DOWNLOAD_DIR="$HOME/Downloads"\nXDG_VIDEOS_DIR="$HOME/Videos"\n')
        (self.home / "Documents/sample.txt").write_text("hello from the app harness\n")

    def env(self, disp):
        e = {**BASE_ENV, "HOME": str(self.home), **disp.env(), **self.app.env}
        return e

    def install(self, pkg, disp):
        r = sh([self.ctx.panpak, "install", "--yes", "--accept-new-permissions", "--runtime", self.ctx.stub, pkg], env=self.env(disp), timeout=900)
        if r.returncode != 0:
            raise StepError("panpak install failed: " + (r.stdout + r.stderr).strip()[-400:])

    def seed_files(self):
        base = self.home / ".local/share/panpak/userdata" / self.app.id
        for rel, text in self.app.seed.items():
            f = base / rel
            isolation.require_inside(f, self.ctx.work, "seed file")
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text(text)

    def icon_ok(self):
        icons = list((self.home / ".local/share/icons/hicolor").glob(f"*/apps/panpak.{self.app.id}.png"))
        entry = self.home / ".local/share/applications" / f"panpak.{self.app.id}.desktop"
        return bool(icons) and entry.exists() and f"Icon=panpak.{self.app.id}" in entry.read_text()

    def start_once(self, disp, timeout=90):
        """Start the app, wait for its first window, end it. Returns the seconds to the window (None if none came) and the exit status of the launcher."""
        base = set(disp.windows())
        t0 = time.monotonic()
        p = subprocess.Popen([str(self.launcher)], env=self.env(disp), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL, start_new_session=True)
        found = None
        mw, mh = self.app.min_window
        try:
            while time.monotonic() - t0 < timeout and found is None:
                for wid, w in disp.windows().items():
                    if wid not in base and w["w"] >= mw and w["h"] >= mh:
                        found = time.monotonic() - t0
                        break
                if found is None:
                    if p.poll() is not None and not display.processes_of(str(self.home)):
                        break
                    time.sleep(0.05)
            if found is not None:
                time.sleep(0.5)
        finally:
            isolation.stop_group(p.pid, p, grace=5)
            display.end_run(str(self.home))
        return found, p.returncode

    def time_cli(self, disp, n=6):
        """A program with no window: n runs of `cli_args`, the seconds of each."""
        times = []
        for _ in range(n):
            t0 = time.monotonic()
            r = sh([self.launcher, *self.app.cli_args], env=self.env(disp), timeout=300)
            times.append(time.monotonic() - t0)
            if r.returncode != 0 and not times[1:]:
                raise StepError(f"{self.app.name} {' '.join(self.app.cli_args)} ended with status {r.returncode}")
        return times


def median_after_first(times):
    return statistics.median(times[1:]) if len(times) > 1 else times[0]
