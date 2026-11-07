// SPDX-License-Identifier: MIT OR Apache-2.0
//! Memory for builds: what the machine has, how many jobs fit, and a scope with a memory cap that records its peak.
//!
//! A build that runs the machine out of memory freezes the desktop. A recipe says what it needs (`build.memory`, measured at `build.jobs`
//! jobs); `panpak build` reads the memory that is available, lowers the number of jobs if it must, stops if even one job does not fit,
//! and runs the build in a systemd user scope with `MemoryMax`, so that when memory ends the kernel closes the build and not the desktop.
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// `3G`, `0.6G`, `512M`, `100K`, plain bytes.
pub fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('G') => (&s[..s.len() - 1], 1024.0 * 1024.0 * 1024.0),
        Some('M') => (&s[..s.len() - 1], 1024.0 * 1024.0),
        Some('K') => (&s[..s.len() - 1], 1024.0),
        _ => (s, 1.0),
    };
    let n: f64 = num.parse().with_context(|| format!("{s:?} is not a size like 3G, 0.6G or 512M"))?;
    if !(n.is_finite() && n > 0.0 && n < 1e6) {
        bail!("{s:?} is not a size like 3G, 0.6G or 512M");
    }
    Ok((n * mult) as u64)
}

/// `3.7G` (one decimal, in gibibytes).
pub fn fmt_size(b: u64) -> String {
    format!("{:.1}G", b as f64 / GIB)
}

/// What the machine can give a build.
#[derive(Debug, Clone, Copy)]
pub struct Avail {
    /// `MemAvailable`: what can be had without swapping.
    pub mem: u64,
    /// `SwapTotal`.
    pub swap: u64,
    pub cpus: usize,
}

/// Read `/proc/meminfo` text (`MemAvailable`, `SwapTotal` in kB).
pub fn parse_meminfo(text: &str) -> (u64, u64) {
    let kb = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)).and_then(|r| r.split_whitespace().next()).and_then(|n| n.parse::<u64>().ok()).unwrap_or(0) * 1024;
    (kb("MemAvailable:"), kb("SwapTotal:"))
}

pub fn available() -> Avail {
    let (mem, swap) = std::fs::read_to_string("/proc/meminfo").map(|t| parse_meminfo(&t)).unwrap_or((0, 0));
    Avail { mem, swap, cpus: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) }
}

/// What a build will run with.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub jobs: usize,
    /// `MemoryMax` of the scope.
    pub cap: u64,
    /// What to tell the user (jobs lowered, memory short).
    pub notes: Vec<String>,
}

/// `memory` and `per_job` are the recipe's (`None`: not declared); `jobs` is the most it was tested with (`None`: the processors).
/// With no declared memory nothing can be checked: the build runs with the processors and a cap of the available memory.
pub fn plan(jobs: Option<u32>, memory: Option<u64>, per_job: Option<u64>, a: Avail) -> Result<Plan> {
    let max_jobs = jobs.map(|j| j as usize).unwrap_or(a.cpus).min(a.cpus).max(1);
    let mut notes = Vec::new();
    let Some(mem) = memory else {
        notes.push("the recipe does not say how much memory it needs (build.memory): nothing was checked, and the peak is measured and shown at the end".to_string());
        return Ok(Plan { jobs: max_jobs, cap: a.mem.max(1 << 30), notes });
    };
    let declared_jobs = jobs.map(|j| j as u64).unwrap_or(max_jobs as u64).max(1);
    let per = per_job.unwrap_or(mem / declared_jobs).max(1);
    let mut j = max_jobs;
    if a.mem < mem && a.mem > 0 {
        // The part of the need that does not depend on the number of jobs is what is left of `memory` with one job: `mem - per * (declared - 1)`.
        let base = mem.saturating_sub(per.saturating_mul(declared_jobs - 1));
        let fit = if a.mem > base { ((a.mem - base) / per) as usize + 1 } else { 0 };
        if fit == 0 {
            return Err(panpak_core::coded(
                panpak_core::ErrorCode::E6005,
                format!("this build needs {} (with {} job(s)) and only {} of memory is available; free memory or close programs, then try again", fmt_size(mem), declared_jobs, fmt_size(a.mem)),
            ));
        }
        j = j.min(fit);
        notes.push(format!("{} of memory is available and the recipe needs {}: running {j} job(s) instead of {max_jobs}", fmt_size(a.mem), fmt_size(mem)));
    }
    let need = mem.max(per.saturating_mul(j as u64));
    Ok(Plan { jobs: j, cap: need + need / 4, notes })
}

/// Where the kernel keeps the memory counters of a user scope.
pub fn scope_cgroup(unit: &str) -> PathBuf {
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/{unit}.scope"))
}

/// Can a scope be made here (`systemd-run --user`)?
pub fn scope_available() -> bool {
    Command::new("systemd-run").args(["--user", "--scope", "--quiet", "--collect", "--", "true"]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Run `argv` in a systemd user scope with `MemoryMax=cap` and `MemorySwapMax=swap`, output to `log`. Returns the status and the peak
/// memory of the scope (sampled twice a second; the counter only grows, so the last sample is close to the true peak).
pub fn run_capped(argv: &[std::ffi::OsString], cap: u64, swap: u64, log: &Path, scoped: bool) -> Result<(ExitStatus, Option<u64>)> {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let out = std::fs::File::create(log).with_context(|| format!("create {}", log.display()))?;
    let err = out.try_clone()?;
    let unit = format!("panpak-build-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) % 1_000_000);
    let mut cmd;
    if scoped {
        cmd = Command::new("systemd-run");
        cmd.args(["--user", "--scope", "--quiet", "--collect", "--unit", &unit, "-p", &format!("MemoryMax={cap}"), "-p", &format!("MemorySwapMax={swap}"), "--"]).args(argv);
    } else {
        cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
    }
    cmd.stdin(Stdio::null()).stdout(out).stderr(err);
    let mut child = cmd.spawn().with_context(|| format!("run {}", argv[0].to_string_lossy()))?;
    let peak = std::sync::Arc::new(AtomicU64::new(0));
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let poll = scoped.then(|| {
        let (peak, stop, path) = (peak.clone(), stop.clone(), scope_cgroup(&unit).join("memory.peak"));
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Some(v) = std::fs::read_to_string(&path).ok().and_then(|t| t.trim().parse::<u64>().ok()) {
                    peak.store(v, Ordering::Relaxed);
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        })
    });
    let status = child.wait()?;
    stop.store(true, Ordering::Relaxed);
    if let Some(p) = poll {
        let _ = p.join();
    }
    let p = peak.load(Ordering::Relaxed);
    Ok((status, (scoped && p > 0).then_some(p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("3G").unwrap(), 3 * G);
        assert_eq!(parse_size("0.5G").unwrap(), G / 2);
        assert_eq!(parse_size("512M").unwrap(), 512 << 20);
        assert_eq!(parse_size("1024").unwrap(), 1024);
        for bad in ["", "G", "-1G", "abc", "0", "1e9999G", "NaNG"] {
            assert!(parse_size(bad).is_err(), "{bad}");
        }
        assert_eq!(fmt_size(3 * G + G / 2), "3.5G");
    }

    #[test]
    fn meminfo() {
        let t = "MemTotal:       14652000 kB\nMemAvailable:    8000000 kB\nSwapTotal:       4194300 kB\n";
        assert_eq!(parse_meminfo(t), (8_000_000 * 1024, 4_194_300 * 1024));
        assert_eq!(parse_meminfo(""), (0, 0));
    }

    fn avail(mem: u64, cpus: usize) -> Avail {
        Avail { mem, swap: 0, cpus }
    }

    #[test]
    fn jobs_follow_the_memory() {
        // Ghostty: 4G at 3 jobs, 1G per job: base 2G.
        let p = plan(Some(3), Some(4 * G), Some(G), avail(8 * G, 16)).unwrap();
        assert_eq!((p.jobs, p.cap), (3, 5 * G));
        assert!(p.notes.is_empty());
        let p = plan(Some(3), Some(4 * G), Some(G), avail(3 * G, 16)).unwrap();
        assert_eq!(p.jobs, 2, "{:?}", p.notes);
        assert!(p.notes[0].contains("running 2 job(s) instead of 3"));
        let p = plan(Some(3), Some(4 * G), Some(G), avail(2 * G + G / 2, 16)).unwrap();
        assert_eq!(p.jobs, 1);
        // Not even one job fits.
        let e = plan(Some(3), Some(4 * G), Some(G), avail(G, 16)).unwrap_err().to_string();
        assert!(e.contains("needs 4.0G") && e.contains("only 1.0G"), "{e}");
        assert_eq!(panpak_core::code_of(&plan(Some(3), Some(4 * G), Some(G), avail(G, 16)).unwrap_err(), panpak_core::ErrorCode::E6000), panpak_core::ErrorCode::E6005);
        // Never more jobs than processors.
        assert_eq!(plan(Some(6), Some(G), None, avail(16 * G, 2)).unwrap().jobs, 2);
        // No declared memory: runs, says it did not check.
        let p = plan(None, None, None, avail(8 * G, 4)).unwrap();
        assert_eq!(p.jobs, 4);
        assert!(p.notes[0].contains("does not say"));
    }
}
