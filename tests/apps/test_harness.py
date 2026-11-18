# SPDX-License-Identifier: MIT OR Apache-2.0
"""Unit tests of the app harness: the list of apps, the cache key, the signing, the readers of window lists. No network, no display, no panpak binary.
Run: python3 -m unittest discover -s tests/apps -p 'test_*.py'"""
import json
import re
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
import sigfmt  # noqa: E402

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


class CriticalList(unittest.TestCase):
    """The list `[format].critical` is made again by every tool that signs a manifest (sigfmt.with_critical); the table it uses is a copy of
    the one of `panpak-core`, and a test reads the Rust source so that the two cannot drift apart."""

    MAN = ('[app]\nid = "a.b"\n[format]\nmin_panpak = 1\ncritical = ["permissions.old"]\ncompression = "zstd"\n'
           'chunking = "c"\npacked_by = "p"\n[permissions]\nx11 = true\naudio = true\n[launch]\nargs = ["a"]\n[env]\nA = "b"\n[desktop]\nwm_class = "X"\n')

    def test_the_list_is_made_again_from_what_the_manifest_uses(self):
        out = sigfmt.with_critical(self.MAN)
        self.assertIn('critical = ["env", "launch.args", "permissions.audio", "permissions.x11"]', out)
        self.assertNotIn("permissions.old", out)
        self.assertEqual(sigfmt.with_critical(out), out)
        self.assertEqual(sigfmt.with_critical(self.MAN.encode()), out.encode())
        self.assertEqual(sigfmt.with_critical('[app]\nid = "a.b"\n[permissions]\nx11 = true\n'), '[app]\nid = "a.b"\n[permissions]\nx11 = true\n')

    def test_the_table_is_the_one_of_the_rust_code(self):
        src = (HERE.parents[1] / "panpak-core/src/lib.rs").read_text()
        for sect, const in (("abi", "ABI_KEYS"), ("permissions", "PERM_KEYS"), ("launch", "LAUNCH_KEYS"), ("runtime", "RUNTIME_KEYS")):
            m = re.search(r"const %s: &\[&str\] = &\[([^\]]*)\];" % const, src)
            self.assertIsNotNone(m, const)
            self.assertEqual(set(re.findall(r'"([a-z_0-9]+)"', m.group(1))), sigfmt.CRITICAL_SECTIONS[sect], sect)
        m = re.search(r"pub const SECTIONS: &\[\(&str, bool\)\] = &\[(.*?)\n\];", src, re.S)
        critical = {s for s, c in re.findall(r'\("([a-z]+)", (true|false)\)', m.group(1)) if c == "true"}
        self.assertEqual(critical, set(sigfmt.CRITICAL_SECTIONS) | {"env"})


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
        self.assertEqual(out[moff2 + mlen2:len(out) - 80], sigfmt.block(ed25519.sign(seed, sigfmt.message(ver, flags, root, newman))))


FAKE_PANPAK = r"""#!/usr/bin/env python3
import os, sys
log = os.path.join(os.environ["HOME"], "calls.log")
a = sys.argv[1:]
if a[0] == "convert":
    open(log, "a").write(" ".join(a) + "\n")
    open(a[a.index("-o") + 1], "wb").write(open(os.path.join(os.environ["HOME"], "base-package"), "rb").read())
elif a[0] == "doctor":
    last = open(log).read().splitlines()[-1].split()
    allowed = [last[i + 1] for i, w in enumerate(last) if w == "--allow-missing"]
    miss = [n for n in ("libaa.so.1", "Bb.so", "libcc.so.3") if n not in allowed]
    if miss:
        print("KO   unresolved libraries: " + ", ".join(miss) + " missing on the host and not included in the tree: the app cannot start here (try another distro)")
    else:
        print("OK   verified package")
"""


class ConvertLoop(unittest.TestCase):
    def test_every_unresolved_library_is_allowed_in_one_round(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            fake = d / "panpak"
            fake.write_text(FAKE_PANPAK)
            fake.chmod(0o755)
            man = b'[app]\nid = "a.b"\n[update]\nkey = "k"\n'
            body = b"STUB" * 8 + b"INDEX"
            (d / "base-package").write_bytes(body + man + b"\0" * 64 + struct.pack("<8sIIQQQQ32s", b"PANPAK\0\0", 1, 0, 32, 5, len(body), len(man), bytes(32)))
            (d / "harness.key").write_text(bytes(range(32)).hex())
            ctx = object.__new__(pipeline.Ctx)
            ctx.work, ctx.pkgs, ctx.panpak, ctx.stub, ctx.keyfile = d, d, fake, d / "stub", d / "harness.key"
            apps, _ = appcfg.parse(GOOD)
            pkg, notes = pipeline.convert(ctx, apps["one"], "k1", (d / "src.AppImage", "digest", []))
            calls = (d / "calls.log").read_text().splitlines()
            self.assertEqual(len(calls), 2, f"one round to find them all, one with them allowed: {calls}")
            self.assertEqual(sorted(w for i, w in enumerate(calls[1].split()[1:]) if calls[1].split()[i] == "--allow-missing"), ["Bb.so", "libaa.so.1", "libcc.so.3"])
            self.assertTrue(pkg.exists())
            self.assertTrue(any("allow_missing added by the harness: libaa.so.1, Bb.so, libcc.so.3" in n for n in notes), notes)


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


class LibraryPath(unittest.TestCase):
    TREES = "/h/.local/share/panpak/trees"

    def row(self, pid, ppid, exe, ld=None):
        return {"pid": pid, "ppid": ppid, "exe": exe, "ld": ld}

    def test_only_the_app_and_what_it_starts_hold_the_variable(self):
        rel = "/$ORIGIN/.panpak-lib-abcd1234/.farm:/$ORIGIN/.panpak-lib-abcd1234/lib"
        rows = [
            self.row(10, 1, "/h/.local/share/panpak/runtimes/1/panpak-stub"),
            self.row(11, 10, "/usr/bin/bwrap"),
            self.row(12, 11, self.TREES + "/abc/bin/app", rel),
            self.row(13, 12, "/usr/bin/zenity", rel),          # started by the app: it inherits the relative value, which is inert
        ]
        self.assertEqual(display.library_path_offenders(rows, self.TREES), [])

    def test_the_outer_processes_must_not_hold_a_value_that_takes_effect(self):
        farm = "/h/.local/share/panpak/farms/abcd/.farm"
        rel = "/$ORIGIN/.panpak-lib-abcd1234/.farm"
        rows = [
            self.row(10, 1, "/h/.local/share/panpak/runtimes/1/panpak-stub", farm),
            self.row(11, 10, "/usr/bin/bwrap", "/h/.local/share/panpak/farms/abcd:" + self.TREES + "/abc/lib"),
            self.row(12, 11, self.TREES + "/abc/bin/app", rel),
        ]
        bad = display.library_path_offenders(rows, self.TREES)
        self.assertEqual(len(bad), 2)
        self.assertIn("pid 10", bad[0] + bad[1])
        self.assertIn("pid 11", bad[0] + bad[1])
        self.assertTrue(all("takes effect" in b for b in bad), bad)

    def test_the_relative_value_in_a_program_of_the_host_is_inert_and_no_fault(self):
        # A converted AppImage whose entry is a shell script: the host's `sh` or `bash` runs it under the value that the stub made, `/$ORIGIN/.panpak-lib-...`,
        # which names a folder that does not exist at the root: nothing is loaded through it.
        rel = "/$ORIGIN/.panpak-lib-abcd1234/.farm:/$ORIGIN/.panpak-lib-abcd1234/lib"
        rows = [
            self.row(10, 1, "/h/.local/share/panpak/runtimes/1/panpak-stub"),
            self.row(11, 10, "/usr/bin/bash", rel),
            self.row(12, 11, self.TREES + "/abc/usr/bin/app", rel),
            self.row(13, 1, "/usr/bin/dash", "/usr/lib/x86_64-linux-gnu"),   # no path of the store: no effect either
        ]
        self.assertEqual(display.library_path_offenders(rows, self.TREES), [])

    def test_a_path_of_the_store_in_the_value_is_a_fault(self):
        rows = [self.row(12, 1, self.TREES + "/abc/bin/app", "/h/.local/share/panpak/farms/abc:" + self.TREES + "/abc/lib")]
        bad = display.library_path_offenders(rows, self.TREES)
        self.assertEqual(len(bad), 1)
        self.assertIn("not relative", bad[0])

    def test_the_rows_of_this_process_can_be_read(self):
        import os
        rows = display.process_rows(os.getpid())
        me = [r for r in rows if r["pid"] == os.getpid()]
        self.assertEqual(len(me), 1)
        self.assertTrue(me[0]["exe"].startswith("/"))


class ScriptsOfConvertedApps(unittest.TestCase):
    TREES = "/h/.local/share/panpak/trees"

    def rows(self):
        shotcut = "/h/.local/share/panpak/trees/abc/usr/bin/bin/shotcut"
        own = self.TREES + "/abc/usr/bin/lib:" + self.TREES + "/abc/usr/bin/lib/qt6"
        return [
            {"pid": 11, "ppid": 10, "exe": "/usr/bin/dash", "ld": "/$ORIGIN/.panpak-lib-ab/.farm"},
            {"pid": 12, "ppid": 11, "exe": shotcut, "ld": own},      # its own script set an absolute value
        ]

    def test_an_absolute_value_that_the_script_of_a_converted_app_set_is_a_warning(self):
        faults, notes = display.library_path_report(self.rows(), self.TREES, converted=True)
        self.assertEqual(faults, [])
        self.assertEqual(len(notes), 1)
        self.assertIn("bin/shotcut", notes[0])
        self.assertIn("script of the app", notes[0])

    def test_the_same_value_in_an_app_made_from_source_is_a_fault(self):
        faults, notes = display.library_path_report(self.rows(), self.TREES, converted=False)
        self.assertEqual(len(faults), 1)
        self.assertEqual(notes, [])
        self.assertEqual(faults, display.library_path_offenders(self.rows(), self.TREES))

    def test_a_converted_app_with_the_relative_value_has_neither(self):
        rows = [{"pid": 12, "ppid": 1, "exe": self.TREES + "/abc/bin/app", "ld": "/$ORIGIN/.panpak-lib-ab/lib"}]
        self.assertEqual(display.library_path_report(rows, self.TREES, converted=True), ([], []))


class Windows(unittest.TestCase):
    def test_a_window_must_be_wide_enough_and_tall_enough_unless_it_is_the_first_one_of_the_app(self):
        self.assertTrue(display.window_counts({"w": 500, "h": 114}, (200, 120), True), "LMMS: the first window is a dialog of 500 x 114")
        self.assertFalse(display.window_counts({"w": 500, "h": 114}, (200, 120), False), "a later window of that height is a helper")
        self.assertFalse(display.window_counts({"w": 24, "h": 24}, (200, 120), True), "a tray icon or a helper is not narrow-tolerated")
        self.assertTrue(display.window_counts({"w": 800, "h": 600}, (200, 120), False))


class BuildNotes(unittest.TestCase):
    def test_the_time_of_each_phase_and_the_memory_reach_the_log_and_nothing_else(self):
        out = "\n".join([
            "panpak build: Text Editor 48.3 (org.gnome.TextEditor)",
            "panpak build: time image: 312.4 s",
            "panpak build: time patch: 0.0 s (kept)",
            "panpak build: time source: 4.1 s",
            "panpak build: building with 6 job(s), memory cap 1G",
            "panpak build: time compile: 11.0 s",
            "panpak build: time collect: 3.2 s",
            "panpak build: time pack: 2.5 s",
            "panpak build: /x/te.panpak: 106 libraries (101 MB), built in 336 s",
            "panpak build: peak memory 0.5G (build.memory is 1G)",
            "a line of the compiler that says time image: 1 s",
            "",
        ])
        self.assertEqual(pipeline.build_notes(out), [
            "time image: 312.4 s", "time patch: 0.0 s (kept)", "time source: 4.1 s", "time compile: 11.0 s", "time collect: 3.2 s", "time pack: 2.5 s",
            "peak memory 0.5G (build.memory is 1G)"])
        self.assertEqual(pipeline.build_notes(""), [])


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
