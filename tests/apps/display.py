# SPDX-License-Identifier: MIT OR Apache-2.0
"""The displays the harness starts apps on, and how it sees their windows: a virtual X server (`Xvfb`, windows read with `xwininfo`) or a headless Sway
(Wayland, windows read with `swaymsg`). Both are made by the harness for the run and ended by it, by process id; neither touches the session of the user.
The runtime folder of a display is a short private folder made by `isolation.owned_mkdtemp`."""
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import isolation  # noqa: E402


class Missing(Exception):
    pass


def need(tool, hint):
    if not shutil.which(tool):
        raise Missing(f"{tool} is not installed ({hint})")


def sh(cmd, **kw):
    kw.setdefault("capture_output", True)
    kw.setdefault("text", True)
    kw.setdefault("timeout", 60)
    return subprocess.run(cmd, **kw)


def parse_xwininfo(out):
    """`xwininfo -root -tree` text to {id: {title, cls, w, h}}."""
    res = {}
    for m in re.finditer(r'^\s+(0x[0-9a-f]+) (?:"([^"]*)"|\(has no name\)): \((?:"([^"]*)" "([^"]*)"|)\)\s+(\d+)x(\d+)\+(-?\d+)\+(-?\d+)', out, re.M):
        res[m.group(1)] = dict(title=m.group(2) or "", cls=m.group(4) or "", w=int(m.group(5)), h=int(m.group(6)))
    return res


def parse_sway_tree(tree):
    """The JSON tree of `swaymsg -t get_tree` (already parsed) to {id: {title, cls, w, h}}: the leaves that are windows."""
    res = {}

    def walk(n):
        kids = n.get("nodes", []) + n.get("floating_nodes", [])
        if not kids and n.get("type") in ("con", "floating_con") and (n.get("app_id") or n.get("window_properties")):
            r = n.get("rect", {})
            res[str(n["id"])] = dict(title=n.get("name") or "", cls=n.get("app_id") or (n.get("window_properties") or {}).get("class", ""), w=r.get("width", 0), h=r.get("height", 0))
        for k in kids:
            walk(k)

    walk(tree)
    return res


class Display:
    kind = "?"
    runtime = None

    def env(self):
        raise NotImplementedError

    def windows(self):
        raise NotImplementedError

    def close(self):
        p = getattr(self, "proc", None)
        if p is not None:
            isolation.stop_group(p.pid, p, grace=5)
        if self.runtime:
            isolation.remove_owned(Path(self.runtime), Path(self.runtime))


class X11Display(Display):
    kind = "x11"

    def __init__(self, size="1920x1080x24"):
        need("Xvfb", "install xvfb")
        need("xwininfo", "install x11-utils")
        self.runtime = isolation.owned_mkdtemp("panpak-apps-x-")
        r, w = os.pipe()
        self.proc = subprocess.Popen(["Xvfb", "-displayfd", str(w), "-screen", "0", size, "-nolisten", "tcp"], pass_fds=(w,), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        os.close(w)
        self.display = None
        with os.fdopen(r) as f:
            line = f.readline().strip()
        if not line.isdigit():
            self.close()
            raise Missing("Xvfb did not start (it did not give a display number)")
        self.display = f":{line}"

    def env(self):
        return {"DISPLAY": self.display, "XDG_RUNTIME_DIR": self.runtime}

    def windows(self):
        out = sh(["xwininfo", "-display", self.display, "-root", "-tree"]).stdout
        return {k: v for k, v in parse_xwininfo(out).items() if "IsViewable" in sh(["xwininfo", "-display", self.display, "-id", k]).stdout}


class WaylandDisplay(Display):
    kind = "wayland"

    def __init__(self, size="1280x800"):
        need("sway", "install sway, version 1.9 or newer")
        need("swaymsg", "it comes with sway")
        self.runtime = isolation.owned_mkdtemp("panpak-apps-w-")
        conf = Path(self.runtime) / "sway.conf"
        conf.write_text(f"output HEADLESS-1 resolution {size}\nfloating_modifier none\n")
        env = {"PATH": os.environ["PATH"], "HOME": self.runtime, "XDG_RUNTIME_DIR": self.runtime, "WLR_BACKENDS": "headless", "WLR_LIBINPUT_NO_DEVICES": "1", "WLR_RENDERER": "pixman"}
        self.proc = subprocess.Popen(["sway", "-c", str(conf)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        rt = Path(self.runtime)
        for _ in range(100):
            socks = [f for f in rt.glob("wayland-*") if not f.name.endswith(".lock")]
            ipc = list(rt.glob("sway-ipc.*"))
            if socks and ipc:
                self.socket, self.ipc = socks[0].name, str(ipc[0])
                return
            if self.proc.poll() is not None:
                break
            time.sleep(0.1)
        self.close()
        raise Missing("a headless sway did not start here (it needs sway 1.9 or newer and no GPU)")

    def env(self):
        return {"WAYLAND_DISPLAY": self.socket, "XDG_RUNTIME_DIR": self.runtime, "SWAYSOCK": self.ipc}

    def windows(self):
        r = sh(["swaymsg", "-s", self.ipc, "-t", "get_tree"])
        try:
            return parse_sway_tree(json.loads(r.stdout))
        except ValueError:
            return {}


def processes_of(marker):
    """The ids of the processes whose command line has `marker` (the isolated home of one run): what an app started and what left its process group."""
    mine = []
    for d in os.listdir("/proc"):
        if d.isdigit() and int(d) != os.getpid():
            try:
                if marker.encode() in Path(f"/proc/{d}/cmdline").read_bytes():
                    mine.append(int(d))
            except OSError:
                pass
    return mine


def end_run(marker, grace=3.0):
    """End every process of a run, by process id: SIGTERM, then SIGKILL for what is left after the grace time."""
    for sig, wait in ((signal.SIGTERM, grace), (signal.SIGKILL, 1.0)):
        pids = processes_of(marker)
        if not pids:
            return
        for p in pids:
            try:
                os.kill(p, sig)
            except OSError:
                pass
        time.sleep(wait)


LIBRARY_PATH_VAR = "LD_LIBRARY_PATH"


def process_rows(root_pid):
    """The process `root_pid` and everything below it, read from /proc: a list of `{pid, ppid, exe, ld}` where `ld` is the value of LD_LIBRARY_PATH in
    the environment of the process, or None. A process that cannot be read (it ended, or it is not ours) is left out."""
    kids = {}
    ppids = {}
    for d in os.listdir("/proc"):
        if d.isdigit():
            try:
                ppid = int(Path(f"/proc/{d}/stat").read_text().rsplit(")", 1)[1].split()[1])
            except (OSError, IndexError, ValueError):
                continue
            ppids[int(d)] = ppid
            kids.setdefault(ppid, []).append(int(d))
    rows, todo = [], [root_pid]
    while todo:
        pid = todo.pop()
        todo += kids.get(pid, [])
        try:
            exe = os.readlink(f"/proc/{pid}/exe")
            env = Path(f"/proc/{pid}/environ").read_bytes().split(b"\0")
        except OSError:
            continue
        ld = next((e[len(LIBRARY_PATH_VAR) + 1:].decode(errors="replace") for e in env if e.startswith(LIBRARY_PATH_VAR.encode() + b"=")), None)
        rows.append({"pid": pid, "ppid": ppids.get(pid), "exe": exe, "ld": ld})
    return rows


def library_path_report(rows, tree_dir, converted=False):
    """What is wrong with the library path in a run (PANPAK-FORMAT.md section 7.1): `(faults, notes)`. `rows` as from `process_rows`, `tree_dir` the folder with the
    trees of the store (the store is the folder above it). Only values that take effect count: an absolute path into the store (a tree, a farm, a package) in a process
    that is not an app program is a fault; the relative `/$ORIGIN/.panpak-lib-...` that the stub makes names a folder that does not exist at the root, so it is inert in
    the `sh` or `bash` of the host that runs the script of an AppImage, and is no fault. A program of the app must hold the relative value. When it holds anything else
    the stub did not put it there: a script of the app set it (Shotcut's `usr/bin/shotcut`). In a converted AppImage that is a note (the scripts of the app are not
    rewritten); in an app made from source it is a fault."""
    tree_dir = str(tree_dir).rstrip("/") + "/"
    store = str(Path(tree_dir.rstrip("/")).parent).rstrip("/") + "/"
    inside = set()
    for r in rows:
        if r["exe"].startswith(tree_dir):
            inside.add(r["pid"])
    changed = True
    while changed:                      # what an app program started is inside too
        changed = False
        for r in rows:
            if r["pid"] not in inside and r["ppid"] in inside:
                inside.add(r["pid"])
                changed = True
    faults, notes = [], []
    for r in rows:
        if r["ld"] is None:
            continue
        effective = [e for e in r["ld"].split(":") if e.startswith(store)]
        if r["exe"].startswith(tree_dir):
            if not r["ld"].startswith("/$ORIGIN/"):
                if converted:
                    text = f"{r['exe'][len(tree_dir):].split('/', 1)[-1]} holds an absolute {LIBRARY_PATH_VAR} that a script of the app set (a converted AppImage: the scripts are not rewritten): {r['ld'][:100]}"
                    if text not in notes:
                        notes.append(text)
                else:
                    faults.append(f"pid {r['pid']} ({r['exe']}) holds {LIBRARY_PATH_VAR} with a path that is not relative to the program: {r['ld'][:120]}")
        elif effective:
            where = "outside the app" if r["pid"] not in inside else "started by the app"
            faults.append(f"pid {r['pid']} ({r['exe']}) is {where} and holds {LIBRARY_PATH_VAR} with a path of the store that takes effect: {effective[0][:120]}")
    return faults, notes


def library_path_offenders(rows, tree_dir):
    """The faults of `library_path_report` for an app made from source (a converted AppImage is `library_path_report(..., converted=True)`)."""
    return library_path_report(rows, tree_dir, converted=False)[0]


def window_counts(win, min_size, is_first):
    """Does this window of the app count as its window? It must be as wide as `min_size` says, and as tall, except the first window of the app: a first dialog
    (LMMS asks for its working directory in one of 500 x 114) is the window of the app even when it is shorter."""
    mw, mh = min_size
    return win["w"] >= mw and (win["h"] >= mh or is_first)
