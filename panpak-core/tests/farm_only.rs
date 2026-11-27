// SPDX-License-Identifier: MIT OR Apache-2.0
//! The library path of an app is the farm alone, and the farm has a link for every library of the tree (PANPAK-FORMAT.md section 7.1).
use panpak_core::abi::{apply, Source};
use std::{collections::HashMap, fs, path::{Path, PathBuf}};

struct Lab {
    dir: PathBuf,
}

impl Lab {
    fn new(tag: &str) -> Lab {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("target").join(format!("farm-only-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for d in ["tree/lib", "tree/lib/gimp/plug-ins/a", "tree/lib/gimp/plug-ins/b", "tree/bin", "store", "sys"] {
            fs::create_dir_all(dir.join(d)).unwrap();
        }
        for f in ["lib/libcore.so.1", "lib/gimp/plug-ins/a/libplugin-a.so", "lib/gimp/plug-ins/b/libplugin-b.so.2", "lib/gimp/plug-ins/b/plug-b", "bin/app", "sys/libhost.so.1"] {
            let p = if f.starts_with("sys") { dir.join(f) } else { dir.join("tree").join(f) };
            fs::write(p, b"x").unwrap();
        }
        Lab { dir }
    }

    fn bundled(&self) -> HashMap<String, PathBuf> {
        HashMap::from([
            ("libcore.so.1".to_string(), PathBuf::from("lib/libcore.so.1")),
            ("libplugin-a.so".to_string(), PathBuf::from("lib/gimp/plug-ins/a/libplugin-a.so")),
            ("libplugin-b.so.2".to_string(), PathBuf::from("lib/gimp/plug-ins/b/libplugin-b.so.2")),
            ("plug-b".to_string(), PathBuf::from("lib/gimp/plug-ins/b/plug-b")),
            ("libhost.so.1".to_string(), PathBuf::from("lib/libhost-old.so.1")),
        ])
    }

    fn decisions(&self) -> Vec<(String, Source)> {
        vec![
            ("libcore.so.1".to_string(), Source::Bundled(PathBuf::from("lib/libcore.so.1"))),
            ("libhost.so.1".to_string(), Source::Host(self.dir.join("sys/libhost.so.1"))),
        ]
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn farm_of(lab: &Lab) -> PathBuf {
    lab.dir.join("store/farms/root")
}

fn listing(farm: &Path) -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = fs::read_dir(farm).unwrap().map(|e| {
        let e = e.unwrap();
        (e.file_name().to_string_lossy().into_owned(), fs::read_link(e.path()).unwrap())
    }).collect();
    v.sort();
    v
}

#[test]
fn the_list_is_the_farm_alone_and_the_farm_has_every_library_of_the_tree() {
    let lab = Lab::new("all");
    let plan = apply(&lab.dir.join("store"), "root", &lab.dir.join("tree"), &lab.decisions(), &lab.bundled()).unwrap();
    assert_eq!(plan.ld_path, vec![farm_of(&lab)], "the library path is the farm alone, not a folder of the tree");
    let names: Vec<String> = listing(&farm_of(&lab)).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["libcore.so.1", "libhost.so.1", "libplugin-a.so", "libplugin-b.so.2"], "a library of a plug-in folder is there; a program is not");
    let farm = listing(&farm_of(&lab));
    let target = |n: &str| farm.iter().find(|(m, _)| m == n).unwrap().1.clone();
    assert_eq!(target("libhost.so.1"), lab.dir.join("sys/libhost.so.1"), "a decided name keeps its decision: the system copy, not the copy of the tree");
    for n in ["libcore.so.1", "libplugin-a.so", "libplugin-b.so.2"] {
        assert!(target(n).is_relative(), "{n}: a library of the tree is linked by a relative path, so that the store can be moved");
        assert!(farm_of(&lab).join(n).canonicalize().unwrap().starts_with(lab.dir.join("tree").canonicalize().unwrap()), "{n} reaches the tree");
    }
}

#[test]
fn the_same_call_gives_the_same_farm_and_a_stale_link_goes() {
    let lab = Lab::new("same");
    let tree = lab.dir.join("tree");
    let first = apply(&lab.dir.join("store"), "root", &tree, &lab.decisions(), &lab.bundled()).unwrap();
    let before = listing(&farm_of(&lab));
    let second = apply(&lab.dir.join("store"), "root", &tree, &lab.decisions(), &lab.bundled()).unwrap();
    assert_eq!(first.ld_path, second.ld_path);
    assert_eq!(listing(&farm_of(&lab)), before, "the farm is the same at every start");
    // A library that is no longer in the inventory loses its link.
    let mut smaller = lab.bundled();
    smaller.remove("libplugin-a.so");
    apply(&lab.dir.join("store"), "root", &tree, &lab.decisions(), &smaller).unwrap();
    assert!(!farm_of(&lab).join("libplugin-a.so").exists() && fs::symlink_metadata(farm_of(&lab).join("libplugin-a.so")).is_err());
}

#[test]
fn a_name_that_is_not_a_file_name_is_never_a_link_and_the_loader_is_not_in_the_farm() {
    let lab = Lab::new("hostile");
    let tree = lab.dir.join("tree");
    // A file outside the farm that a hostile name could reach: it must stay.
    let victim = lab.dir.join("store/victim.so.desktop");
    fs::write(&victim, b"mine").unwrap();
    let mut bundled = lab.bundled();
    for bad in ["../victim.so.desktop", "../../store/victim.so.desktop", "/etc/x.so", "a/b.so", "..", ".", "", "x\n.so", "ld-linux-x86-64.so.2", "ld.so"] {
        bundled.insert(bad.to_string(), PathBuf::from("lib/libcore.so.1"));
    }
    let plan = apply(&lab.dir.join("store"), "root", &tree, &lab.decisions(), &bundled).unwrap();
    assert_eq!(plan.ld_path, vec![farm_of(&lab)]);
    assert_eq!(fs::read(&victim).unwrap(), b"mine", "a file outside the farm is not touched");
    assert!(fs::symlink_metadata(lab.dir.join("store/victim.so.desktop")).unwrap().file_type().is_file());
    let names: Vec<String> = listing(&farm_of(&lab)).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["libcore.so.1", "libhost.so.1", "libplugin-a.so", "libplugin-b.so.2"], "only plain names, and not the loader");
    // A decided name that is not a file name stops the start.
    let mut decisions = lab.decisions();
    decisions.push(("../victim.so.desktop".to_string(), Source::Bundled(PathBuf::from("lib/libcore.so.1"))));
    let err = apply(&lab.dir.join("store"), "root", &tree, &decisions, &lab.bundled()).err().expect("a decided name that is not a file name stops the start");
    assert!(format!("{err:#}").contains("cannot be a file name"), "{err:#}");
    assert_eq!(fs::read(&victim).unwrap(), b"mine");
}
