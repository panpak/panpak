# SPDX-License-Identifier: MIT OR Apache-2.0
"""Unit tests of the app harness: the list of apps, the cache key, the signing, the readers of window lists. No network, no display, no panpak binary.
Run: python3 -m unittest discover -s tests/apps -p 'test_*.py'"""
import json
import struct
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import appcfg  # noqa: E402
import display  # noqa: E402
import ed25519  # noqa: E402
import pipeline  # noqa: E402

HERE = Path(__file__).resolve().parent
GOOD = """
[apps.one]
kind = ["qt", "tray"]
id = "org.example.One"
url = "https://example.invalid/One.AppImage"
sha256 = "%s"
convert_opts = ["--entry", "usr/bin/one"]
[apps.two]
kind = "gtk"
cli = true
cli_args = ["--version"]
id = "org.example.Two"
url = "https://example.invalid/Two.AppImage"
[apps.three]
kind = ["gtk"]
recipe = '''
[app]
id = "org.example.Three"
'''
""" % ("ab" * 32)


class Config(unittest.TestCase):
    def test_apps_groups_and_defaults(self):
        apps, _ = appcfg.parse(GOOD)
        self.assertEqual(sorted(apps), ["one", "three", "two"])
        self.assertEqual((apps["one"].display, apps["three"].display), ("x11", "wayland"))
        self.assertEqual(apps["three"].id, "org.example.Three", "the id of a recipe is read from it")
        self.assertEqual(apps["one"].groups, {"qt", "tray", "appimage"})
        self.assertEqual(apps["two"].groups, {"gtk", "cli", "appimage"})
        self.assertEqual(apps["three"].groups, {"gtk", "source"})
        self.assertIn("x11 = true", apps["one"].permissions)

    def test_selection(self):
        apps, _ = appcfg.parse(GOOD)
        names = lambda groups=(), n=(): [a.name for a in appcfg.select(apps, groups, n)]  # noqa: E731
        self.assertEqual(names(), ["one", "two", "three"])
        self.assertEqual(names(["gtk"]), ["two", "three"])
        self.assertEqual(names(["qt"], ["three"]), ["one", "three"])
        self.assertEqual(names(n=["two"]), ["two"])
        with self.assertRaisesRegex(appcfg.ConfigError, "no app in the group 'nothing'"):
            names(["nothing"])
        with self.assertRaisesRegex(appcfg.ConfigError, "no app named 'nope'"):
            names(n=["nope"])

    def test_mistakes_are_named(self):
        cases = {
            "[apps.a]\nkind = 'qt'\n": "needs url and id",
            "[apps.a]\nid = 'x.y'\nurl = 'https://e.invalid/a'\n": "kind is missing",
            "[apps.a]\nkind = 'qt'\nid = 'x.y'\nurl = 'https://e.invalid/a'\nsha256 = 'XYZ'\n": "sha256 must be 64",
            "[apps.a]\nkind = 'qt'\nid = 'x.y'\nurl = 'https://e.invalid/a'\nbogus = 1\n": "unknown key",
            "[apps.a]\nkind = 'qt'\nrecipe = 'x'\nurl = 'https://e.invalid/a'\n": "no url or convert_opts",
            "[apps.'a b']\nkind = 'qt'\n": "app name",
            "[apps.a]\nkind = 'qt'\nid = 'x.y'\nurl = 'u'\ndisplay = 'mir'\n": "display must be",
            "[nope]\n": "unknown table",
            "not toml [": "not valid TOML",
        }
        for text, want in cases.items():
            with self.assertRaisesRegex(appcfg.ConfigError, want, msg=text):
                appcfg.parse(text)

    def test_the_example_has_the_three_recipes_and_nothing_of_anyone_elses(self):
        text = (HERE / "apps.example.toml").read_text()
        apps, _ = appcfg.load(HERE / "apps.example.toml")
        self.assertEqual(sorted(apps), ["ghostty", "qbittorrent", "text-editor"])
        self.assertTrue(all(a.source for a in apps.values()))
        import re
        for url in re.findall(r"https?://\S*AppImage", text):
            self.assertIn("example.invalid", url, "no AppImage of a third party in the example")
        import getpass
        for word in ("/home/", "/Users/", str(Path.home()), getpass.getuser()):
            self.assertNotIn(word, text, "no personal path or user name in the example")


class Signing(unittest.TestCase):
    def test_rfc8032_vectors(self):
        seed = bytes.fromhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
        self.assertEqual(ed25519.public_key(seed).hex(), "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
        self.assertEqual(ed25519.sign(seed, b"").hex(), "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b")
        seed2 = bytes.fromhex("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb")
        self.assertEqual(ed25519.sign(seed2, bytes.fromhex("72")).hex(), "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00")
        with self.assertRaises(ValueError):
            ed25519.sign(b"short", b"")

    def test_adding_permissions_changes_the_manifest_and_signs_it_again(self):
        seed = bytes(range(32))
        root = bytes(32)
        man = b'[app]\nid = "a.b"\n[permissions]\nx11 = true\naudio = true\n[update]\nkey = "k"\n'
        stub = b"STUB" * 8
        body = stub + b"INDEX"
        moff = len(body)
        pkg = body + man + b"\0" * 64 + struct.pack("<8sIIQQQQ32s", b"PANPAK\0\0", 1, 0, len(stub), 5, moff, len(man), root)
        with tempfile.TemporaryDirectory() as d:
            src, dst = Path(d) / "in", Path(d) / "out"
            src.write_bytes(pkg)
            pipeline.add_permissions(src, dst, "[permissions]\naudio = false\nnetwork = true\n", seed)
            out = dst.read_bytes()
        magic, ver, flags, ioff, ilen, moff2, mlen2, root2 = pipeline.read_footer(out)
        newman = out[moff2:moff2 + mlen2]
        self.assertEqual((moff2, root2, out[:moff]), (moff, root, body))
        text = newman.decode()
        # `convert` wrote x11 and audio; the block says audio again and adds network: x11 stays, audio is the block's.
        self.assertEqual(text.count("[permissions]"), 1)
        self.assertIn("x11 = true", text)
        self.assertIn("audio = false", text)
        self.assertNotIn("audio = true", text)
        self.assertIn("network = true", text)
        self.assertEqual(out[moff2 + mlen2:moff2 + mlen2 + 64], ed25519.sign(seed, newman + root))


class Readers(unittest.TestCase):
    def test_xwininfo(self):
        out = '     0x20000a "Terminal": ("xterm" "XTerm")  800x600+10+20  +10+20\n     0x200001 (has no name): ()  10x10+-1+-1  +-1+-1\n'
        w = display.parse_xwininfo(out)
        self.assertEqual(w["0x20000a"], dict(title="Terminal", cls="XTerm", w=800, h=600))
        self.assertEqual(w["0x200001"]["w"], 10)

    def test_sway_tree_keeps_only_the_windows(self):
        tree = {"type": "root", "nodes": [{"type": "output", "nodes": [{"type": "workspace", "nodes": [
            {"type": "con", "id": 7, "app_id": "org.example.App", "name": "Title", "rect": {"width": 640, "height": 480}, "nodes": []},
            {"type": "con", "id": 8, "nodes": [{"type": "con", "id": 9, "window_properties": {"class": "X"}, "name": "t", "rect": {"width": 1, "height": 2}, "nodes": []}]},
        ]}]}]}
        w = display.parse_sway_tree(tree)
        self.assertEqual(sorted(w), ["7", "9"])
        self.assertEqual(w["7"], dict(title="Title", cls="org.example.App", w=640, h=480))

    def test_median_leaves_the_first_start_out(self):
        self.assertEqual(pipeline.median_after_first([9.0, 1.0, 2.0, 3.0, 4.0, 5.0]), 3.0)
        self.assertEqual(pipeline.median_after_first([2.5]), 2.5)


class CacheKey(unittest.TestCase):
    def ctx(self, d, a, b):
        c = object.__new__(pipeline.Ctx)
        c.digests = (a, b)
        return c

    def test_key_follows_the_source_the_binaries_and_the_options(self):
        apps, _ = appcfg.parse(GOOD)
        one = apps["one"]
        c = self.ctx(None, "p1", "s1")
        k = pipeline.cache_key(c, one, "src")
        self.assertEqual(k, pipeline.cache_key(c, one, "src"))
        self.assertNotEqual(k, pipeline.cache_key(c, one, "src2"), "another AppImage or recipe")
        self.assertNotEqual(k, pipeline.cache_key(self.ctx(None, "p2", "s1"), one, "src"), "another panpak")
        self.assertNotEqual(k, pipeline.cache_key(self.ctx(None, "p1", "s2"), one, "src"), "another stub")
        one.convert_opts = ["--entry", "other"]
        self.assertNotEqual(k, pipeline.cache_key(c, one, "src"), "other options")


if __name__ == "__main__":
    unittest.main()
