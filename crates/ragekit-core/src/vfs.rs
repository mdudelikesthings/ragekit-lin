//! Opening any supported archive, detecting games, and searching a game
//! folder across all archives (including nested ones).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::archive::Archive;
use crate::blob::Blob;
use crate::gta4;
use crate::keys::Keys;
use crate::tree::{Format, RowKind, Tree};
use crate::{Error, Result};

/// File extensions that ragekit opens as archives.
pub fn is_archive_name(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.ends_with(".rpf") || l.ends_with(".img")
}

/// Detect the archive format of some bytes.
pub fn detect(data: &[u8], keys: &Keys) -> Option<Format> {
    if data.len() < 16 {
        return None;
    }
    match u32::from_le_bytes(data[0..4].try_into().unwrap()) {
        crate::archive::RPF7_MAGIC => Some(Format::Rpf7),
        gta4::RPF2_MAGIC => Some(Format::Rpf2),
        gta4::RPF3_MAGIC => Some(Format::Rpf3),
        gta4::RPF4_MAGIC => Some(Format::Rpf4),
        _ if crate::rdr::is_rpf6(data) => Some(Format::Rpf6),
        _ if gta4::is_img3(data, keys) => Some(Format::Img3),
        _ => None,
    }
}

/// Open an archive file of any supported format as an editable tree.
pub fn open(path: &Path, keys: Arc<Keys>) -> Result<Tree> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    open_blob(Blob::map_file(path)?, &name, keys)
}

/// Open archive bytes. `name` is the file name (GTA V NG keys depend on it).
pub fn open_blob(blob: Blob, name: &str, keys: Arc<Keys>) -> Result<Tree> {
    let fmt = detect(blob.bytes(), &keys);
    match fmt {
        Some(Format::Rpf7) => Tree::from_archive(Archive::parse(blob, name, &keys)?, keys),
        Some(Format::Rpf2) | Some(Format::Rpf3) | Some(Format::Rpf4) => gta4::parse_rpf(blob, keys),
        Some(Format::Rpf6) => crate::rdr::parse(blob, keys),
        Some(Format::Img3) => gta4::parse_img(blob, keys),
        None => {
            if name.to_ascii_lowercase().ends_with(".img") && keys.iv_aes.is_none() {
                Err(Error::MissingKey("GTA IV AES key (this IMG archive is encrypted)"))
            } else {
                Err(Error::Invalid(format!("{name}: not a supported archive")))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Game {
    GtaV,
    GtaIV,
    Eflc,
    MaxPayne3,
}

impl Game {
    pub fn label(self) -> &'static str {
        match self {
            Game::GtaV => "GTA V",
            Game::GtaIV => "GTA IV",
            Game::Eflc => "Episodes from Liberty City",
            Game::MaxPayne3 => "Max Payne 3",
        }
    }
    pub fn format(self) -> Format {
        match self {
            Game::GtaV => Format::Rpf7,
            Game::MaxPayne3 => Format::Rpf4,
            _ => Format::Rpf2,
        }
    }
    /// Does OpenIV's .oiv package format cover this game?
    pub fn takes_oiv(self) -> bool {
        self != Game::MaxPayne3
    }
}

/// Which game a folder holds, and its executable.
pub fn detect_game(dir: &Path) -> Option<(Game, PathBuf)> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut found = None;
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_ascii_lowercase();
        let g = match n.as_str() {
            "gta5.exe" | "gta5_enhanced.exe" => Game::GtaV,
            "gtaiv.exe" => Game::GtaIV,
            "eflc.exe" => Game::Eflc,
            "maxpayne3.exe" => Game::MaxPayne3,
            _ => continue,
        };
        // Prefer the real game exe over launchers if several match.
        if found.is_none() || n == "gta5.exe" || n == "gta5_enhanced.exe" {
            found = Some((g, e.path()));
        }
    }
    found
}

/// Which key an encrypted archive needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFor {
    GtaIV,
    MaxPayne3,
    Rdr,
}

impl KeyFor {
    pub fn label(self) -> &'static str {
        match self {
            KeyFor::GtaIV => "GTA IV",
            KeyFor::MaxPayne3 => "Max Payne 3",
            KeyFor::Rdr => "Red Dead Redemption",
        }
    }
    pub fn slot(self, keys: &mut Keys) -> &mut Option<[u8; 32]> {
        match self {
            KeyFor::GtaIV => &mut keys.iv_aes,
            KeyFor::MaxPayne3 => &mut keys.mp3_aes,
            KeyFor::Rdr => &mut keys.rdr_aes,
        }
    }
}

pub type KeyProbe = Box<dyn Fn(&[u8; 32]) -> bool + Sync + Send>;

/// If `head` (the first few KiB of an archive) is an encrypted archive whose
/// key can be found by probing, say which key and return the probe.
pub fn key_probe(head: &[u8]) -> Option<(KeyFor, KeyProbe)> {
    if let Some(p) = crate::rdr::key_probe(head) {
        return Some((KeyFor::Rdr, Box::new(p)));
    }
    let p = gta4::rpf_key_probe(head)?;
    let which = if u32::from_le_bytes(head[0..4].try_into().unwrap()) == gta4::RPF4_MAGIC { KeyFor::MaxPayne3 } else { KeyFor::GtaIV };
    Some((which, Box::new(p)))
}

/// Find an encrypted archive under `dir` (a few levels deep) to probe keys
/// against, preferring the given kind.
pub fn find_probe_archive(dir: &Path, want: Option<KeyFor>) -> Option<(PathBuf, KeyFor, KeyProbe)> {
    use std::io::Read;
    let mut stack = vec![(dir.to_path_buf(), 0)];
    let mut fallback = None;
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut ents: Vec<_> = rd.flatten().collect();
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let p = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                if depth < 3 {
                    stack.push((p, depth + 1));
                }
                continue;
            }
            if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("rpf")) {
                continue;
            }
            let mut head = vec![0u8; 0x810];
            let Ok(mut f) = std::fs::File::open(&p) else { continue };
            if f.read_exact(&mut head).is_err() {
                continue;
            }
            if let Some((k, probe)) = key_probe(&head) {
                if want.is_none_or(|w| w == k) {
                    return Some((p, k, probe));
                }
                fallback.get_or_insert((p, k, probe));
            }
        }
    }
    if want.is_some() { None } else { fallback }
}

/// Case-insensitive wildcard match (`*`, `?`); a pattern without wildcards
/// matches substrings.
pub fn wildcard(pat: &str, s: &str) -> bool {
    let pat = pat.to_ascii_lowercase();
    let s = s.to_ascii_lowercase();
    if !pat.contains('*') && !pat.contains('?') {
        return s.contains(&pat);
    }
    let (p, t): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// A search hit: the archive file on disk and the path inside it
/// (empty when the hit is the archive or a loose file itself).
#[derive(Debug, Clone)]
pub struct Hit {
    pub file: PathBuf,
    pub inner: String,
}

/// Search names under `root` (a folder or one archive), looking inside all
/// archives and nested archives. Errors for individual archives go to
/// `on_error` and the search carries on.
pub fn search(
    root: &Path,
    pattern: &str,
    keys: Arc<Keys>,
    cancel: &AtomicBool,
    on_hit: &mut dyn FnMut(Hit),
    on_error: &mut dyn FnMut(String),
) {
    let mut files = Vec::new();
    if root.is_dir() {
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            let mut ents: Vec<_> = rd.flatten().collect();
            ents.sort_by_key(|e| e.file_name());
            for e in ents {
                let p = e.path();
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    stack.push(p);
                } else {
                    files.push(p);
                }
            }
        }
    } else {
        files.push(root.to_path_buf());
    }
    for f in files {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if wildcard(pattern, &name) {
            on_hit(Hit { file: f.clone(), inner: String::new() });
        }
        if !is_archive_name(&name) {
            continue;
        }
        match open(&f, keys.clone()) {
            Ok(tree) => search_tree(&tree, "", &f, pattern, cancel, on_hit, on_error),
            Err(e) => on_error(format!("{}: {e}", f.display())),
        }
    }
}

fn search_tree(
    tree: &Tree,
    dir: &str,
    file: &Path,
    pattern: &str,
    cancel: &AtomicBool,
    on_hit: &mut dyn FnMut(Hit),
    on_error: &mut dyn FnMut(String),
) {
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    let rows = match tree.list(dir) {
        Ok(r) => r,
        Err(e) => {
            on_error(format!("{}/{dir}: {e}", file.display()));
            return;
        }
    };
    for r in rows {
        let p = if dir.is_empty() { r.name.clone() } else { format!("{dir}/{}", r.name) };
        if wildcard(pattern, &r.name) {
            on_hit(Hit { file: file.to_path_buf(), inner: p.clone() });
        }
        if r.kind != RowKind::File {
            search_tree(tree, &p, file, pattern, cancel, on_hit, on_error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::wildcard;

    #[test]
    fn wildcards() {
        assert!(wildcard("adder", "ADDER.yft"));
        assert!(wildcard("*.ytd", "adder.ytd"));
        assert!(!wildcard("*.ytd", "adder.yft"));
        assert!(wildcard("add?r*", "adder_hi.yft"));
    }
}
