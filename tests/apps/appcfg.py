# SPDX-License-Identifier: MIT OR Apache-2.0
"""The list of apps of the harness: a TOML file that is NOT in the repository (the path comes from `--apps FILE` or the variable PANPAK_APPS),
and `apps.example.toml`, which has the three recipes of step 13a and nothing of anyone else's.

    [settings]                      # optional
    download_dir = "/some/folder"   # where AppImages are kept (default: <work>/dl)

    [apps.NAME]
    kind = "qt"                     # a word or a list of words: the groups that --group selects (gtk, qt, electron, cli, source, ...)
    id = "org.example.App"          # AppImage: the id of the package (convert --id)
    url = "https://..."             # AppImage: where to download it
    sha256 = "<64 hex>"             # AppImage: the hash that the download must have (without it the run says it was not checked)
    convert_opts = ["--entry", "usr/bin/app"]
    permissions = "[permissions]\\nx11 = true\\n"   # AppImage: the [permissions] block added to the converted package
    recipe = "<text of a recipe>"   # from source: the recipe (or recipe_dir = "folder with panpak.toml")
    display = "x11" | "wayland"     # default: x11 for an AppImage, wayland for a recipe
    seed = { "config/app/app.conf" = "text" }   # files made in the private data folder of the app before its first start
    env = { NAME = "value" }        # extra environment for the start
    cli = true                      # a program with no window: time of `cli_args`
    cli_args = ["--version"]
    min_window = [200, 120]         # the smallest window that counts
"""
import re
import sys
from pathlib import Path

try:
    import tomllib
except ImportError:  # Python before 3.11
    tomllib = None

KEYS = {"kind", "id", "url", "sha256", "convert_opts", "permissions", "recipe", "recipe_dir", "display", "seed", "env", "cli", "cli_args", "min_window"}
DEFAULT_PERMISSIONS = '[permissions]\nx11 = true\n\n[[permissions.xdg_dirs]]\nname = "documents"\naccess = "rw"\n'


class ConfigError(Exception):
    pass


class App:
    def __init__(self, name, raw):
        self.name = name
        self.raw = raw
        self.kinds = [raw["kind"]] if isinstance(raw.get("kind"), str) else list(raw.get("kind", []))
        self.source = "recipe" in raw or "recipe_dir" in raw
        if self.source and "recipe_dir" in raw:
            self.recipe_text = (Path(raw["recipe_dir"]) / "panpak.toml").read_text()
        else:
            self.recipe_text = raw.get("recipe")
        self.display = raw.get("display", "wayland" if self.source else "x11")
        self.id = raw.get("id") or (recipe_id(self.recipe_text) if self.source else None)
        self.url = raw.get("url")
        self.sha256 = raw.get("sha256")
        self.convert_opts = list(raw.get("convert_opts", []))
        self.permissions = raw.get("permissions", DEFAULT_PERMISSIONS)
        self.seed = dict(raw.get("seed", {}))
        self.env = {k: str(v) for k, v in dict(raw.get("env", {})).items()}
        self.cli = bool(raw.get("cli", False))
        self.cli_args = list(raw.get("cli_args", ["--version"]))
        self.min_window = tuple(raw.get("min_window", (200, 120)))

    @property
    def groups(self):
        return set(self.kinds) | ({"source"} if self.source else {"appimage"}) | ({"cli"} if self.cli else set())


def recipe_id(text):
    m = re.search(r'^\s*id\s*=\s*"([^"]+)"', text or "", re.M)
    return m.group(1) if m else None


def parse(text):
    """The apps of a configuration text: ({name: App}, settings)."""
    if tomllib is None:
        raise ConfigError("Python 3.11 or newer is needed to read the list of apps (tomllib)")
    try:
        raw = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        raise ConfigError(f"the list of apps is not valid TOML: {e}")
    unknown = set(raw) - {"settings", "apps"}
    if unknown:
        raise ConfigError(f"unknown table(s) {sorted(unknown)}: the file has [settings] and [apps.NAME]")
    apps = {}
    for name, a in raw.get("apps", {}).items():
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", name):
            raise ConfigError(f"app name {name!r}: letters, digits, '_', '.', '-'")
        bad = set(a) - KEYS
        if bad:
            raise ConfigError(f"app {name}: unknown key(s) {sorted(bad)}")
        has_recipe = "recipe" in a or "recipe_dir" in a
        if has_recipe and ("url" in a or "convert_opts" in a):
            raise ConfigError(f"app {name}: a recipe is built from source: no url or convert_opts")
        if not has_recipe:
            if not a.get("url") or not a.get("id"):
                raise ConfigError(f"app {name}: an AppImage needs url and id")
            if a.get("sha256") and not re.fullmatch(r"[0-9a-f]{64}", a["sha256"]):
                raise ConfigError(f"app {name}: sha256 must be 64 lowercase hex digits")
        if a.get("display", "x11" if not has_recipe else "wayland") not in ("x11", "wayland"):
            raise ConfigError(f"app {name}: display must be x11 or wayland")
        if "kind" not in a:
            raise ConfigError(f"app {name}: kind is missing (the groups that --group selects)")
        apps[name] = App(name, a)
        if apps[name].id is None:
            raise ConfigError(f"app {name}: the recipe has no [app] id")
    return apps, raw.get("settings", {})


def load(path):
    p = Path(path)
    try:
        return parse(p.read_text())
    except OSError as e:
        raise ConfigError(f"cannot read the list of apps {p}: {e}")


def select(apps, groups=(), names=()):
    """The apps to run: those of the groups, then the names (an unknown name or group is an error); with neither, all of them."""
    if not groups and not names:
        return list(apps.values())
    known = set().union(*(a.groups for a in apps.values())) if apps else set()
    for g in groups:
        if g not in known:
            raise ConfigError(f"no app in the group {g!r} (groups: {', '.join(sorted(known))})")
    for n in names:
        if n not in apps:
            raise ConfigError(f"no app named {n!r} (apps: {', '.join(sorted(apps))})")
    chosen = {a.name: a for a in apps.values() if a.groups & set(groups)}
    chosen.update({n: apps[n] for n in names})
    return [a for a in apps.values() if a.name in chosen]


if __name__ == "__main__":
    a, _ = load(sys.argv[1])
    print(", ".join(sorted(a)))
