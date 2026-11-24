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
        Some(Format::Rpf2) | Some(Format::Rpf3) => gta4::parse_rpf(blob, keys),
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
}

impl Game {
    pub fn label(self) -> &'static str {
        match self {
            Game::GtaV => "GTA V",
            Game::GtaIV => "GTA IV",
            Game::Eflc => "Episodes from Liberty City",
        }
    }
    pub fn format(self) -> Format {
        match self {
            Game::GtaV => Format::Rpf7,
            _ => Format::Rpf2,
        }
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
            _ => continue,
        };
        // Prefer the real game exe over launchers if several match.
        if found.is_none() || n == "gta5.exe" || n == "gta5_enhanced.exe" {
            found = Some((g, e.path()));
        }
    }
    found
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
