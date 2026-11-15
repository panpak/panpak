# SPDX-License-Identifier: MIT OR Apache-2.0
"""Unit tests of tests/apps/patched_cache.py: what comes back from the CI cache is checked, and a bad folder is removed, not used.
Run: python3 -m unittest discover -s tests/apps -p 'test_*.py'"""
import hashlib
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import patched_cache as pc  # noqa: E402


def make(folder, ident, files=None):
    """A folder as `panpak build` leaves it."""
    files = files or {"libgtk-4-1_4.18.6+panpak1_amd64.deb": b"one", "libgtk-4-dev_4.18.6+panpak1_amd64.deb": b"two"}
    d = Path(folder) / ident
    (d / "debs").mkdir(parents=True)
    sums = ""
    for name, data in sorted(files.items()):
        (d / "debs" / name).write_bytes(data)
        sums += f"{hashlib.sha256(data).hexdigest()}  {name}\n"
    (d / "SHA256SUMS").write_text(sums)
    (d / ".complete").write_text("gtk4 gtk4-dmabuf-v5.patch\n")
    return d


class PatchedCache(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / "patched"
        self.root.mkdir()

    def tearDown(self):
        self.tmp.cleanup()

    def test_a_good_folder_is_verified_and_kept(self):
        make(self.root, "a1")
        self.assertEqual(pc.verify(self.root), (1, 0))
        self.assertTrue((self.root / "a1").is_dir())

    def test_what_does_not_match_is_removed(self):
        for what in ("changed", "extra", "missing", "no-sums", "no-complete", "bad-sums", "link"):
            with self.subTest(what=what):
                d = make(self.root, what)
                deb = d / "debs/libgtk-4-1_4.18.6+panpak1_amd64.deb"
                if what == "changed":
                    deb.write_bytes(b"other")
                elif what == "extra":
                    (d / "debs/evil.deb").write_bytes(b"x")
                elif what == "missing":
                    deb.unlink()
                elif what == "no-sums":
                    (d / "SHA256SUMS").unlink()
                elif what == "no-complete":
                    (d / ".complete").unlink()
                elif what == "bad-sums":
                    (d / "SHA256SUMS").write_text("not a hash  x.deb\n")
                elif what == "link":
                    deb.unlink()
                    deb.symlink_to("/etc/hostname")
                self.assertEqual(pc.verify(self.root), (0, 1), what)
                self.assertFalse(d.exists(), what)

    def test_a_good_one_stays_when_a_bad_one_goes(self):
        make(self.root, "good")
        bad = make(self.root, "bad")
        (bad / "debs/libgtk-4-1_4.18.6+panpak1_amd64.deb").write_bytes(b"other")
        os.chmod(bad / "debs", 0o500)  # the store makes trees read-only
        self.assertEqual(pc.verify(self.root), (1, 1))
        self.assertTrue((self.root / "good").is_dir() and not bad.exists())

    def test_nothing_to_verify_is_not_an_error(self):
        self.assertEqual(pc.verify(Path(self.tmp.name) / "none"), (0, 0))
        self.assertEqual(pc.verify(self.root), (0, 0))

    def test_the_root_hash_follows_the_content_and_not_the_place(self):
        a, b = Path(self.tmp.name) / "a", Path(self.tmp.name) / "b"
        make(a, "id1")
        make(b, "id1")
        self.assertEqual(pc.root_hash(a), pc.root_hash(b))
        self.assertEqual(pc.main(["x", "compare", str(a), str(b)]), 0)
        make(a, "id2", {"x_1_amd64.deb": b"x"})
        self.assertNotEqual(pc.root_hash(a), pc.root_hash(b))
        self.assertEqual(pc.main(["x", "compare", str(a), str(b)]), 1)
        # Another content under the same id: another root.
        c = Path(self.tmp.name) / "c"
        make(c, "id1", {"libgtk-4-1_4.18.6+panpak1_amd64.deb": b"CHANGED", "libgtk-4-dev_4.18.6+panpak1_amd64.deb": b"two"})
        self.assertNotEqual(pc.root_hash(b), pc.root_hash(c))
        # An empty folder has no root: not equal to another empty one.
        e1, e2 = Path(self.tmp.name) / "e1", Path(self.tmp.name) / "e2"
        e1.mkdir()
        e2.mkdir()
        self.assertEqual(pc.main(["x", "compare", str(e1), str(e2)]), 1)

    def test_strict_fails_on_a_bad_folder_and_the_usage_is_checked(self):
        d = make(self.root, "a1")
        (d / "debs/libgtk-4-1_4.18.6+panpak1_amd64.deb").write_bytes(b"other")
        self.assertEqual(pc.main(["x", "verify", "--strict", str(self.root)]), 1)
        make(self.root, "a2")
        self.assertEqual(pc.main(["x", "verify", "--strict", str(self.root)]), 0)
        self.assertEqual(pc.main(["x"]), 2)


if __name__ == "__main__":
    unittest.main()
