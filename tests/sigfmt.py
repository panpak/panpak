# SPDX-License-Identifier: MIT OR Apache-2.0
"""The signed message and the signature block of a package (PANPAK-FORMAT.md sections 2 and 5), for the tests that make packages by hand."""
import struct

LABEL_PACKAGE = b"panpak/package/v1\0"


def message(version, flags, root, manifest):
    """What the package signature covers: label, footer version and flags, the hash of the index, then the manifest."""
    return LABEL_PACKAGE + struct.pack("<II", version, flags) + root + manifest


def block(sig):
    """A signature block with the package signature as its only entry (type 1, critical)."""
    return struct.pack("<HHI", 1, 1, len(sig)) + sig
