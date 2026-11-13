#![no_main]
// SPDX-License-Identifier: MIT OR Apache-2.0
use libfuzzer_sys::fuzz_target;
use panpak_core::{filechooser, frames, startfiles};
use std::path::Path;

// What the actor and the stub read from sources that are not trusted: the call of a file dialog (from the app, through the router), the answer text of the
// actor, the addresses the portal returns, the frames that carry them, and the arguments an app is started with. Whatever the bytes are, nothing panics,
// what is accepted is bounded and has the shape the rest of the program counts on.
fuzz_target!(|data: &[u8]| {
    if let Ok(call) = filechooser::parse_call(data, "/run/user/1000/doc/") {
        assert!(call.title.len() <= 512 && call.parent.len() <= 128 + 8);
        assert!(call.names.len() <= 64 && call.names.iter().all(|n| !n.is_empty() && !n.contains(&b'/') && !n.contains(&0)));
        // What goes to the portal reads back as a message.
        let args = filechooser::portal_args(&call, "panpaktoken");
        assert_eq!(args.len(), 3);
        for (k, _) in &call.opts {
            assert!(["accept_label", "modal", "multiple", "directory", "writable", "filters", "current_filter", "choices", "current_name", "current_folder", "current_file", "files"].contains(&k.as_str()), "an option that is not on the list: {k}");
        }
    }
    let text = String::from_utf8_lossy(data);
    if let Some((_, code, dict)) = filechooser::parse_response_text(&text) {
        assert_eq!(code == 0, dict.is_some());
        if let Some(d) = &dict {
            assert!(filechooser::check_result_dict(d).is_some(), "an accepted dictionary is accepted again");
        }
    }
    if let Ok(p) = filechooser::file_uri_to_path(&text) {
        assert!(p.is_absolute() && !p.to_string_lossy().contains('\0'));
        // An address made from a path reads back as the same path.
        use std::os::unix::ffi::OsStrExt;
        let back = filechooser::file_uri_to_path(&format!("file://{}", filechooser::percent_encode(p.as_os_str().as_bytes()))).expect("a made address reads back");
        assert_eq!(back, p);
    }
    // The frames: a request and a response, byte by byte as they come.
    let _ = frames::parse_request(data);
    let _ = frames::parse_response(data);
    // The arguments of a start.
    let _ = startfiles::classify(&text, Path::new("/nonexistent-cwd"));
    if let Some((token, call)) = filechooser::parse_request_payload(data) {
        assert!(call.len() + 4 == data.len());
        let again = filechooser::request_payload(token, call);
        assert_eq!(again, data);
    }
});
