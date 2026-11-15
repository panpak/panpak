// SPDX-License-Identifier: MIT OR Apache-2.0
//! The settings of the user: `~/.config/panpak/config.toml`. PANPAK-FORMAT.md section 6.7.
//!
//! ```toml
//! [background]
//! updates = true      # check the update URL of an app once a day, and stage what is new (never apply it by itself without the rules of 6.6)
//! verify_mb = 64      # read this many megabytes a day of the installed files to find damage; 0 turns it off
//! ```
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub updates: bool,
    pub verify_mb: u64,
}

impl Default for Config {
    fn default() -> Config {
        Config { updates: true, verify_mb: 64 }
    }
}

pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|c| c.join("panpak").join("config.toml"))
}

pub fn parse(text: &str) -> Config {
    let mut c = Config::default();
    let Ok(v) = text.parse::<toml::Table>() else { return c };
    if let Some(b) = v.get("background").and_then(|b| b.as_table()) {
        if let Some(u) = b.get("updates").and_then(|u| u.as_bool()) {
            c.updates = u;
        }
        if let Some(m) = b.get("verify_mb").and_then(|m| m.as_integer()) {
            c.verify_mb = m.clamp(0, 100_000) as u64;
        }
    }
    c
}

pub fn render(c: &Config) -> String {
    format!("[background]\nupdates = {}\nverify_mb = {}\n", c.updates, c.verify_mb)
}

/// The settings; the defaults when there is no file or it is not valid.
pub fn load() -> Config {
    path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| parse(&t)).unwrap_or_default()
}

/// Change one setting by its name (`background.updates`, `background.verify_mb`) and write the file.
pub fn set(config_file: &Path, key: &str, value: &str) -> Result<Config> {
    let mut c = std::fs::read_to_string(config_file).map(|t| parse(&t)).unwrap_or_default();
    match key {
        "background.updates" => {
            c.updates = match value {
                "on" | "true" | "yes" => true,
                "off" | "false" | "no" => false,
                _ => bail!("background.updates takes on or off"),
            }
        }
        "background.verify_mb" => c.verify_mb = value.parse::<u64>().ok().filter(|m| *m <= 100_000).context("background.verify_mb takes a number of megabytes, 0 to 100000")?,
        _ => bail!("unknown setting {key}: background.updates, background.verify_mb"),
    }
    if let Some(dir) = config_file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::durable::write_file(config_file, render(&c).as_bytes(), 0o644)?;
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_on_a_bad_file_and_set_round_trips() {
        assert_eq!(parse("not toml ["), Config::default());
        assert_eq!(parse("[background]\nupdates = false\nverify_mb = 5\n"), Config { updates: false, verify_mb: 5 });
        let d = std::env::temp_dir().join(format!("panpak-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let f = d.join("sub/config.toml");
        assert_eq!(set(&f, "background.updates", "off").unwrap(), Config { updates: false, verify_mb: 64 });
        assert_eq!(set(&f, "background.verify_mb", "0").unwrap(), Config { updates: false, verify_mb: 0 });
        assert!(set(&f, "background.updates", "maybe").is_err() && set(&f, "x", "1").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
