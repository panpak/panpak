# SPDX-License-Identifier: MIT OR Apache-2.0
"""The signed message and the signature block of a package (PANPAK-FORMAT.md sections 2 and 5), for the tests that make packages by hand."""
import struct

LABEL_PACKAGE = b"panpak/package/v1\0"


def message(version, flags, root, manifest):
    """What the package signature covers: label, footer version and flags, the hash of the index, then the manifest."""
    return LABEL_PACKAGE + struct.pack("<II", version, flags) + root + manifest


# The sections whose keys change what is safe, and the keys of each that a reader of this level knows (PANPAK-FORMAT.md section 3.1.9).
CRITICAL_SECTIONS = {
    "release": {"revision", "floor", "security", "channel", "history"},
    "abi": {"needs", "prefer_host", "modules"},
    "permissions": {"x11", "audio", "network", "network_allow_tcp", "network_bind_tcp", "sockets_allow", "home_paths", "xdg_dirs",
                    "portal", "notifications", "tray", "mpris", "devices", "exec_written", "gvfs", "dbus_own"},
    "launch": {"args", "env"},
    "runtime": {"min_runtime", "stub_hash"},
    "trust": {"root", "threshold", "recovery", "domain", "chain"},
    "host": {"arch", "glibc", "kernel", "landlock", "display", "portals", "bus"},
}


def with_critical(manifest):
    """The manifest (bytes or text) with its `[format].critical` list made again from what it uses, as every function that signs a manifest
    must do. A manifest with no `[format]` is returned as it is. `tests/apps/test_harness.py` checks this table against the specification."""
    import json
    import re
    import tomllib

    text = manifest.decode() if isinstance(manifest, bytes) else manifest
    if "\n[format]\n" not in "\n" + text:
        return manifest
    man = tomllib.loads(text)
    paths = sorted([f"{s}.{k}" for s, keys in CRITICAL_SECTIONS.items() for k in man.get(s, {}) if k in keys] + (["env"] if "env" in man else []))
    new = re.sub(r"(?m)^critical = \[[^\]]*\]$", "critical = [" + ", ".join(json.dumps(p) for p in paths) + "]", text, count=1)
    return new.encode() if isinstance(manifest, bytes) else new


def block(sig):
    """A signature block with the package signature as its only entry (type 1, critical)."""
    return struct.pack("<HHI", 1, 1, len(sig)) + sig
