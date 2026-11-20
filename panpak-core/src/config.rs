// SPDX-License-Identifier: MIT OR Apache-2.0
//! The settings of the user: `~/.config/panpak/config.toml`. PANPAK-FORMAT.md section 6.7.
//!
//! ```toml
//! [background]
//! updates = true      # check the update URL of an app once a day, and stage what is new (never apply it by itself without the rules of 6.6)
//! verify_mb = 64      # read this many megabytes a day of the installed files to find damage; 0 turns it off
//!
//! [network]
//! ca_file = "/etc/ssl/my-ca.pem"   # trust these certificate authorities (PEM) INSTEAD of the built-in list, for a network with its own CA; none by default
//!
//! [trust]
//! confirm_domain = true   # ask the domain of an author to confirm its key (PANPAK-FORMAT.md 3.2.1) at a first install and at a succession; false: never ask
//! ```
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub updates: bool,
    pub verify_mb: u64,
    /// A file of certificate authorities (PEM) that replaces the built-in list for the downloads of panpak.
    pub ca_file: Option<String>,
    /// Whether a first install and a succession ask the domain of the author to confirm the key.
    pub confirm_domain: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config { updates: true, verify_mb: 64, ca_file: None, confirm_domain: true }
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
    if let Some(f) = v.get("network").and_then(|n| n.as_table()).and_then(|n| n.get("ca_file")).and_then(|f| f.as_str()).filter(|f| !f.is_empty()) {
        c.ca_file = Some(f.to_string());
    }
    if let Some(b) = v.get("trust").and_then(|t| t.as_table()).and_then(|t| t.get("confirm_domain")).and_then(|b| b.as_bool()) {
        c.confirm_domain = b;
    }
    c
}

pub fn render(c: &Config) -> String {
    let mut t = format!("[background]\nupdates = {}\nverify_mb = {}\n", c.updates, c.verify_mb);
    if let Some(f) = &c.ca_file {
        t.push_str(&format!("\n[network]\nca_file = {}\n", toml::Value::String(f.clone())));
    }
    if !c.confirm_domain {
        t.push_str("\n[trust]\nconfirm_domain = false\n");
    }
    t
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
        "trust.confirm_domain" => {
            c.confirm_domain = match value {
                "on" | "true" | "yes" => true,
                "off" | "false" | "no" => false,
                _ => bail!("trust.confirm_domain takes on or off"),
            }
        }
        "network.ca_file" => c.ca_file = Some(value.to_string()).filter(|v| !v.is_empty() && v != "default"),
        _ => bail!("unknown setting {key}: background.updates, background.verify_mb, network.ca_file, trust.confirm_domain"),
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
        assert_eq!(parse("[background]\nupdates = false\nverify_mb = 5\n"), Config { updates: false, verify_mb: 5, ca_file: None, confirm_domain: true });
        let d = std::env::temp_dir().join(format!("panpak-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let f = d.join("sub/config.toml");
        assert_eq!(set(&f, "background.updates", "off").unwrap(), Config { updates: false, verify_mb: 64, ca_file: None, confirm_domain: true });
        assert_eq!(set(&f, "background.verify_mb", "0").unwrap(), Config { updates: false, verify_mb: 0, ca_file: None, confirm_domain: true });
        assert!(set(&f, "background.updates", "maybe").is_err() && set(&f, "x", "1").is_err());
        assert_eq!(set(&f, "network.ca_file", "/x/my ca.pem").unwrap().ca_file.as_deref(), Some("/x/my ca.pem"));
        assert_eq!(parse(&std::fs::read_to_string(&f).unwrap()).ca_file.as_deref(), Some("/x/my ca.pem"));
        assert_eq!(set(&f, "network.ca_file", "default").unwrap().ca_file, None);
        assert!(!set(&f, "trust.confirm_domain", "off").unwrap().confirm_domain);
        assert!(!parse(&std::fs::read_to_string(&f).unwrap()).confirm_domain);
        assert!(set(&f, "trust.confirm_domain", "on").unwrap().confirm_domain);
        assert!(set(&f, "trust.confirm_domain", "perhaps").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
