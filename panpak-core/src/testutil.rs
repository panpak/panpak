// SPDX-License-Identifier: MIT OR Apache-2.0
//! Helpers for tests that fork or start many processes.
use std::io::Read as _;

/// A test that depends on a descriptor being closed at once runs alone. A lock taken with `flock(2)` belongs to the open
/// file description, not to the descriptor: when another test forks, in this process, while the descriptor is open, the child
/// holds a copy until it calls `exec`, and the lock is not released by `drop` for that long (measured with a stand-alone
/// program: 0 failures in 20000 rounds alone, about 260 when other threads start programs). So each such test runs again in
/// a new process: one test, one thread, a time limit. Returns true where the body is to run (the new process, or a run that
/// is already alone), false in the first process, which has only waited for the result.
pub fn alone() -> bool {
    const VAR: &str = "PANPAK_TEST_ALONE";
    let name = std::thread::current().name().map(str::to_string).unwrap_or_default();
    if std::env::var_os(VAR).is_some() || !name.contains("::") {
        return true;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--test-threads=1", "--nocapture"])
        .env(VAR, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > std::time::Duration::from_secs(120) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{name} did not finish in 120 s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut out = String::new();
    for mut pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read>), child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read>)].into_iter().flatten() {
        let _ = pipe.read_to_string(&mut out);
    }
    assert!(status.success(), "{name} failed when run alone:\n{out}");
    false
}
