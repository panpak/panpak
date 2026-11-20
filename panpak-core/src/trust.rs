// SPDX-License-Identifier: MIT OR Apache-2.0
//! Who may sign an app (PANPAK-FORMAT.md §3.2): the `[trust]` table of the manifest, the succession chain that moves an installed app from one root
//! key to another, the state kept for an installed app (`apps/<id>/trust`), and the phrase of words that holds the recovery key.
//!
//! Three keys are in play. The **root** (the mother key) signs the packages. The **recovery** key is made with it and is never in a package: the package
//! carries only its commitment, `blake3:<hash of the public key>`, from the first release on. A thief of the root cannot change the commitment (a succession
//! signed by the root keeps it); the person who holds the recovery key reveals it in a succession that names a new root and a new commitment, and that
//! succession wins over any one signed by the root.

use crate::{coded, ErrorCode};
use anyhow::{ensure, Context, Result};
use ed25519_dalek::VerifyingKey;
use std::path::Path;

/// The label of the message that a succession signature covers (the same key signs packages and successions: the labels keep them apart).
pub const LABEL_SUCCESSION: &[u8] = b"panpak/succession/v2\0";
/// The longest chain of successions that a package may carry.
pub const MAX_CHAIN: usize = 8;

/// The `[trust]` table. Written by `panpak pack` (the author gives only `domain`) and signed with the rest of the manifest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trust {
    /// The keys that may sign the package. A list so that more than one can come later; for now exactly one.
    #[serde(default)]
    pub root: Vec<String>,
    /// How many of `root` must sign. Only 1 is understood; a larger number is reserved (a reader refuses it with "update panpak").
    #[serde(default)]
    pub threshold: u32,
    /// `blake3:<64 hex>`: the hash of the public key of the recovery key.
    #[serde(default)]
    pub recovery: String,
    /// The domain of the author, checked against the app id (`example.org` for `org.example.app`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// The successions that lead from an older root to `root[0]`, oldest first (at most `MAX_CHAIN`). Only a package signed by a new root carries it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain: Vec<Step>,
}

/// One succession: `to` becomes the root, and the recovery commitment is `recovery` afterwards.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// `root` (signed by the root that was) or `recovery` (signed by the recovery key, which is revealed in `from`).
    pub by: String,
    /// The public key that signed: the old root, or the recovery public key.
    pub from: String,
    /// The new root.
    pub to: String,
    /// The recovery commitment after this step: the same as before for a step of the root, a new one for a step of the recovery key.
    pub recovery: String,
    /// 128 hex characters: the signature of `from` over `succession_message`.
    pub sig: String,
}

/// The keys of `[trust]` and of a step, for the schema check and for the list of critical paths.
pub const TRUST_KEYS: &[&str] = &["root", "threshold", "recovery", "domain", "chain"];
pub const STEP_KEYS: &[&str] = &["by", "from", "to", "recovery", "sig"];

/// `blake3:<hash of the public key>`.
pub fn commitment_of(vk: &VerifyingKey) -> String {
    format!("blake3:{}", crate::hex(blake3::hash(vk.as_bytes()).as_bytes()))
}

/// The 32 bytes of a commitment text, which must be `blake3:` and 64 lowercase hex characters.
pub fn parse_commitment(s: &str) -> Result<[u8; 32]> {
    let h = s.strip_prefix("blake3:").context("a recovery commitment starts with `blake3:`")?;
    ensure!(h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "a recovery commitment is `blake3:` and 64 lowercase hex characters");
    let mut out = [0u8; 32];
    for (i, d) in out.iter_mut().enumerate() {
        *d = u8::from_str_radix(&h[2 * i..2 * i + 2], 16)?;
    }
    Ok(out)
}

/// The bytes a succession signature covers: the label, the app id (length first), who signs (0 the root, 1 the recovery key), the key that signs, the new
/// root and the recovery commitment afterwards. A signature belongs to one app and one step.
pub fn succession_message(app_id: &str, by_recovery: bool, from: &VerifyingKey, to: &VerifyingKey, new_recovery: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(LABEL_SUCCESSION.len() + 3 + app_id.len() + 97);
    m.extend_from_slice(LABEL_SUCCESSION);
    m.extend_from_slice(&(app_id.len() as u16).to_le_bytes());
    m.extend_from_slice(app_id.as_bytes());
    m.push(by_recovery as u8);
    m.extend_from_slice(from.as_bytes());
    m.extend_from_slice(to.as_bytes());
    m.extend_from_slice(new_recovery);
    m
}

/// The rule between a domain and an app id: the domain, read backwards, is the beginning of the id, and has at least two labels
/// (`example.org` and `org.example.app`).
pub fn domain_fits_id(domain: &str, app_id: &str) -> bool {
    let labels: Vec<&str> = domain.split('.').collect();
    let ok_label = |l: &str| !l.is_empty() && l.len() <= 63 && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !l.starts_with('-') && !l.ends_with('-');
    if domain.len() > 253 || labels.len() < 2 || !labels.iter().all(|l| ok_label(l)) {
        return false;
    }
    let id: Vec<&str> = app_id.split('.').collect();
    id.len() >= labels.len() && labels.iter().rev().zip(id.iter()).all(|(d, i)| d.eq_ignore_ascii_case(i))
}

fn bad(what: &str) -> anyhow::Error {
    coded(ErrorCode::E0003, format!("[trust] {what} (corrupt or tampered file: redownload it from a trusted source)"))
}

/// The message for a package that has no `[trust]` table: it was made before the table existed.
pub fn require(man: &crate::Manifest) -> Result<&Trust> {
    man.trust.as_ref().ok_or_else(|| coded(ErrorCode::E0011, "this package has no [trust] table: it was made with a test version of panpak; make it again with the current panpak"))
}

/// The `[trust]` table of a manifest whose signature is verified: a reader that does not know what it asks refuses with "update panpak", and the forms are checked.
pub fn check(man: &crate::Manifest) -> Result<()> {
    let t = require(man)?;
    let newer = |what: &str| coded(ErrorCode::E0002, format!("{what}: update panpak to the latest version"));
    if t.root.is_empty() {
        return Err(bad("has no root key"));
    }
    if t.root.len() != 1 {
        return Err(newer("this package names more than one root key, which this panpak does not understand"));
    }
    if t.threshold != 1 {
        return Err(newer(&format!("this package asks for {} signatures of the root, which this panpak does not understand", t.threshold)));
    }
    crate::parse_pubkey(&t.root[0]).map_err(|_| bad("root is not a key of the form ed25519:<64 hex>"))?;
    parse_commitment(&t.recovery).map_err(|_| bad("recovery is not a commitment of the form blake3:<64 hex>"))?;
    if let Some(d) = &t.domain {
        crate::ensure_code!(domain_fits_id(d, &man.app.id), ErrorCode::E0003, "[trust] domain {d:?} does not fit the app id {}: the domain, read backwards, must be the beginning of the id (example.org for org.example.app)", man.app.id);
    }
    if t.chain.len() > MAX_CHAIN {
        return Err(bad("chain is longer than 8 steps"));
    }
    for s in &t.chain {
        if s.by != "root" && s.by != "recovery" {
            return Err(newer(&format!("a succession of this package is signed by {:?}, which this panpak does not know", s.by)));
        }
        crate::parse_pubkey(&s.from).map_err(|_| bad("a succession has a bad `from` key"))?;
        crate::parse_pubkey(&s.to).map_err(|_| bad("a succession has a bad `to` key"))?;
        parse_commitment(&s.recovery).map_err(|_| bad("a succession has a bad recovery commitment"))?;
        if s.sig.len() != 128 || !s.sig.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(bad("a succession signature is not 128 lowercase hex characters"));
        }
    }
    Ok(())
}

/// What an installed app pins: its root and its recovery commitment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub root: String,
    pub recovery: String,
}

fn sig_of(hex: &str) -> Result<ed25519_dalek::Signature> {
    let mut raw = [0u8; 64];
    for (i, d) in raw.iter_mut().enumerate() {
        *d = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| coded(ErrorCode::E1005, "succession signature is not hex"))?;
    }
    Ok(ed25519_dalek::Signature::from_bytes(&raw))
}

/// The keys of a new manifest against what the app pins. The same root and commitment need nothing. Otherwise the chain must lead from the pin to them: the steps
/// before the first one that the pin can start are history and are ignored; a step of the root must start from the root that was, and keeps the commitment; a step
/// of the recovery key reveals a key whose hash is the commitment that was, and sets the next one. Returns the steps that were followed (`0` when nothing changed).
pub fn follow(app_id: &str, pin: &Pin, new: &Trust) -> Result<usize> {
    let new_root = new.root.first().context("[trust] has no root")?;
    if *new_root == pin.root && new.recovery == pin.recovery {
        return Ok(0);
    }
    let err = |m: &str| coded(ErrorCode::E1005, m.to_string());
    if new.chain.is_empty() {
        return Err(err("the key of the package is not the pinned one and the package carries no succession: refused (the author must make a succession with `panpak pack --rotate-from-file`, or with the recovery key)"));
    }
    let (mut root, mut recovery) = (pin.root.clone(), pin.recovery.clone());
    let mut followed = 0;
    for s in &new.chain {
        let from = crate::parse_pubkey(&s.from)?;
        let to = crate::parse_pubkey(&s.to)?;
        let new_rec = parse_commitment(&s.recovery)?;
        let by_recovery = s.by == "recovery";
        let starts_here = if by_recovery { commitment_of(&from) == recovery } else { s.from == root };
        if followed == 0 && !starts_here {
            continue; // history before the pin
        }
        if !starts_here {
            return Err(err("the succession is broken: a step does not start from the key that the one before it made"));
        }
        if by_recovery {
            // The recovery key is revealed; its hash is the commitment that was pinned.
            ensure!(commitment_of(&from) == recovery, "the recovery key of the succession is not the one that the app committed to");
        } else if s.recovery != recovery {
            return Err(err("a succession signed by the root cannot change the recovery commitment: refused"));
        }
        from.verify_strict(&succession_message(app_id, by_recovery, &from, &to, &new_rec), &sig_of(&s.sig)?).map_err(|_| err("invalid succession: bad signature"))?;
        root = s.to.clone();
        recovery = s.recovery.clone();
        followed += 1;
    }
    if followed == 0 {
        return Err(err("the succession of the package does not start from the key that this app pins: refused"));
    }
    if root != *new_root || recovery != new.recovery {
        return Err(err("the succession does not end at the key and the recovery commitment that the package names: refused"));
    }
    Ok(followed)
}

// ---------------------------------------------------------------------------------------------------------------------------------------------
// The state of an installed app: `apps/<id>/trust`

/// The file, in the folder of the app.
pub const FILE: &str = "trust";

/// What the client keeps for an installed app, as `key=value` lines. One writer replaces the whole file (`durable::write_file`), so a stop leaves the old
/// one or the new one. `root` may be on two lines for a moment while a key is replaced (the start accepts either). Unknown lines are kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// The pinned root key(s).
    pub roots: Vec<String>,
    /// The pinned recovery commitment.
    pub recovery: Option<String>,
    /// Where the updates come from: `author` (the `[update].url` of the author's package). Another value is reserved for a catalog or a distribution.
    pub source: String,
    /// The highest floor seen (it folds the file `floor` of an earlier layout).
    pub floor: u64,
    /// The highest revision seen.
    pub seen_revision: u64,
    /// Set when a change of channel took a release of the same revision with other bytes (an error of the author), for `doctor`.
    pub switch_note: Option<String>,
    /// Reserved: a version the person holds back.
    pub hold: Option<String>,
    /// Reserved: the confirmation of the domain.
    pub confirmed: Option<String>,
    extra: Vec<String>,
}

pub const SOURCE_AUTHOR: &str = "author";

impl State {
    pub fn parse(text: &str) -> State {
        let mut s = State { source: SOURCE_AUTHOR.to_string(), ..State::default() };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                s.extra.push(line.to_string());
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "root" if !v.is_empty() => s.roots.push(v.to_string()),
                "recovery" if !v.is_empty() => s.recovery = Some(v.to_string()),
                "source" if !v.is_empty() => s.source = v.to_string(),
                "floor" => s.floor = v.parse().unwrap_or(0),
                "seen_revision" => s.seen_revision = v.parse().unwrap_or(0),
                "switch_note" if !v.is_empty() => s.switch_note = Some(v.to_string()),
                "hold" if !v.is_empty() => s.hold = Some(v.to_string()),
                "confirmed" if !v.is_empty() => s.confirmed = Some(v.to_string()),
                "root" | "recovery" | "source" | "switch_note" | "hold" | "confirmed" => {}
                _ => s.extra.push(line.to_string()),
            }
        }
        s
    }

    pub fn render(&self) -> String {
        let mut out = String::from("# Kept by panpak for this app. Do not edit.\n");
        for r in &self.roots {
            out.push_str(&format!("root={r}\n"));
        }
        if let Some(r) = &self.recovery {
            out.push_str(&format!("recovery={r}\n"));
        }
        out.push_str(&format!("source={}\n", self.source));
        out.push_str(&format!("floor={}\nseen_revision={}\n", self.floor, self.seen_revision));
        for (k, v) in [("switch_note", &self.switch_note), ("hold", &self.hold), ("confirmed", &self.confirmed)] {
            if let Some(v) = v {
                out.push_str(&format!("{k}={v}\n"));
            }
        }
        for e in &self.extra {
            out.push_str(e);
            out.push('\n');
        }
        out
    }

    /// The pin: the first root and the commitment, when both are there.
    pub fn pin(&self) -> Option<Pin> {
        Some(Pin { root: self.roots.first()?.clone(), recovery: self.recovery.clone()? })
    }
}

/// The state of an app, if it has a file. A file that cannot be read is no state (the app then has no pin and is not started).
pub fn read(appdir: &Path) -> Option<State> {
    std::fs::read_to_string(appdir.join(FILE)).ok().map(|t| State::parse(&t))
}

pub fn write(appdir: &Path, state: &State) -> Result<()> {
    crate::durable::write_file(&appdir.join(FILE), state.render().as_bytes(), 0o644)
}

/// Read the state (an empty one when there is none), let `f` change it, write it back.
pub fn modify(appdir: &Path, f: impl FnOnce(&mut State)) -> Result<()> {
    let mut s = read(appdir).unwrap_or_else(|| State { source: SOURCE_AUTHOR.to_string(), ..State::default() });
    f(&mut s);
    write(appdir, &s)
}

// ---------------------------------------------------------------------------------------------------------------------------------------------
// The phrase of the recovery key

/// The English word list of BIP-39 (2048 words, the first four letters of each are unique), the public list that hardware wallets and password managers
/// also use: a phrase can be checked by other tools, and a word can be looked up in any copy of the list. MIT licensed (see NOTICE).
const WORDS: &str = include_str!("bip39-english.txt");

fn word(i: usize) -> &'static str {
    WORDS.lines().nth(i).expect("the list has 2048 words")
}

/// The check byte of BIP-39 for 256 bits: the first byte of the SHA-256 of the key.
fn check_byte(secret: &[u8; 32]) -> u8 {
    use sha2::Digest;
    sha2::Sha256::digest(secret)[0]
}

/// 24 words: the 256 bits of the key and 8 bits of check (BIP-39), 11 bits a word.
pub fn phrase_of(secret: &[u8; 32]) -> Vec<String> {
    let mut bits: Vec<bool> = Vec::with_capacity(264);
    for b in secret.iter().chain(std::iter::once(&check_byte(secret))) {
        for i in (0..8).rev() {
            bits.push(b >> i & 1 == 1);
        }
    }
    bits.chunks(11).map(|c| word(c.iter().fold(0usize, |a, &b| a << 1 | b as usize)).to_string()).collect()
}

/// The key of a phrase; the check catches a word that was written wrong.
pub fn secret_of(phrase: &str) -> Result<[u8; 32]> {
    let words: Vec<String> = phrase.split_whitespace().map(|w| w.to_ascii_lowercase()).collect();
    ensure!(words.len() == 24, "a recovery phrase has 24 words, this one has {}", words.len());
    let index: std::collections::HashMap<&str, usize> = WORDS.lines().enumerate().map(|(i, w)| (w, i)).collect();
    let mut bits: Vec<bool> = Vec::with_capacity(264);
    for w in &words {
        let i = *index.get(w.as_str()).with_context(|| format!("{w:?} is not a word of the recovery phrase"))?;
        for k in (0..11).rev() {
            bits.push(i >> k & 1 == 1);
        }
    }
    let bytes: Vec<u8> = bits.chunks(8).map(|c| c.iter().fold(0u8, |a, &b| a << 1 | b as u8)).collect();
    let mut secret = [0u8; 32];
    secret.copy_from_slice(&bytes[..32]);
    ensure!(bytes[32] == check_byte(&secret), "the recovery phrase does not check: a word is wrong or out of place");
    Ok(secret)
}

// ---------------------------------------------------------------------------------------------------------------------------------------------
// The chain beside the key

/// The successions an author carries, kept in a file beside the signing key (`<key>.chain`): `pack`, `build` and `convert` put them in every package, so that a client that
/// stayed on an old key still reaches the current one. A succession is signed for one app, so the file keeps them by app id. Missing or unreadable means no chain.
#[derive(serde::Serialize, serde::Deserialize)]
struct ChainEntry {
    app: String,
    by: String,
    from: String,
    to: String,
    recovery: String,
    sig: String,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct ChainFile {
    #[serde(default)]
    chain: Vec<ChainEntry>,
}

fn read_chain_file(path: &Path) -> ChainFile {
    std::fs::read_to_string(path).ok().and_then(|t| toml::from_str::<ChainFile>(&t).ok()).unwrap_or_default()
}

/// The steps kept for `app`, oldest first.
pub fn read_chain(path: &Path, app: &str) -> Vec<Step> {
    read_chain_file(path).chain.into_iter().filter(|e| e.app == app).map(|e| Step { by: e.by, from: e.from, to: e.to, recovery: e.recovery, sig: e.sig }).collect()
}

/// Keep `chain` for `app`; the steps of the other apps in the file stay as they are.
pub fn write_chain(path: &Path, app: &str, chain: &[Step]) -> Result<()> {
    let mut file = read_chain_file(path);
    file.chain.retain(|e| e.app != app);
    file.chain.extend(chain.iter().map(|s| ChainEntry { app: app.to_string(), by: s.by.clone(), from: s.from.clone(), to: s.to.clone(), recovery: s.recovery.clone(), sig: s.sig.clone() }));
    let text = format!("# The successions of this signing key, by app; panpak puts them in every package. A public file.\n{}", toml::to_string(&file)?);
    crate::durable::write_file(path, text.as_bytes(), 0o644)
}

/// A step, signed.
pub fn sign_step(signer: &ed25519_dalek::SigningKey, app_id: &str, by_recovery: bool, to: &VerifyingKey, new_recovery: &[u8; 32]) -> ed25519_dalek::Signature {
    use ed25519_dalek::Signer;
    signer.sign(&succession_message(app_id, by_recovery, &signer.verifying_key(), to, new_recovery))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }
    fn pubkey(sk: &SigningKey) -> String {
        format!("ed25519:{}", crate::hex(sk.verifying_key().as_bytes()))
    }
    fn step(app: &str, by_recovery: bool, signer: &SigningKey, to: &SigningKey, new_rec: &str) -> Step {
        let msg = succession_message(app, by_recovery, &signer.verifying_key(), &to.verifying_key(), &parse_commitment(new_rec).unwrap());
        Step { by: if by_recovery { "recovery" } else { "root" }.into(), from: pubkey(signer), to: pubkey(to), recovery: new_rec.into(), sig: crate::hex(&signer.sign(&msg).to_bytes()) }
    }
    fn trust(root: &SigningKey, rec: &str, chain: Vec<Step>) -> Trust {
        Trust { root: vec![pubkey(root)], threshold: 1, recovery: rec.into(), domain: None, chain }
    }

    #[test]
    fn the_keys_of_trust_are_the_ones_the_critical_list_knows() {
        assert_eq!(crate::TRUST_PATHS, TRUST_KEYS);
    }

    #[test]
    fn a_domain_is_the_beginning_of_the_id_read_backwards() {
        assert!(domain_fits_id("example.org", "org.example.app"));
        assert!(domain_fits_id("example.org", "org.example"));
        assert!(domain_fits_id("app.example.org", "org.example.app.Editor"));
        // One label is not enough, and the order matters.
        assert!(!domain_fits_id("org", "org.example.app"));
        assert!(!domain_fits_id("org.example", "org.example.app"));
        assert!(!domain_fits_id("example.com", "org.example.app"));
        assert!(!domain_fits_id("example.org", "org.exampler.app"));
        assert!(!domain_fits_id("Example.org", "org.example.app"));
        assert!(!domain_fits_id("exa mple.org", "org.exa mple"));
    }

    #[test]
    fn the_same_keys_need_no_chain_and_other_keys_need_one_that_starts_at_the_pin() {
        let (a, b, c) = (key(1), key(2), key(3));
        let rec_sk = key(9);
        let rec = commitment_of(&rec_sk.verifying_key());
        let pin = Pin { root: pubkey(&a), recovery: rec.clone() };
        assert_eq!(follow("org.x.app", &pin, &trust(&a, &rec, vec![])).unwrap(), 0);
        // Another root with no chain: refused.
        assert!(follow("org.x.app", &pin, &trust(&b, &rec, vec![])).is_err());
        // a to b by the root: followed, one step. A second step b to c is followed from a pin at b, and skipped as history from a pin at a... no: from a it is two steps.
        let s1 = step("org.x.app", false, &a, &b, &rec);
        let s2 = step("org.x.app", false, &b, &c, &rec);
        assert_eq!(follow("org.x.app", &pin, &trust(&b, &rec, vec![s1.clone()])).unwrap(), 1);
        assert_eq!(follow("org.x.app", &pin, &trust(&c, &rec, vec![s1.clone(), s2.clone()])).unwrap(), 2);
        let at_b = Pin { root: pubkey(&b), recovery: rec.clone() };
        assert_eq!(follow("org.x.app", &at_b, &trust(&c, &rec, vec![s1.clone(), s2.clone()])).unwrap(), 1, "the step before the pin is history");
        // A chain that does not end at the key of the package, a step of another app, a chain that starts elsewhere.
        assert!(follow("org.x.app", &pin, &trust(&c, &rec, vec![s1.clone()])).is_err());
        assert!(follow("org.y.app", &pin, &trust(&b, &rec, vec![s1.clone()])).is_err());
        assert!(follow("org.x.app", &pin, &trust(&c, &rec, vec![s2.clone()])).is_err());
    }

    #[test]
    fn the_root_cannot_change_the_recovery_commitment_and_the_recovery_key_can() {
        let (a, b, c) = (key(1), key(2), key(3));
        let (rec_sk, new_rec_sk) = (key(9), key(10));
        let rec = commitment_of(&rec_sk.verifying_key());
        let new_rec = commitment_of(&new_rec_sk.verifying_key());
        let pin = Pin { root: pubkey(&a), recovery: rec.clone() };
        // A thief with the root makes a succession to his own key and a commitment of his own: refused.
        let theft = step("org.x.app", false, &a, &b, &new_rec);
        assert!(follow("org.x.app", &pin, &trust(&b, &new_rec, vec![theft])).is_err());
        // The person reveals the recovery key: a new root and a new commitment. Valid from the pin, and also from a pin that the thief moved to b.
        let fix = step("org.x.app", true, &rec_sk, &c, &new_rec);
        assert_eq!(follow("org.x.app", &pin, &trust(&c, &new_rec, vec![fix.clone()])).unwrap(), 1);
        let moved = Pin { root: pubkey(&b), recovery: rec.clone() };
        assert_eq!(follow("org.x.app", &moved, &trust(&c, &new_rec, vec![fix])).unwrap(), 1, "the recovery succession wins over the root that was stolen");
        // A recovery step signed by a key that is not the committed one.
        let wrong = step("org.x.app", true, &key(11), &c, &new_rec);
        assert!(follow("org.x.app", &pin, &trust(&c, &new_rec, vec![wrong])).is_err());
    }

    #[test]
    fn a_tampered_signature_and_the_old_bare_form_are_not_a_succession() {
        let (a, b) = (key(1), key(2));
        let rec = commitment_of(&key(9).verifying_key());
        let pin = Pin { root: pubkey(&a), recovery: rec.clone() };
        let mut s = step("org.x.app", false, &a, &b, &rec);
        let first = if &s.sig[0..2] == "00" { "ff" } else { "00" };
        s.sig.replace_range(0..2, first);
        assert!(follow("org.x.app", &pin, &trust(&b, &rec, vec![s])).is_err());
        // The first version of the format signed the 32 bytes of the new key and nothing else.
        let mut bare = step("org.x.app", false, &a, &b, &rec);
        bare.sig = crate::hex(&a.sign(b.verifying_key().as_bytes()).to_bytes());
        assert!(follow("org.x.app", &pin, &trust(&b, &rec, vec![bare])).is_err());
        // A step of the recovery kind signed as if it were the root's, and the other way round: the kind is in the message.
        let mut wrong = step("org.x.app", false, &a, &b, &rec);
        wrong.by = "recovery".into();
        assert!(follow("org.x.app", &pin, &trust(&b, &rec, vec![wrong])).is_err());
    }

    #[test]
    fn the_phrase_is_bip39() {
        // The published test vectors of BIP-39 for 256 bits, and the list itself.
        let zeros = phrase_of(&[0u8; 32]).join(" ");
        assert_eq!(zeros, format!("{}art", "abandon ".repeat(23)));
        let ones = phrase_of(&[0xff; 32]).join(" ");
        assert_eq!(ones, format!("{}vote", "zoo ".repeat(23)));
        assert_eq!(secret_of(&zeros).unwrap(), [0u8; 32]);
        assert_eq!(WORDS.lines().count(), 2048);
        use sha2::Digest;
        assert_eq!(crate::hex(&sha2::Sha256::digest(WORDS.as_bytes())), "2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda");
    }

    #[test]
    fn the_chain_file_keeps_each_app_apart() {
        let d = std::env::temp_dir().join(format!("panpak-chain-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("k.chain");
        let _ = std::fs::remove_file(&f);
        assert!(read_chain(&f, "org.a.app").is_empty());
        let st = Step { by: "root".into(), from: "ed25519:aa".into(), to: "ed25519:bb".into(), recovery: "blake3:cc".into(), sig: "dd".into() };
        write_chain(&f, "org.a.app", &[st.clone(), st.clone()]).unwrap();
        write_chain(&f, "org.b.app", &[st]).unwrap();
        assert_eq!(read_chain(&f, "org.a.app").len(), 2);
        assert_eq!(read_chain(&f, "org.b.app").len(), 1);
        write_chain(&f, "org.a.app", &[]).unwrap();
        assert!(read_chain(&f, "org.a.app").is_empty());
        assert_eq!(read_chain(&f, "org.b.app")[0].to, "ed25519:bb");
        std::fs::remove_file(&f).unwrap();
        std::fs::remove_dir(&d).unwrap();
    }

    #[test]
    fn the_phrase_gives_the_key_back_and_a_wrong_word_is_caught() {
        let secret = [0x5au8; 32];
        let words = phrase_of(&secret);
        assert_eq!(words.len(), 24);
        assert!(words.iter().all(|w| w.is_ascii()));
        assert_eq!(secret_of(&words.join(" ")).unwrap(), secret);
        assert_eq!(secret_of(&words.join("\n").to_uppercase()).unwrap(), secret);
        let mut other = words.clone();
        other.swap(3, 4);
        assert!(secret_of(&other.join(" ")).is_err());
        assert!(secret_of(&words[..23].join(" ")).is_err());
        let mut bad = words.clone();
        bad[0] = "xxxx".into();
        assert!(secret_of(&bad.join(" ")).is_err());
        // 2048 distinct words.
        let all: std::collections::HashSet<&str> = (0..2048).map(word).collect();
        assert_eq!(all.len(), 2048);
    }

    #[test]
    fn the_state_round_trips_and_keeps_what_it_does_not_know() {
        let text = "# x\nroot=ed25519:aa\nroot=ed25519:bb\nrecovery=blake3:cc\nfloor=4\nseen_revision=9\nfuture=1\nswitch_note=rev 3\n";
        let s = State::parse(text);
        assert_eq!(s.roots.len(), 2);
        assert_eq!((s.floor, s.seen_revision, s.source.as_str()), (4, 9, "author"));
        let again = State::parse(&s.render());
        assert_eq!(again, s);
        assert!(s.render().contains("future=1") && s.render().contains("switch_note=rev 3"));
        assert!(s.pin().is_some());
        assert!(State::parse("floor=1\n").pin().is_none());
    }
}
