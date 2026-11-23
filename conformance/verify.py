#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Independent Panpak v1 format verifier.

Written ONLY from the spec (PANPAK-FORMAT.md), without reading the Rust code.
Checks: footer, adjacent layout, signature block, Ed25519 signature (labeled message),
root hash, index, chunks (hash+before use), stub_hash, safe paths.

Usage: verify.py <file.panpak> [--json]
Exit 0 if valid (prints VALID), 1 if invalid (reason on stderr / JSON).
"""

import json
import re
import struct
import subprocess
import sys
import unicodedata

sys.path.insert(0, __import__("os").path.dirname(__file__))
from b3 import blake3

MAGIC = b"PANP\0\x01\0\0"
FOOTER_LEN = 80
SIG_LEN = 64
LABEL_PACKAGE = b"panpak/package/v1\0"
MAX_SIG_BLOCK = 64 * 1024
MAX_MANIFEST = 1 << 20
MAX_INDEX_COMP = 1 << 28
MAX_INDEX_RAW = 1 << 28
MAX_CHUNK_RAW = 1 << 20
MAX_CHUNK_COMP = 1 << 21


class Fail(Exception):
    pass


def u16(b, o):
    if o + 2 > len(b):
        raise Fail("truncated index (u16)")
    return struct.unpack_from("<H", b, o)[0]


def u32(b, o):
    if o + 4 > len(b):
        raise Fail("truncated index (u32)")
    return struct.unpack_from("<I", b, o)[0]


def u64(b, o):
    if o + 8 > len(b):
        raise Fail("truncated index (u64)")
    return struct.unpack_from("<Q", b, o)[0]


def zstd_decompress(data: bytes, what: str) -> bytes:
    p = subprocess.run(["zstd", "-d", "-c"], input=data, capture_output=True)
    if p.returncode != 0:
        raise Fail(f"{what}: zstd decompression failed")
    return p.stdout


def ed25519_verify(vk_hex: str, msg: bytes, sig: bytes):
    # Key in strict "ed25519:" + 64 lowercase-hex format.
    if not vk_hex.startswith("ed25519:"):
        raise Fail("key without ed25519: prefix")
    hx = vk_hex[8:]
    if len(hx) != 64 or any(c not in "0123456789abcdef" for c in hx):
        raise Fail("key must be 64 lowercase [0-9a-f] characters")
    vk = bytes.fromhex(hx)
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

    try:
        Ed25519PublicKey.from_public_bytes(vk).verify(sig, msg)
    except InvalidSignature:
        raise Fail("invalid Ed25519 signature")
    except Exception as e:
        raise Fail(f"malformed key/signature: {e}")


APP_ID = re.compile(r"[A-Za-z_][0-9A-Za-z_-]{0,62}(?:\.[A-Za-z_][0-9A-Za-z_-]{0,62})+")
VERSION = re.compile(r"[0-9A-Za-z][0-9A-Za-z._+~-]{0,63}")
UNSAFE_TEXT = set("\u2028\u2029\u200e\u200f") | {chr(c) for c in range(0x202A, 0x202F)} | {chr(c) for c in range(0x2066, 0x206A)}


def text_ok(s, max_bytes):
    return (
        isinstance(s, str)
        and 1 <= len(s.encode("utf-8", "surrogatepass")) <= max_bytes
        and not any(unicodedata.category(c) == "Cc" or c in UNSAFE_TEXT for c in s)
    )


# Launch fields and home paths (PANPAK-FORMAT.md section 3.1.2). Written out here on
# purpose: the verifier does not import the Rust lists.
PLACEHOLDERS = (
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "TMPDIR",
)
FORBIDDEN_ENV = (
    "PATH",
    "HOME",
    "XDG_RUNTIME_DIR",
    "PYTHONPATH",
    "PYTHONHOME",
    "PERL5LIB",
    "NODE_OPTIONS",
    "ELECTRON_RUN_AS_NODE",
    "GIO_MODULE_DIR",
    "GTK_MODULES",
    "GTK_PATH",
    "QT_PLUGIN_PATH",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_DIRS",
    "XDG_DATA_DIRS",
    "TMPDIR",
    "SQLITE_TMPDIR",
)
FORBIDDEN_ENV_PREFIXES = ("LD_", "PANPAK_")
BROAD_HOME_PATHS = (
    ".config",
    ".local",
    ".local/share",
    ".cache",
)
FORBIDDEN_HOME_PATHS = (
    ".config/autostart",
    ".config/systemd",
    ".config/environment.d",
    ".local/bin",
    ".local/share/applications",
    ".xinitrc",
    ".xsession",
    ".xsessionrc",
    ".xprofile",
    ".Xresources",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".pam_environment",
    ".local/share/nautilus",
    ".local/share/nemo",
    ".local/share/kio",
    ".config/mimeapps.list",
    ".config/panpak",
    ".local/share/panpak",
    ".cache/panpak",
    ".config/git",
    ".gitconfig",
    ".config/fish",
    ".config/nvim",
    ".vimrc",
    ".vim",
    ".emacs",
    ".emacs.d",
    ".config/i3",
    ".config/sway",
    ".config/hypr",
    ".config/openbox",
    ".config/kitty",
    ".config/alacritty",
    ".ssh",
    ".gnupg",
    ".pki",
    ".password-store",
    ".local/share/keyrings",
    ".mozilla",
    ".config/google-chrome",
    ".config/chromium",
    ".netrc",
    ".git-credentials",
    ".config/gh",
    ".docker",
    ".kube",
    ".aws",
    ".cargo",
    ".npmrc",
)
ENV_NAME = re.compile(r"[A-Z_][A-Z0-9_]*")


def check_launch_text(field, v):
    if not (isinstance(v, str) and len(v.encode()) <= 4096 and not any(unicodedata.category(c) == "Cc" for c in v)):
        raise Fail(f"invalid manifest field {field}")
    rest = v
    while "${" in rest:
        after = rest[rest.index("${") + 2 :]
        if "}" not in after or after[: after.index("}")] not in PLACEHOLDERS:
            raise Fail(f"invalid manifest field {field}: placeholder")
        rest = after[after.index("}") + 1 :]


def check_launch(man):
    launch = man.get("launch")
    if launch is None:
        return
    args = launch.get("args", [])
    if len(args) > 64:
        raise Fail("invalid manifest field launch.args")
    for a in args:
        check_launch_text("launch.args", a)
    env = launch.get("env", {})
    if len(env) > 64:
        raise Fail("invalid manifest field launch.env")
    for k, v in env.items():
        if not (len(k) <= 128 and ENV_NAME.fullmatch(k)) or k in FORBIDDEN_ENV or k.startswith(FORBIDDEN_ENV_PREFIXES):
            raise Fail(f"invalid manifest field launch.env: {k[:40]!r}")
        check_launch_text("launch.env", v)


def check_home_paths(man):
    seen = []
    items = man.get("permissions", {}).get("home_paths", [])
    if len(items) > 32:
        raise Fail("invalid manifest field permissions.home_paths")
    for h in items:
        p, a = h.get("path"), h.get("access")
        ok = (
            isinstance(p, str)
            and 1 <= len(p) <= 4096
            and p != "."
            and all(0x20 <= ord(c) <= 0x7E for c in p)
            and not p.startswith("/")
            and not p.endswith("/")
            and all(seg not in ("", ".", "..") for seg in p.split("/"))
            and p not in BROAD_HOME_PATHS
            and not any(p == f or p.startswith(f + "/") or f.startswith(p + "/") for f in FORBIDDEN_HOME_PATHS)
            and a in ("ro", "rw")
            and p not in seen
        )
        if not ok:
            raise Fail(f"invalid manifest field permissions.home_paths: {str(p)[:60]!r}")
        seen.append(p)


XDG_DIR_NAMES = ("music", "documents", "pictures", "videos", "download")
RESERVED_SCHEMES = (
    "http", "https", "file", "ftp", "ftps", "sftp", "ssh", "mailto", "javascript", "data", "about", "blob", "ws", "wss",
    "chrome", "view-source", "vbscript", "ms-msdt", "search-ms", "x-scheme-handler",
)


def check_xdg_dirs(man):
    items = man.get("permissions", {}).get("xdg_dirs", [])
    if len(items) > len(XDG_DIR_NAMES):
        raise Fail("invalid manifest field permissions.xdg_dirs")
    seen = []
    for d in items:
        n, a = d.get("name"), d.get("access")
        if not (n in XDG_DIR_NAMES and a in ("ro", "rw") and n not in seen):
            raise Fail(f"invalid manifest field permissions.xdg_dirs: {str(n)[:60]!r}")
        seen.append(n)


def check_desktop(man):
    schemes = man.get("desktop", {}).get("schemes", [])
    if len(schemes) > 8:
        raise Fail("invalid manifest field desktop.schemes")
    for i, sc in enumerate(schemes):
        ok = (
            isinstance(sc, str)
            and re.fullmatch(r"[a-z][a-z0-9+.-]{0,31}", sc) is not None
            and sc not in RESERVED_SCHEMES
            and sc not in schemes[:i]
        )
        if not ok:
            raise Fail(f"invalid manifest field desktop.schemes: {str(sc)[:60]!r}")


# Names an app never owns on the session bus (PANPAK-FORMAT.md section 3.1.5). The same list as
# panpak-core/src/dbus/rules.rs; the fixtures make the two agree.
NEVER_OWN = (
    "org.freedesktop", "org.gtk", "org.gnome.Shell", "org.gnome.SessionManager", "org.gnome.Mutter", "org.gnome.SettingsDaemon",
    "org.gnome.keyring", "org.gnome.evolution", "org.gnome.OnlineAccounts", "org.gnome.ScreenSaver", "org.a11y", "ca.desrt.dconf",
    "com.canonical", "com.ubuntu", "org.ayatana", "org.ibus", "org.fcitx", "org.mpris", "org.xfce", "org.mate", "org.cinnamon",
    "org.pantheon", "org.lxqt", "org.bluez", "net.hadess", "org.pulseaudio", "org.PulseAudio1", "org.pipewire", "fi.w1",
    "com.system76", "com.redhat", "org.fedoraproject", "org.opensuse", "org.cups",
)
BUS_NAME = re.compile(r"[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)+")


def own_name_ok(name, app):
    if not (isinstance(name, str) and len(name) <= 255 and BUS_NAME.fullmatch(name) and name.count(".") >= 2):
        return False
    if any(name == f or name.startswith(f + ".") for f in NEVER_OWN):
        return False
    if name.startswith("org.kde."):
        rest = name[len("org.kde."):]
        norm = lambda w: "".join(c.lower() for c in w if c.isascii() and c.isalnum())
        mine = [app["id"].rsplit(".", 1)[-1], app["name"], app["entry"].rsplit("/", 1)[-1]]
        return re.fullmatch(r"[a-z0-9]+", rest) is not None and any(w and norm(w) == rest for w in mine)
    return True


def check_bus(man):
    perms = man.get("permissions", {})
    for k in ("notifications", "tray", "exec_written"):
        if k in perms and not isinstance(perms[k], bool):
            raise Fail(f"invalid manifest field permissions.{k}")
    own = perms.get("dbus_own", [])
    if not isinstance(own, list) or len(own) > 8:
        raise Fail("invalid manifest field permissions.dbus_own")
    for i, n in enumerate(own):
        if not own_name_ok(n, man["app"]) or n in own[:i]:
            raise Fail(f"invalid manifest field permissions.dbus_own: {str(n)[:60]!r}")
    w = man.get("desktop", {}).get("wm_class")
    if w is not None and not (isinstance(w, str) and re.fullmatch(r"[A-Za-z0-9._+-]([A-Za-z0-9._+ -]{0,98}[A-Za-z0-9._+-])?", w)):
        raise Fail("invalid manifest field desktop.wm_class")


FORMAT_LEVEL = 1
# The keys of each critical section that a reader of this level knows (PANPAK-FORMAT.md section 3.1.9).
KNOWN_CRITICAL = {
    "release": {"revision", "floor", "security", "channel", "history"},
    "permissions": {"x11", "audio", "network", "network_allow_tcp", "network_bind_tcp", "sockets_allow", "home_paths", "xdg_dirs",
                    "portal", "notifications", "tray", "mpris", "devices", "exec_written", "dbus_own"},
    "launch": {"args", "env"},
    "abi": {"needs", "prefer_host", "modules"},
    "runtime": {"min_runtime", "stub_hash"},
    "trust": {"root", "threshold", "recovery", "domain", "chain"},
    "host": {"arch", "glibc", "kernel", "landlock", "display", "portals", "bus"},
}


def domain_fits_id(domain, app_id):
    """The domain, read backwards, is the beginning of the app id, and has at least two labels (PANPAK-FORMAT.md section 3.2)."""
    labels = domain.split(".")
    if len(domain) > 253 or len(labels) < 2 or not all(re.fullmatch(r"[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?", l) for l in labels):
        return False
    ident = app_id.split(".")
    return len(ident) >= len(labels) and list(reversed(labels)) == ident[: len(labels)]


DEVICE_CATEGORIES = {"gamepad", "camera"}
DEVICE_RESERVED = {"input", "usb"}


def check_devices(man):
    """`permissions.devices` (section 3.1.5): a list of one to four different categories; a name that this reader does not implement is "update panpak"."""
    p = man.get("permissions", {})
    if "devices" not in p:
        return
    d = p["devices"]
    if not (isinstance(d, list) and 1 <= len(d) <= 4 and all(isinstance(x, str) for x in d) and len(set(d)) == len(d)):
        raise Fail("invalid manifest field permissions.devices")
    for x in d:
        if x not in DEVICE_CATEGORIES:
            raise Fail(f"permissions.devices {x!r} is not implemented by this reader: update panpak")


HOST_ARCHS = {"x86_64", "aarch64"}
HOST_DISPLAYS = {"x11", "wayland"}
HOST_PORTALS = {"document"}
HOST_BUSES = {"none", "private"}


def check_sbom(man):
    """The `[sbom]` table (section 3.1.12): the hash of `.panpak/sbom.cdx.json` and the coverage. It is not critical: a key that this reader does not know is ignored."""
    s = man.get("sbom")
    if s is None:
        return
    h = s.get("hash")
    if not (isinstance(h, str) and len(h) == 64 and all(c in "0123456789abcdef" for c in h)):
        raise Fail("invalid manifest field sbom.hash")
    c = s.get("coverage")
    if not (isinstance(c, str) and 1 <= len(c) <= 16 and all(ch.islower() and ch.isascii() or ch == "-" for ch in c)):
        raise Fail("invalid manifest field sbom.coverage")


def check_host(man):
    """The `[host]` table (section 3.1.11): the forms, and a name that this reader does not know is "update panpak"."""
    h = man.get("host")
    if h is None:
        return
    unknown = set(h) - KNOWN_CRITICAL["host"]
    if unknown:
        raise Fail(f"unknown keys in [host]: {sorted(unknown)}")
    for k in ("glibc", "kernel"):
        if k in h and not (isinstance(h[k], str) and re.fullmatch(r"[0-9]{1,4}(\.[0-9]{1,4}){1,3}", h[k]) and len(h[k]) <= 24):
            raise Fail(f"invalid manifest field host.{k}")
    if "landlock" in h and not (isinstance(h["landlock"], int) and not isinstance(h["landlock"], bool) and 1 <= h["landlock"] <= 9):
        raise Fail("invalid manifest field host.landlock")
    for k, known in (("display", HOST_DISPLAYS), ("portals", HOST_PORTALS)):
        if k in h:
            v = h[k]
            if not (isinstance(v, list) and 1 <= len(v) <= 8 and all(isinstance(x, str) for x in v) and len(set(v)) == len(v)):
                raise Fail(f"invalid manifest field host.{k}")
            for x in v:
                if x not in known:
                    raise Fail(f"[host] {k} {x!r} is not known to this reader: update panpak")
    if "arch" in h and h["arch"] not in HOST_ARCHS:
        raise Fail(f"[host] arch {h['arch']!r} is not known to this reader: update panpak")
    if "bus" in h and h["bus"] not in HOST_BUSES:
        raise Fail(f"[host] bus {h['bus']!r} is not known to this reader: update panpak")


def check_trust(man):
    """The [trust] table (PANPAK-FORMAT.md section 3.2). Run on a manifest whose signature is verified. The chain is checked for its form here; it is followed
    by a client that has a pin, which is not a property of a package."""
    t = man.get("trust")
    if t is None:
        raise Fail("no [trust] table: a package of a test version of panpak, make it again")
    if set(t) - {"root", "threshold", "recovery", "domain", "chain"}:
        raise Fail(f"unknown key in [trust]: {sorted(t)}")
    key_ok = lambda k: isinstance(k, str) and re.fullmatch(r"ed25519:[0-9a-f]{64}", k) is not None
    commit_ok = lambda c: isinstance(c, str) and re.fullmatch(r"blake3:[0-9a-f]{64}", c) is not None
    root = t.get("root")
    if not isinstance(root, list) or len(root) == 0:
        raise Fail("[trust].root is not a list of keys")
    if len(root) != 1:
        raise Fail("more than one root key is reserved: update panpak")
    if t.get("threshold") != 1 or isinstance(t.get("threshold"), bool):
        raise Fail("a threshold other than 1 is reserved: update panpak")
    if not key_ok(root[0]):
        raise Fail("[trust].root is not a key")
    if not commit_ok(t.get("recovery")):
        raise Fail("[trust].recovery is not a commitment")
    if "domain" in t and not (isinstance(t["domain"], str) and domain_fits_id(t["domain"], man["app"]["id"])):
        raise Fail("[trust].domain does not fit the app id")
    chain = t.get("chain", [])
    if not isinstance(chain, list) or len(chain) > 8:
        raise Fail("[trust].chain is longer than 8 steps")
    for s in chain:
        if not isinstance(s, dict) or set(s) != {"by", "from", "to", "recovery", "sig"}:
            raise Fail("a step of [trust].chain has unknown or missing keys")
        if s["by"] not in ("root", "recovery"):
            raise Fail("a succession is signed by something this reader does not know: update panpak")
        if not (key_ok(s["from"]) and key_ok(s["to"]) and commit_ok(s["recovery"]) and isinstance(s["sig"], str) and re.fullmatch(r"[0-9a-f]{128}", s["sig"])):
            raise Fail("a step of [trust].chain is malformed")
    if "key" in man.get("update", {}):
        raise Fail("[update].key is not part of a package: its key is [trust].root")


def check_format(man):
    """The [format] table (PANPAK-FORMAT.md section 3.1.9). Run on a manifest whose signature is verified."""
    f = man.get("format")
    if f is None:
        raise Fail("no [format] table: a package of a test version of panpak, make it again")
    if set(f) != {"min_panpak", "critical", "compression", "chunking", "packed_by"}:
        raise Fail(f"unknown or missing key in [format]: {sorted(f)}")
    lvl = f["min_panpak"]
    if not isinstance(lvl, int) or isinstance(lvl, bool) or lvl < 1 or lvl > FORMAT_LEVEL:
        raise Fail(f"min_panpak {lvl!r} is not a level this reader has: update panpak")
    if f["compression"] != "zstd":
        raise Fail(f"compression {f['compression']!r} is not known: update panpak")
    crit = f["critical"]
    if not isinstance(crit, list) or len(crit) > 256 or not all(
        isinstance(c, str) and len(c) <= 64 and re.fullmatch(r"[a-z0-9_.]*", c) for c in crit
    ):
        raise Fail("[format].critical is malformed")
    for c in crit:
        sect, _, key = c.partition(".")
        if (c == "env") or (sect in KNOWN_CRITICAL and key in KNOWN_CRITICAL[sect]):
            continue
        raise Fail(f"critical field {c!r} is not known to this reader: update panpak")
    # The list is complete for the fields this reader knows: a manifest that was changed after `pack` and signed with the old list
    # would otherwise hide a field that restricts the app from an older reader.
    used = [f"{sect}.{k}" for sect, keys in KNOWN_CRITICAL.items() for k in man.get(sect, {}) if k in keys]
    if "env" in man:
        used.append("env")
    left_out = sorted(set(used) - set(crit))
    if left_out:
        raise Fail(f"[format].critical does not list {left_out}: the manifest was signed without making the list again")
    for k in ("chunking", "packed_by"):
        if not text_ok(f[k], 128):
            raise Fail(f"invalid manifest field format.{k}")


def check_release(man):
    """The [release] table (PANPAK-FORMAT.md section 3.1.10). Run on a manifest whose signature is verified."""
    r = man.get("release")
    if r is None:
        raise Fail("no [release] table: a package of a test version of panpak, make it again")
    if set(r) - {"revision", "floor", "security", "channel", "history"} or "revision" not in r:
        raise Fail(f"unknown or missing key in [release]: {sorted(r)}")
    rev, floor, sec, chan = r["revision"], r.get("floor", 0), r.get("security", 0), r.get("channel", "stable")
    for name, v in (("revision", rev), ("floor", floor), ("security", sec)):
        if not isinstance(v, int) or isinstance(v, bool) or v < 0 or v > 2**63 - 1:
            raise Fail(f"[release].{name} is not a number")
    if rev < 1:
        raise Fail("[release].revision must be from 1")
    if floor > rev or sec > rev:
        raise Fail("[release].floor and security cannot be above the revision")
    if not (isinstance(chan, str) and re.fullmatch(r"[a-z0-9][a-z0-9-]{0,31}", chan)):
        raise Fail("[release].channel must be 1 to 32 characters of a-z, 0-9 and -")
    h = r.get("history")
    if h is not None and not (isinstance(h, str) and re.fullmatch(r"[0-9a-f]{64}", h)):
        raise Fail("[release].history must be 64 lowercase hex characters")


def check_fields(man):
    """Field rules of PANPAK-FORMAT.md section 3.1.1 (applied even to signed manifests)."""
    app = man["app"]
    app_id = app["id"]
    if not (isinstance(app_id, str) and len(app_id) <= 128 and app_id.isascii() and APP_ID.fullmatch(app_id)):
        raise Fail(f"invalid app.id: {app_id!r}"[:120])
    if not text_ok(app["name"], 256):
        raise Fail("invalid manifest field app.name")
    if not (isinstance(app["version"], str) and VERSION.fullmatch(app["version"])):
        raise Fail("invalid manifest field app.version")
    entry = app["entry"]
    if not (
        isinstance(entry, str)
        and 1 <= len(entry.encode()) <= 4096
        and not any(unicodedata.category(c) == "Cc" for c in entry)
        and all(seg not in ("", ".", "..") for seg in entry.split("/"))
    ):
        raise Fail(f"invalid app.entry: {entry!r}")
    for key in ("needs", "prefer_host"):
        for n in man.get("abi", {}).get(key, []):
            if not (isinstance(n, str) and 1 <= len(n) <= 255 and n not in (".", "..") and all(0x21 <= ord(c) <= 0x7E and c != "/" for c in n)):
                raise Fail(f"invalid manifest field abi.{key}")
    for s in man.get("permissions", {}).get("sockets_allow", []):
        if not (isinstance(s, str) and 1 <= len(s.encode()) <= 4096 and not any(unicodedata.category(c) == "Cc" for c in s)):
            raise Fail("invalid manifest field permissions.sockets_allow")
    mods = man.get("abi", {}).get("modules")
    if mods is not None and not (isinstance(mods, str) and len(mods) == 64 and all(c in "0123456789abcdef" for c in mods)):
        raise Fail("invalid manifest field abi.modules")
    check_sbom(man)
    check_host(man)
    check_devices(man)
    check_home_paths(man)
    check_xdg_dirs(man)
    check_desktop(man)
    check_bus(man)
    check_launch(man)
    if "origin" in man:
        o = man["origin"]
        if not text_ok(o.get("converted_from"), 256) or "/" in o["converted_from"]:
            raise Fail("invalid manifest field origin.converted_from")
        if not text_ok(o.get("version"), 64):
            raise Fail("invalid manifest field origin.version")
        if not (isinstance(o.get("sha256"), str) and re.fullmatch(r"[0-9a-f]{64}", o["sha256"])):
            raise Fail("invalid manifest field origin.sha256")
    url = man.get("update", {}).get("url")
    if url is not None and not (isinstance(url, str) and len(url.encode()) <= 2048 and all(0x20 < b != 0x7F for b in url.encode())):
        raise Fail("invalid manifest field update.url")


def parse_index(raw: bytes):
    """Return (chunks, entries). chunks: list[dict]; entries: list[dict]."""
    p = 0
    n = u32(raw, p)
    p += 4
    if n > (len(raw) // 48) + 1:
        raise Fail(f"excessive n_chunks: {n}")
    chunks = []
    for i in range(n):
        if p + 48 > len(raw):  # [u8;32] + u64 + u32 + u32
            raise Fail(f"chunk {i} truncated")
        h, off, comp, rlen = raw[p : p + 32], u64(raw, p + 32), u32(raw, p + 40), u32(raw, p + 44)
        if not (1 <= comp <= MAX_CHUNK_COMP):
            raise Fail(f"chunk {i}: comp_len out of range ({comp})")
        if not (1 <= rlen <= MAX_CHUNK_RAW):
            raise Fail(f"chunk {i}: raw_len out of range ({rlen})")
        chunks.append({"hash": h, "off": off, "comp": comp, "raw": rlen})
        p += 48
    n = u32(raw, p)
    p += 4
    entries = []
    seen = set()
    for _ in range(n):
        ln = u16(raw, p)
        p += 2
        if not (1 <= ln <= 4096):
            raise Fail(f"path_len out of range: {ln}")
        if p + ln > len(raw):
            raise Fail("truncated path")
        try:
            path = raw[p : p + ln].decode("utf-8")
        except UnicodeDecodeError:
            raise Fail("non-UTF-8 path")
        p += ln
        if any(unicodedata.category(c) == "Cc" for c in path):
            raise Fail(f"unsafe path (control character): {path!r}")
        parts = path.split("/")
        if any(c in ("", ".", "..") for c in parts):
            raise Fail(f"unsafe path (empty, . or ..): {path!r}")
        if path in seen:
            raise Fail(f"duplicate path: {path!r}")
        seen.add(path)
        kind = raw[p] if p < len(raw) else (_ for _ in ()).throw(Fail("truncated kind"))
        p += 1
        mode = u32(raw, p) & 0o777
        p += 4
        if kind == 0:
            size, k = u64(raw, p), u32(raw, p + 8)
            p += 12
            ids = []
            total = 0
            for _ in range(k):
                cid = u32(raw, p)
                p += 4
                if cid >= len(chunks):
                    raise Fail(f"chunk_id {cid} beyond table ({len(chunks)})")
                total += chunks[cid]["raw"]
                ids.append(cid)
            if total != size:
                raise Fail(f"size {size} does not match chunks ({total})")
            # The facts about the content (section 4): flags, the SONAME, the extension area.
            if p >= len(raw):
                raise Fail("truncated file flags")
            flags = raw[p]
            p += 1
            if flags & ~0x0F:
                raise Fail(f"file flags {flags:#x} that this reader does not know: update panpak")
            if flags & 4 and not flags & 1:
                raise Fail("a SONAME on a file that is not marked as ELF")
            if flags & 1 and flags & 2:
                raise Fail("a file marked as ELF and as a script")
            soname = None
            if flags & 4:
                sl = u16(raw, p)
                p += 2
                if not (1 <= sl <= 255) or p + sl > len(raw):
                    raise Fail("SONAME length out of range")
                try:
                    soname = raw[p : p + sl].decode("utf-8")
                except UnicodeDecodeError:
                    raise Fail("non-UTF-8 SONAME")
                p += sl
                if any(unicodedata.category(c) == "Cc" or c == "/" for c in soname):
                    raise Fail("unsafe SONAME")
            patch = None
            if flags & 8:
                if p + 76 > len(raw):
                    raise Fail("truncated patch fields")
                base = raw[p : p + 32]
                poff, plen = struct.unpack_from("<QI", raw, p + 32)
                phash = raw[p + 44 : p + 76]
                p += 76
                if not (1 <= plen <= 1 << 30):
                    raise Fail("patch length out of range")
                if poff + plen >= 1 << 64:
                    raise Fail("patch offset out of range")
                patch = {"base": base, "off": poff, "len": plen, "hash": phash}
            el = u16(raw, p)
            p += 2
            if el > 1024 or p + el > len(raw):
                raise Fail("extension area of a file out of range")
            ext = raw[p : p + el]
            p += el
            q = 0
            while q < len(ext):
                if len(ext) - q < 4:
                    raise Fail("extension area: an entry is cut short")
                ty, xl = struct.unpack_from("<HH", ext, q)
                if len(ext) - q - 4 < xl:
                    raise Fail("extension area: an entry is longer than the area")
                if ty & 0x8000:
                    raise Fail(f"a critical extension of type {ty:#x} that this reader does not know: update panpak")
                q += 4 + xl
            entries.append({"path": path, "mode": mode, "kind": "file", "size": size, "chunks": ids, "flags": flags, "soname": soname, "patch": patch})
        elif kind == 1:
            entries.append({"path": path, "mode": mode, "kind": "dir"})
        elif kind == 2:
            ln = u16(raw, p)
            p += 2
            if not (1 <= ln <= 4096):
                raise Fail(f"symlink target out of range: {ln}")
            if p + ln > len(raw):
                raise Fail("truncated symlink target")
            try:
                tgt = raw[p : p + ln].decode("utf-8")
            except UnicodeDecodeError:
                raise Fail("non-UTF-8 symlink target")
            p += ln
            if not tgt:
                raise Fail("empty symlink target")
            if any(unicodedata.category(c) == "Cc" for c in tgt):
                raise Fail("unsafe symlink target (control character)")
            entries.append({"path": path, "mode": mode, "kind": "symlink", "target": tgt})
        else:
            raise Fail(f"unknown kind: {kind}")
    if p != len(raw):
        raise Fail(f"trailing bytes after index ({len(raw) - p})")
    return chunks, entries


def parse_sig_block(block: bytes) -> bytes:
    """Entries `u16 type, u16 flags, u32 length, data` that fill the block. Exactly one type 1 (critical, 64 bytes) is the package
    signature; another type is ignored unless it is critical; flag bits other than bit 0 are not defined."""
    if len(block) > MAX_SIG_BLOCK:
        raise Fail("signature block too large")
    p, sig = 0, None
    while p < len(block):
        if p + 8 > len(block):
            raise Fail("signature block: truncated entry")
        ty, fl, ln = struct.unpack_from("<HHI", block, p)
        p += 8
        if p + ln > len(block):
            raise Fail("signature block: an entry is longer than the block")
        data, p = block[p : p + ln], p + ln
        if fl & ~1:
            raise Fail("signature block: undefined flags")
        if ty == 1:
            if not fl & 1 or ln != SIG_LEN or sig is not None:
                raise Fail("signature block: package signature malformed or repeated")
            sig = data
        elif fl & 1:
            raise Fail(f"signature block: unknown critical entry (type {ty})")
    if sig is None:
        raise Fail("signature block: no package signature")
    return sig


def check(data: bytes):
    size = len(data)
    if size <= FOOTER_LEN:
        raise Fail("file too short")
    foot = data[size - FOOTER_LEN :]
    if foot[0:8] != MAGIC:
        raise Fail("wrong magic")
    if u32(foot, 8) != 1:
        raise Fail(f"unsupported version: {u32(foot, 8)}")
    if u32(foot, 12) != 0:
        raise Fail(f"nonzero flags: {u32(foot, 12)}")
    index_off, index_len = u64(foot, 16), u64(foot, 24)
    man_off, man_len = u64(foot, 32), u64(foot, 40)
    root = foot[48:80]
    # Layout coherence (adjacency, no holes nor extras).
    if index_off + index_len != man_off:
        raise Fail("inconsistent layout: index not adjacent to manifest")
    block_len = size - FOOTER_LEN - (man_off + man_len)
    if not (8 + SIG_LEN <= block_len <= MAX_SIG_BLOCK):
        raise Fail("inconsistent layout: manifest+signature block not adjacent to footer")
    if man_len > MAX_MANIFEST:
        raise Fail("manifest too large")
    if index_len > MAX_INDEX_COMP:
        raise Fail("compressed index too large")
    # Manifest + signature block.
    toml_b = data[man_off : man_off + man_len]
    block = data[man_off + man_len : size - FOOTER_LEN]
    if len(toml_b) != man_len:
        raise Fail("manifest outside file bounds")
    sig = parse_sig_block(block)
    try:
        import tomllib

        man = tomllib.loads(toml_b.decode("utf-8"))
    except Exception as e:
        raise Fail(f"manifest not TOML/UTF-8: {e}")
    try:
        key = man["trust"]["root"][0]
        app = man["app"]
        entry, app_id = app["entry"], app["id"]
    except KeyError as e:
        raise Fail(f"manifest: missing field {e}")
    # Only to name a package made before [format] rightly; the full check follows the signature.
    if "format" not in man:
        raise Fail("no [format] table: a package of a test version of panpak, make it again")
    # The signature covers label, footer version and flags, the root (raw) and the manifest.
    ed25519_verify(key, LABEL_PACKAGE + foot[8:16] + root + toml_b, sig)
    check_format(man)
    check_trust(man)
    check_release(man)
    # Field rules: a valid signature does not make the fields safe (spec 3.1.1).
    check_fields(man)
    # stub_hash REQUIRED, verified over bytes [0:first chunk].
    try:
        want_stub = man["runtime"]["stub_hash"]
    except KeyError:
        raise Fail("manifest without [runtime].stub_hash")
    if not (isinstance(want_stub, str) and len(want_stub) == 64 and all(c in "0123456789abcdef" for c in want_stub)):
        raise Fail("malformed stub_hash")
    # Index: decompress with cap, verify root, parse.
    comp_index = data[index_off : index_off + index_len]
    if len(comp_index) != index_len:
        raise Fail("index outside file bounds")
    raw_index = zstd_decompress(comp_index, "index")
    if len(raw_index) > MAX_INDEX_RAW:
        raise Fail("decompressed index exceeds limit")
    if blake3(raw_index) != root:
        raise Fail("root hash mismatch")
    chunks, entries = parse_index(raw_index)
    # Stub position: first chunk, or index_off if no chunks.
    stub_end = min([c["off"] for c in chunks], default=index_off)
    if blake3(data[0:stub_end]).hex() != want_stub:
        raise Fail("stub does not match signed hash")
    # Every chunk: decompress and verify BEFORE use (here: verify only).
    for i, c in enumerate(chunks):
        comp = data[c["off"] : c["off"] + c["comp"]]
        if len(comp) != c["comp"]:
            raise Fail(f"chunk {i} outside file bounds")
        raw = zstd_decompress(comp, f"chunk {i}")
        if len(raw) != c["raw"] or blake3(raw) != c["hash"]:
            raise Fail(f"chunk {i} corrupt")
    if not entry or entry.startswith("/") or ".." in entry.split("/"):
        raise Fail(f"invalid app.entry: {entry!r}")
    return {"app": app_id, "entry": entry, "chunks": len(chunks), "entries": len(entries)}


def main():
    args = [a for a in sys.argv[1:] if a != "--json"]
    as_json = "--json" in sys.argv
    if not args:
        print("usage: verify.py <file.panpak> [--json]", file=sys.stderr)
        return 2
    try:
        info = check(open(args[0], "rb").read())
    except Fail as e:
        if as_json:
            print(json.dumps({"ok": False, "error": str(e)}))
        else:
            print(f"INVALID: {e}", file=sys.stderr)
        return 1
    if as_json:
        print(json.dumps({"ok": True, "info": info}))
    else:
        print(f"VALID: {info['app']} ({info['entries']} entries, {info['chunks']} chunks)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
