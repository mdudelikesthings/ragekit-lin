//! Operations shared by the GUI and the CLI.

use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use ragekit_core::{Keys, RowKind, Tree};

/// Where keys live: --keys, $RAGEKIT_KEYS, or ~/.config/ragekit/keys.
pub fn keys_dir(over: Option<&Path>) -> PathBuf {
    if let Some(k) = over {
        return k.to_path_buf();
    }
    if let Ok(k) = std::env::var("RAGEKIT_KEYS") {
        return PathBuf::from(k);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("ragekit").join("keys")
}

pub fn load_keys(dir: &Path) -> Result<Arc<Keys>> {
    Ok(Arc::new(Keys::load_dir(dir).with_context(|| format!("loading keys from {}", dir.display()))?))
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// Refuse names that would escape the output folder.
pub fn safe_name(name: &str) -> Result<&str> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') || name.contains('\0') {
        bail!("refusing unsafe entry name {name:?}");
    }
    Ok(name)
}

/// Export `path` (file, folder or nested archive) from `tree` to `dest`.
/// Returns (files written, failures).
#[derive(Clone, Copy, Default)]
pub struct ExtractOpts {
    /// Unpack nested archives into folders.
    pub expand: bool,
    /// Also write binary metadata as .xml and .gxt2 as .txt next to the file.
    pub convert: bool,
}

pub fn extract_opts(tree: &Tree, path: &str, dest: &Path, o: &ExtractOpts, progress: &mut dyn FnMut(&str)) -> Result<(usize, Vec<String>)> {
    let mut count = 0;
    let mut failed = Vec::new();
    extract_rec(tree, path, dest, o, progress, &mut count, &mut failed)?;
    Ok((count, failed))
}

fn extract_rec(
    tree: &Tree,
    path: &str,
    dest: &Path,
    o: &ExtractOpts,
    progress: &mut dyn FnMut(&str),
    count: &mut usize,
    failed: &mut Vec<String>,
) -> Result<()> {
    let kind = tree.kind(path)?;
    if kind == RowKind::Folder || (kind == RowKind::Archive && o.expand) {
        fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
        let rows = match tree.list(path) {
            Ok(r) => r,
            Err(e) if kind == RowKind::Archive => {
                failed.push(format!("{path}: {e} (saved as a file instead)"));
                let _ = fs::remove_dir(dest);
                return write_file(tree, path, dest, o, progress, count, failed);
            }
            Err(e) => return Err(e.into()),
        };
        for r in rows {
            let child = join(path, &r.name);
            extract_rec(tree, &child, &dest.join(safe_name(&r.name)?), o, progress, count, failed)?;
        }
        return Ok(());
    }
    write_file(tree, path, dest, o, progress, count, failed)
}

fn write_file(
    tree: &Tree,
    path: &str,
    dest: &Path,
    o: &ExtractOpts,
    progress: &mut dyn FnMut(&str),
    count: &mut usize,
    failed: &mut Vec<String>,
) -> Result<()> {
    progress(path);
    match tree.read(path) {
        Ok(data) => {
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            if o.convert {
                if let Ok((text, conv)) = ragekit_core::convert::to_text(&data) {
                    let mut p = dest.as_os_str().to_owned();
                    p.push(format!(".{}", conv.ext()));
                    let _ = fs::write(PathBuf::from(p), text);
                }
            }
            fs::write(dest, data).with_context(|| format!("writing {}", dest.display()))?;
            *count += 1;
        }
        Err(e) => failed.push(format!("{path}: {e}")),
    }
    Ok(())
}

/// Write a tree over `target` safely: temp file in the same folder, then
/// rename. With `backup`, the original is first copied to `<name>.bak`
/// (only if no backup exists yet, so the first original is preserved).
pub fn save(tree: &Tree, target: &Path, backup: bool) -> Result<(u64, Option<PathBuf>)> {
    let mut bak_made = None;
    if backup && target.exists() {
        let mut s = target.as_os_str().to_owned();
        s.push(".bak");
        let bak = PathBuf::from(s);
        if !bak.exists() {
            fs::copy(target, &bak).with_context(|| format!("backing up to {}", bak.display()))?;
            bak_made = Some(bak);
        }
    }
    let n = write_atomic(target, |w| Ok(tree.write(w)?))?;
    Ok((n, bak_made))
}

pub fn write_atomic(target: &Path, f: impl FnOnce(&mut BufWriter<fs::File>) -> Result<u64>) -> Result<u64> {
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
        Ok::<u64, anyhow::Error>(n)
    })();
    match res {
        Ok(n) => {
            fs::rename(&tmp, target).with_context(|| format!("replacing {}", target.display()))?;
            Ok(n)
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

pub fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// Find a running game process (GTA V, GTA IV or EFLC under Wine/Proton).
pub fn find_game_pid() -> Option<u32> {
    for e in fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        let cmd = fs::read(e.path().join("cmdline")).unwrap_or_default();
        let cmd = String::from_utf8_lossy(&cmd).to_ascii_lowercase();
        let exe = cmd.split('\0').next().unwrap_or("");
        if ["gta5.exe", "gta5_enhanced.exe", "gtaiv.exe", "eflc.exe", "maxpayne3.exe"].iter().any(|n| exe.ends_with(n)) {
            return Some(pid);
        }
    }
    None
}

/// Find the game executable given an exe path or a game folder.
pub fn resolve_exe(p: &Path) -> Result<PathBuf> {
    if p.is_file() {
        return Ok(p.to_path_buf());
    }
    if let Some((_, exe)) = ragekit_core::vfs::detect_game(p) {
        return Ok(exe);
    }
    bail!("no GTA5.exe / GTAIV.exe / EFLC.exe / MaxPayne3.exe found in {}", p.display())
}

/// Text-like file extensions shown in the text viewer.
pub fn is_text_name(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "xml" | "meta" | "txt" | "dat" | "ide" | "ipl" | "cfg" | "ini" | "log" | "lua" | "json" | "csv" | "nametable" | "sc" | "gfx_info"
            | "zon" | "ped" | "rel" | "grp"
    )
}

/// What the file is, for the browser's type column.
pub fn describe(name: &str) -> &'static str {
    if let Some(d) = ragekit_core::resource::describe_extension(name) {
        return d;
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "wtd" => "Texture dictionary",
        "wdr" => "Drawable",
        "wdd" => "Drawable dictionary",
        "wft" => "Fragment (vehicle/breakable)",
        "wbd" | "wbn" => "Collision bounds",
        "wad" => "Animation dictionary",
        "wpl" => "Map placement",
        "wnv" => "Nav mesh",
        "ide" => "Item definitions",
        "ipl" => "Item placement",
        "img" => "Image archive",
        "ivaud" => "Audio",
        "sco" => "Compiled script",
        "gxt" => "Text table",
        "nod" => "Path nodes",
        "whm" => "Height map",
        "wpfl" => "Particle effects",
        "dds" => "DDS texture",
        "png" | "jpg" | "bmp" | "tga" => "Image",
        "exe" => "Program",
        "dll" | "asi" => "Library / plugin",
        "txt" | "log" | "ini" | "cfg" => "Text",
        _ => "",
    }
}

pub fn state_file() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("ragekit").join("last_game_folder"))
}
pub fn load_last_root() -> Option<PathBuf> {
    let p = PathBuf::from(fs::read_to_string(state_file()?).ok()?.trim());
    p.is_dir().then_some(p)
}
pub fn save_last_root(p: &Path) {
    if let Some(f) = state_file() {
        let _ = fs::create_dir_all(f.parent().unwrap());
        let _ = fs::write(f, p.display().to_string());
    }
}


/// The folder a running game was started from, found through the Windows
/// exe path on its command line (Wine/Proton) or its /proc exe link.
pub fn process_game_dir(pid: u32) -> Option<PathBuf> {
    if let Ok(cmd) = fs::read(format!("/proc/{pid}/cmdline")) {
        for arg in cmd.split(|&b| b == 0) {
            let a = String::from_utf8_lossy(arg).replace('\\', "/");
            if !a.to_ascii_lowercase().ends_with(".exe") {
                continue;
            }
            // Z:/home/... is the Unix root under Wine.
            let unix = match a.get(..2) {
                Some(d) if d.eq_ignore_ascii_case("z:") => PathBuf::from(&a[2..]),
                _ => PathBuf::from(&a),
            };
            if let Some(dir) = unix.parent().filter(|d| d.is_dir()) {
                return Some(dir.to_path_buf());
            }
            // Relative / drive-C paths: fall back to the working directory.
            if let Ok(cwd) = fs::read_link(format!("/proc/{pid}/cwd")) {
                return Some(cwd);
            }
        }
    }
    fs::read_link(format!("/proc/{pid}/exe")).ok().and_then(|p| p.parent().map(Path::to_path_buf))
}

/// Search a game executable for every key it can give: GTA V / GTA IV keys
/// by hash, and Max Payne 3 / RDR keys by testing candidates against an
/// encrypted archive in the same folder. Returns a one-line summary.
pub fn find_keys_in_exe(keys: &mut Keys, exe: &Path) -> Result<String> {
    use ragekit_core::vfs;
    let data = fs::read(exe).with_context(|| format!("reading {}", exe.display()))?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    let mut parts = Vec::new();
    let mp3 = vfs::detect_game(dir).is_some_and(|(g, _)| g == vfs::Game::MaxPayne3);
    if !mp3 {
        let r = keys.search_bytes(&data);
        parts.push(format!("GTA V AES: {}", if r.aes { "found" } else { "not found" }));
        parts.push(format!("GTA IV: {}", if r.iv_aes { "found" } else { "not found" }));
    }
    if let Some((_, which, probe)) = vfs::find_probe_archive(dir, None) {
        if which.slot(keys).is_none() {
            let found = Keys::find_by_probe(&data, &*probe);
            parts.push(format!("{}: {}", which.label(), if found.is_some() { "found" } else { "not in the file; try the running game" }));
            if found.is_some() {
                *which.slot(keys) = found;
            }
        }
    }
    Ok(parts.join("   "))
}

/// Scan a running game's memory: probe for the Max Payne 3 / RDR key when
/// the game folder holds such archives and that key is missing, otherwise
/// look for the GTA V keys.
pub fn find_keys_in_process(keys: &mut Keys, pid: u32) -> Result<String> {
    use ragekit_core::vfs;
    if let Some((_, which, probe)) = process_game_dir(pid).and_then(|d| vfs::find_probe_archive(&d, None)) {
        if which.slot(keys).is_none() {
            let found = Keys::find_by_probe_in_process(pid, &*probe)?;
            let msg = format!("{} key: {}", which.label(), if found.is_some() { "found" } else { "not found" });
            *which.slot(keys) = found;
            return Ok(msg);
        }
    }
    let r = keys.search_process(pid)?;
    Ok(format!("NG keys {}/101, NG tables {}/272", r.ng_keys_found, r.ng_tables_found))
}
