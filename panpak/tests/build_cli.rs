// SPDX-License-Identifier: MIT OR Apache-2.0
// `panpak init` and the checks of `panpak build` that need neither the network nor an image: a recipe with holes, a lock for another image,
// a source that is not pinned. The build itself (image, compiler, sandbox) is tried by hand and by the harness.
use std::{fs, path::{Path, PathBuf}, process::{Command, Output}};

fn cli(dir: &Path, args: &[&str]) -> Output {
    let home = dir.join("home");
    fs::create_dir_all(home.join("cache")).unwrap();
    Command::new(env!("CARGO_BIN_EXE_panpak"))
        .args(args)
        .current_dir(dir)
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env_remove("PANPAK_SECRET_FILE")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn lab(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("build-cli-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("proj/data")).unwrap();
    fs::write(d.join("proj/meson.build"), "project('demo', 'c', version: '1.2.3')\ndependency('gtk4')\n").unwrap();
    fs::write(d.join("proj/data/org.example.Demo.desktop.in"), "[Desktop Entry]\nName=Demo\n").unwrap();
    d
}

fn key(d: &Path) -> PathBuf {
    let k = d.join("secret.key");
    let o = cli(d, &["keygen", "-o", k.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    k
}

#[test]
fn init_writes_a_recipe_and_a_lock_and_build_refuses_the_holes() {
    let d = lab("init");
    let proj = d.join("proj");
    let o = cli(&d, &["init", proj.to_str().unwrap()]);
    let t = text(&o);
    assert!(o.status.success() && t.contains("found: build system meson, id org.example.Demo") && t.contains("TODO"), "{t}");
    assert!(proj.join("panpak.toml").is_file() && proj.join("panpak.lock").is_file());
    let lock = fs::read_to_string(proj.join("panpak.lock")).unwrap();
    assert!(lock.contains("name = \"debian-13\"") && lock.contains("manifest_digest = \"sha256:") && lock.contains("snapshot = "), "{lock}");
    // Not overwritten.
    let o = cli(&d, &["init", proj.to_str().unwrap()]);
    assert!(!o.status.success() && text(&o).contains("does not overwrite"), "{}", text(&o));
    // build stops at the holes (no commit, no entry), before any download.
    let k = key(&d);
    let stub = d.join("stub");
    fs::write(&stub, "x").unwrap();
    let o = cli(&d, &["build", proj.to_str().unwrap(), "--secret-file", k.to_str().unwrap(), "--stub", stub.to_str().unwrap()]);
    let t = text(&o);
    assert!(!o.status.success() && t.contains("PANPAK-E6000") && t.contains("panpak.toml"), "{t}");
    assert!(!d.join("home/cache/panpak/build/images").exists(), "nothing was fetched");
    let _ = Command::new("chmod").args(["-R", "u+w"]).arg(&d).output();
    let _ = fs::remove_dir_all(&d);
}

fn recipe(extra_source: &str) -> String {
    format!("[app]\nid = \"org.example.Demo\"\nname = \"Demo\"\nversion = \"1\"\nentry = \"bin/demo\"\n[source]\n{extra_source}\n[build]\nsystem = \"meson\"\n")
}

#[test]
fn build_refuses_a_source_that_is_not_pinned_and_a_lock_for_another_image() {
    let d = lab("pin");
    let proj = d.join("proj");
    let k = key(&d);
    let stub = d.join("stub");
    fs::write(&stub, "x").unwrap();
    let build = |d: &Path| cli(d, &["build", proj.to_str().unwrap(), "--secret-file", k.to_str().unwrap(), "--stub", stub.to_str().unwrap()]);
    for (what, src, want) in [
        ("a tag", "git = \"https://example.org/a.git\"\ncommit = \"v1.0\"", "full commit id"),
        ("no hash", "url = \"https://example.org/a.tar.gz\"", "sha256"),
        ("plain http", "git = \"http://example.org/a.git\"\ncommit = \"0123456789abcdef0123456789abcdef01234567\"", "https://"),
    ] {
        fs::write(proj.join("panpak.toml"), recipe(src)).unwrap();
        let o = build(&d);
        assert!(!o.status.success() && text(&o).contains(want), "{what}: {}", text(&o));
    }
    // A lock for another image than the recipe's.
    fs::write(proj.join("panpak.toml"), recipe("git = \"https://example.org/a.git\"\ncommit = \"0123456789abcdef0123456789abcdef01234567\"").replace("system = \"meson\"", "system = \"meson\"\nimage = \"debian-13\"")).unwrap();
    fs::write(proj.join("panpak.lock"), "[image]\nname = \"debian-12\"\nmanifest_digest = \"sha256:7378260c0a098d27ac206225db5e7d92b8c7c84ea7afccc86eeb513756bbd392\"\nsnapshot = \"20261107T000000Z\"\n").unwrap();
    let o = build(&d);
    assert!(!o.status.success() && text(&o).contains("unknown build image"), "a lock for an image this panpak does not know: {}", text(&o));
    let _ = Command::new("chmod").args(["-R", "u+w"]).arg(&d).output();
    let _ = fs::remove_dir_all(&d);
}
