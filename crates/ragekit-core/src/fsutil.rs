//! Small filesystem helpers: atomic writes and case-insensitive paths
//! (game files keep their Windows casing on Linux; packages don't always
//! match it).

use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::Result;

/// Write `target` via a temp file in the same folder, then rename over it.
/// Readers that had the old file open (or memory-mapped) keep seeing it.
pub fn write_atomic(target: &Path, f: impl FnOnce(&mut dyn Write) -> Result<u64>) -> Result<u64> {
    let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.ragekit-tmp-{}",
        target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        std::process::id()
    ));
    let res = (|| {
        let mut w = BufWriter::new(fs::File::create(&tmp)?);
        let n = f(&mut w)?;
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok::<u64, crate::Error>(n)
    })();
    match res {
        Ok(n) => {
            fs::rename(&tmp, target)?;
            Ok(n)
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Resolve `rel` (with `/` or `\`) under `base`, matching existing path
/// components case-insensitively. Components that don't exist are kept as
/// given. Returns the path and whether it exists.
pub fn resolve_ci(base: &Path, rel: &str) -> (PathBuf, bool) {
    let mut cur = base.to_path_buf();
    let mut exists = true;
    for part in rel.split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".") {
        if exists {
            let direct = cur.join(part);
            if direct.exists() {
                cur = direct;
                continue;
            }
            if let Ok(rd) = fs::read_dir(&cur) {
                if let Some(e) = rd.flatten().find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part)) {
                    cur = e.path();
                    continue;
                }
            }
            exists = false;
        }
        cur.push(part);
    }
    let exists = exists && cur.exists();
    (cur, exists)
}

/// (size, mtime in ns) of a file, or None if absent.
pub fn stamp(p: &Path) -> Option<(u64, u64)> {
    let m = fs::metadata(p).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos() as u64;
    Some((m.len(), t))
}
