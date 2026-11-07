// SPDX-License-Identifier: MIT OR Apache-2.0
// `panpak convert` with icons, end to end: an AppImage is built here (squashfs written by `backhand`), converted, installed, and
// what is installed is looked at. Includes hostile AppImages: links that lead out of the tree must make nothing land outside.
use std::{
    fs,
    io::Cursor,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn work(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("convert-icons-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("run")).unwrap();
    fs::set_permissions(d.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
    d
}

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_panpak"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env_remove("PANPAK_SECRET_FILE")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn fixture(name: &str) -> Vec<u8> {
    fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../panpak-icon/tests/data").join(name)).unwrap()
}

const ID: &str = "org.example.converted";

struct Lab {
    home: PathBuf,
    key: PathBuf,
    stub: PathBuf,
}

/// The scratch folder holds a copy of the `panpak` binary (about 100 MB in a debug build): removed when the test ends, kept if it failed.
impl Drop for Lab {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = Command::new("chmod").args(["-R", "u+w"]).arg(&self.home).output();
            let _ = fs::remove_dir_all(&self.home);
        }
    }
}

fn lab(name: &str) -> Lab {
    let home = work(name);
    let key = home.join("secret.key");
    assert!(cli(&home, &["keygen", "-o", key.to_str().unwrap()]).status.success());
    let stub = home.join("stub");
    fs::write(&stub, "#!/bin/sh\necho 1\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let o = cli(&home, &["install-runtime", stub.to_str().unwrap(), "--allow-unsigned"]);
    assert!(o.status.success(), "{}", text(&o));
    Lab { home, key, stub }
}

enum Node<'a> {
    File(&'a str, Vec<u8>),
    Link(&'a str, &'a str),
}

/// An AppImage: some bytes of "runtime" and a squashfs with the given nodes (parents are made as needed).
fn appimage(nodes: Vec<Node>) -> Vec<u8> {
    use backhand::{FilesystemWriter, NodeHeader};
    let h = NodeHeader::new(0o755, 0, 0, 0);
    let mut w = FilesystemWriter::default();
    for n in nodes {
        match n {
            Node::File(p, d) => {
                if let Some(parent) = Path::new(p).parent().filter(|p| !p.as_os_str().is_empty()) {
                    w.push_dir_all(parent, h).unwrap();
                }
                w.push_file(Cursor::new(d), p, h).unwrap();
            }
            Node::Link(p, target) => {
                if let Some(parent) = Path::new(p).parent().filter(|p| !p.as_os_str().is_empty()) {
                    w.push_dir_all(parent, h).unwrap();
                }
                w.push_symlink(target, p, h).unwrap();
            }
        }
    }
    let mut out = vec![0u8; 4096];
    let mut sq = Cursor::new(Vec::new());
    w.write(&mut sq).unwrap();
    out.extend_from_slice(&sq.into_inner());
    out
}

fn desktop_file() -> Vec<u8> {
    b"[Desktop Entry]\nType=Application\nName=Converted\nExec=app\nIcon=app\nStartupWMClass=Converted\n".to_vec()
}

impl Lab {
    fn convert(&self, image: &[u8], extra: &[&str]) -> (Output, PathBuf) {
        let src = self.home.join("app.AppImage");
        fs::write(&src, image).unwrap();
        let out = self.home.join("converted.panpak");
        let _ = fs::remove_file(&out);
        let mut args = vec!["convert", src.to_str().unwrap(), "--id", ID, "--stub", self.stub.to_str().unwrap(), "--secret-file", self.key.to_str().unwrap(), "-o", out.to_str().unwrap()];
        args.extend_from_slice(extra);
        (cli(&self.home, &args), out)
    }
    fn manifest(&self, pkg: &Path) -> String {
        let m = self.home.join("manifest.toml");
        let o = cli(&self.home, &["dump-manifest", pkg.to_str().unwrap(), m.to_str().unwrap()]);
        assert!(o.status.success(), "{}", text(&o));
        fs::read_to_string(m).unwrap()
    }
    fn install(&self, pkg: &Path) -> Output {
        cli(&self.home, &["install", "--yes", pkg.to_str().unwrap()])
    }
    fn icon(&self, side: u32) -> PathBuf {
        self.home.join(format!(".local/share/icons/hicolor/{side}x{side}/apps/panpak.{ID}.png"))
    }
    fn entry(&self) -> String {
        fs::read_to_string(self.home.join(format!(".local/share/applications/panpak.{ID}.desktop"))).unwrap()
    }
    fn icons_installed(&self) -> Vec<u32> {
        (1..=1024).filter(|s| self.icon(*s).exists()).collect()
    }
}

/// The layout of a real AppImage (Audacity): the files are in `share/`, `usr` is a link to `.`, and `.DirIcon` and the root
/// icon are links.
fn real_layout() -> Vec<Node<'static>> {
    vec![
        Node::File("AppRun", b"#!/bin/sh\nexec \"$APPDIR/bin/app\" \"$@\"\n".to_vec()),
        Node::File("app.desktop", desktop_file()),
        Node::File("bin/app", b"#!/bin/sh\nexit 0\n".to_vec()),
        Node::Link("usr", "."),
        Node::File("share/icons/hicolor/256x256/apps/app.png", fixture("ok-256-rgba.png")),
        Node::File("share/icons/hicolor/32x32/apps/app.png", fixture("bad-text.png")),
        Node::File("share/icons/hicolor/16x16/apps/app.png", fixture("ok-16-rgba.png")),
        Node::Link("app.png", "share/icons/hicolor/256x256/apps/app.png"),
        Node::Link(".DirIcon", "app.png"),
    ]
}

#[test]
fn an_appimage_with_usr_linked_to_dot_gets_its_icons_cleaned_in_the_package() {
    let l = lab("usr-dot");
    let (o, pkg) = l.convert(&appimage(real_layout()), &[]);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&o);
    // The text chunk of the 32 icon is removed and said; two other sources have the size of the first (256).
    assert!(t.contains("panpak pack: icon usr/share/icons/hicolor/32x32/apps/app.png: removed the chunks tEXt"), "{t}");
    assert!(t.contains("note: icon not used: app.png: same size (256x256) as .DirIcon") && t.contains("same size (256x256) as .DirIcon, which is used"), "{t}");
    // The signed manifest points only into `.panpak/icons`, the largest first.
    let m = l.manifest(&pkg);
    assert!(m.contains("\".panpak/icons/256.png\", \".panpak/icons/32.png\", \".panpak/icons/16.png\""), "{m}");
    assert!(!m.contains("share/icons"), "{m}");
    let o = l.install(&pkg);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(l.icons_installed(), [16, 32, 256]);
    assert_eq!(fs::read(l.icon(256)).unwrap(), fixture("ok-256-rgba.png"));
    let cleaned = fs::read(l.icon(32)).unwrap();
    assert_ne!(cleaned, fixture("bad-text.png"));
    assert!(!cleaned.windows(4).any(|w| w == b"tEXt"));
    assert!(l.entry().contains(&format!("\nIcon=panpak.{ID}\n")), "{}", l.entry());
    // Converting again gives the same package.
    let first = fs::read(&pkg).unwrap();
    let (o, pkg2) = l.convert(&appimage(real_layout()), &[]);
    assert!(o.status.success());
    assert_eq!(first, fs::read(pkg2).unwrap(), "the same AppImage always gives the same package");
}

#[test]
fn a_usr_link_to_a_folder_outside_the_tree_is_never_written_through_or_read() {
    let l = lab("usr-outside");
    let outside = l.home.join("outside");
    fs::create_dir_all(outside.join("share/icons/hicolor/48x48/apps")).unwrap();
    fs::write(outside.join("share/icons/hicolor/48x48/apps/app.png"), fixture("ok-48-rgba.png")).unwrap();
    fs::write(outside.join("victim"), "keep").unwrap();
    let snapshot = |d: &Path| {
        let mut v = Vec::new();
        let mut stack = vec![d.to_path_buf()];
        while let Some(p) = stack.pop() {
            for e in fs::read_dir(&p).unwrap().flatten() {
                v.push((e.path(), fs::metadata(e.path()).map(|m| m.len()).unwrap_or(0)));
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                }
            }
        }
        v.sort();
        v
    };
    let before = snapshot(&outside);
    let abs = outside.to_str().unwrap().to_string();
    let image = appimage(vec![
        Node::File("app.desktop", desktop_file()),
        Node::File("bin/app", b"#!/bin/sh\nexit 0\n".to_vec()),
        Node::Link("usr", &abs),
        // The icon is only reachable through the link; `.DirIcon` and a `panpak.toml` point out as well.
        Node::Link(".DirIcon", &format!("{abs}/share/icons/hicolor/48x48/apps/app.png")),
        Node::Link("panpak.toml", &format!("{abs}/victim")),
    ]);
    let (o, pkg) = l.convert(&image, &[]);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&o);
    assert!(t.contains("points outside the tree"), "{t}");
    assert!(t.contains("WARNING: no usable PNG icon"), "{t}");
    assert_eq!(snapshot(&outside), before, "a file was written outside the tree");
    assert_eq!(fs::read_to_string(outside.join("victim")).unwrap(), "keep");
    // The package has no icon, and the app gets the generic one.
    assert!(!l.manifest(&pkg).contains("icons"), "{}", l.manifest(&pkg));
    assert!(l.install(&pkg).status.success());
    assert!(l.icons_installed().is_empty());
    assert!(l.entry().contains("\nIcon=application-x-executable\n"));
}

#[test]
fn a_bin_link_to_a_folder_outside_the_tree_stops_the_wrapper_from_being_written() {
    let l = lab("bin-outside");
    let outside = l.home.join("outside-bin");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("app"), "#!/bin/sh\nexit 0\n").unwrap();
    let abs = outside.to_str().unwrap().to_string();
    // The entry is a script (so convert wants a wrapper in `bin/`), and `bin` leads out of the tree.
    let image = appimage(vec![Node::File("app.desktop", desktop_file()), Node::Link("bin", &abs)]);
    let (o, _) = l.convert(&image, &[]);
    assert!(!o.status.success(), "{}", text(&o));
    let names: Vec<_> = fs::read_dir(&outside).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["app"], "a file was written outside the tree: {names:?}");
}

#[test]
fn an_appimage_with_a_dot_panpak_folder_is_refused() {
    let l = lab("reserved");
    let mut nodes = real_layout();
    nodes.push(Node::File(".panpak/icons/48.png", fixture("ok-48-rgba.png")));
    let (o, _) = l.convert(&appimage(nodes), &[]);
    assert!(!o.status.success() && text(&o).contains("reserved"), "{}", text(&o));
}

#[test]
fn the_icon_option_replaces_what_the_appimage_has_and_is_read_from_outside_the_tree() {
    let l = lab("icon-option");
    let mine = l.home.join("mine.png");
    fs::write(&mine, fixture("bad-icc.png")).unwrap();
    let (o, pkg) = l.convert(&appimage(real_layout()), &["--icon", mine.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("removed the chunks iCCP"), "{}", text(&o));
    assert!(l.manifest(&pkg).contains("icons = [\".panpak/icons/32.png\"]"), "{}", l.manifest(&pkg));
    assert!(l.install(&pkg).status.success());
    assert_eq!(l.icons_installed(), [32]);
    // A bad file given with --icon is an error, and so is a missing one.
    fs::write(&mine, "<svg/>").unwrap();
    let (o, _) = l.convert(&appimage(real_layout()), &["--icon", mine.to_str().unwrap()]);
    assert!(!o.status.success() && text(&o).contains("SVG"), "{}", text(&o));
    let (o, _) = l.convert(&appimage(real_layout()), &["--icon", l.home.join("missing.png").to_str().unwrap()]);
    assert!(!o.status.success());
}

#[test]
fn only_a_dir_icon_link_is_enough() {
    let l = lab("dir-icon");
    let image = appimage(vec![
        Node::File("app.desktop", desktop_file()),
        Node::File("bin/app", b"#!/bin/sh\nexit 0\n".to_vec()),
        Node::File("real/icon.png", fixture("bad-text.png")),
        Node::Link(".DirIcon", "real/icon.png"),
    ]);
    let (o, pkg) = l.convert(&image, &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(l.manifest(&pkg).contains("icons = [\".panpak/icons/32.png\"]"), "{}", l.manifest(&pkg));
    assert!(l.install(&pkg).status.success());
    assert_eq!(l.icons_installed(), [32]);
}
