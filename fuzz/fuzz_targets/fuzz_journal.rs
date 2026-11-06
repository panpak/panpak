#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;
use panpak_core::removal::{valid_package_name, valid_root, Journal};

// The journal of a removal names files that are deleted from the store. Whatever the bytes, a journal that
// is accepted lists only plain package names and tree roots, and writing it out and reading it again gives
// the same journal.
fuzz_target!(|data: &[u8]| {
    let Ok(j) = Journal::parse(data) else { return };
    assert!(panpak_core::validate_app_id(&j.app_id).is_ok());
    for p in &j.packages {
        assert!(valid_package_name(p) && !p.contains('/') && !p.starts_with('.') && !p.contains(".."), "{p:?}");
    }
    for r in &j.roots {
        assert!(valid_root(r), "{r:?}");
    }
    assert_eq!(Journal::parse(j.render().as_bytes()).unwrap(), j);
});
