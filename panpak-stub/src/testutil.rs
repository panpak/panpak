// SPDX-License-Identifier: MIT OR Apache-2.0
//! Helpers for tests that fork or start many processes.
use std::io::Read as _;

/// A test that cannot run here says why and returns false (the caller returns). In the CI (`PANPAK_CI=1`) it fails with the reason instead:
/// a skip there is a test that passes without having run.
pub fn skip(reason: &str) -> bool {
    assert!(std::env::var_os("PANPAK_CI").is_none_or(|v| v != "1"), "cannot run here and PANPAK_CI=1 forbids a skip: {reason}");
    eprintln!("skipped: {reason}");
    false
}

/// The Landlock ABI of the kernel of the CI runner (the Ubuntu 24.04 image of GitHub): V8. A test that needs a newer one cannot run there; it
/// says so (`skip_known`) and runs on a machine with a newer kernel. The list of them is what the CI log shows as `known skip`.
pub const RUNNER_ABI: landlock::ABI = landlock::ABI::V8;

/// A skip that the CI cannot avoid, for a reason that is named (the runner's kernel is older than the test needs): it is reported and does
/// not fail with `PANPAK_CI=1`.
pub fn skip_known(reason: &str) -> bool {
    use std::io::Write as _;
    let name = std::thread::current().name().unwrap_or("?").to_string();
    eprintln!("known skip (the CI runner cannot run this): {reason}");
    // The test may run in a child process whose output is not shown when it passes: the CI reads this file (`PANPAK_SKIP_LOG`).
    if let Some(path) = std::env::var_os("PANPAK_SKIP_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{name}: {reason}");
        }
    }
    false
}

/// The tests that fork run the body in the child, which allocates and takes locks. A fork of a process
/// that has other threads (the other tests, the opener) can copy the allocator in the middle of a call by
/// another thread, and the child then waits for ever. So each of them runs again alone in a new
/// process: one test, one thread, a time limit. Returns true where the body is to run (the new process,
/// or a run that is already alone), false in the first process, which has only waited for the result.
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
