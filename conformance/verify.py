#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Independent Panpak v1 format verifier.

Written ONLY from the spec (PANPAK-FORMAT.md), without reading the Rust code.
Checks: footer, adjacent layout, Ed25519 signature (manifest||root_hash),
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
    for k in ("notifications", "tray"):
        if k in perms and not isinstance(perms[k], bool):
            raise Fail(f"invalid manifest field permissions.{k}")
    own = perms.get("dbus_own", [])
    if not isinstance(own, list) or len(own) > 8:
        raise Fail("invalid manifest field permissions.dbus_own")
    for i, n in enumerate(own):
        if not own_name_ok(n, man["app"]) or n in own[:i]:
            raise Fail(f"invalid manifest field permissions.dbus_own: {str(n)[:60]!r}")
    w = man.get("desktop", {}).get("wm_class")
    if w is not None and not (isinstance(w, str) and re.fullmatch(r"[A-Za-z0-9._+-]{1,100}", w)):
        raise Fail("invalid manifest field desktop.wm_class")


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
    for key in ("needs", "prefer_host", "allow_missing"):
        for n in man.get("abi", {}).get(key, []):
            if not (isinstance(n, str) and 1 <= len(n) <= 255 and n not in (".", "..") and all(0x21 <= ord(c) <= 0x7E and c != "/" for c in n)):
                raise Fail(f"invalid manifest field abi.{key}")
    for s in man.get("permissions", {}).get("sockets_allow", []):
        if not (isinstance(s, str) and 1 <= len(s.encode()) <= 4096 and not any(unicodedata.category(c) == "Cc" for c in s)):
            raise Fail("invalid manifest field permissions.sockets_allow")
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
    url = man["update"].get("url")
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
            entries.append({"path": path, "mode": mode, "kind": "file", "size": size, "chunks": ids})
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
    if man_off + man_len + SIG_LEN != size - FOOTER_LEN:
        raise Fail("inconsistent layout: manifest+signature not adjacent to footer")
    if man_len > MAX_MANIFEST:
        raise Fail("manifest too large")
    if index_len > MAX_INDEX_COMP:
        raise Fail("compressed index too large")
    # Manifest + signature.
    toml_b = data[man_off : man_off + man_len]
    sig = data[man_off + man_len : man_off + man_len + SIG_LEN]
    if len(toml_b) != man_len or len(sig) != SIG_LEN:
        raise Fail("manifest/signature outside file bounds")
    try:
        import tomllib

        man = tomllib.loads(toml_b.decode("utf-8"))
    except Exception as e:
        raise Fail(f"manifest not TOML/UTF-8: {e}")
    try:
        key = man["update"]["key"]
        app = man["app"]
        entry, app_id = app["entry"], app["id"]
    except KeyError as e:
        raise Fail(f"manifest: missing field {e}")
    # Verify signature over manifest_toml || root_hash (raw).
    ed25519_verify(key, toml_b + root, sig)
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
