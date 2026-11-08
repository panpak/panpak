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
