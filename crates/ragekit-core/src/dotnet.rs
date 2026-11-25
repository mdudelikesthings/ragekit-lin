//! .NET Framework 4.8 in a game's Proton prefix, which Script Hook V .NET
//! needs (Wine's built-in Mono isn't enough and the game crashes on load).
//!
//! The prefix is found from the game folder (`steamapps/compatdata/<appid>/pfx`
//! in the same Steam library), the install state is read from the prefix's
//! registry, and the install itself is done by protontricks when available,
//! otherwise by winetricks running with the game's own Proton.

use std::fs;
use std::path::{Path, PathBuf};

/// A game's Proton prefix.
#[derive(Debug, Clone, PartialEq)]
pub struct Prefix {
    pub app_id: u32,
    /// `.../compatdata/<appid>/pfx`
    pub pfx: PathBuf,
    /// The Proton install the prefix was last run with, if known.
    pub proton: Option<PathBuf>,
}

/// .NET Framework 4.8's lowest "Release" value.
const NET48_RELEASE: u32 = 528_040;

/// Find the Proton prefix of the game in `game_dir`, trying each app id.
pub fn find_prefix(game_dir: &Path, app_ids: &[u32], home: &Path) -> Option<Prefix> {
    // Libraries to look in: the one holding the game first, then all others.
    let mut libs: Vec<PathBuf> = Vec::new();
    let mut d = game_dir.to_path_buf();
    while let Some(p) = d.parent() {
        if d.file_name().is_some_and(|n| n == "steamapps") {
            libs.push(p.to_path_buf());
            break;
        }
        d = p.to_path_buf();
    }
    for root in crate::games::steam_roots(home) {
        if let Ok(t) = fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
            libs.extend(crate::games::vdf_paths(&t));
        }
        libs.push(root);
    }
    for lib in &libs {
        for &id in app_ids {
            let data = lib.join("steamapps/compatdata").join(id.to_string());
            let pfx = data.join("pfx");
            if pfx.join("system.reg").is_file() {
                return Some(Prefix { app_id: id, proton: proton_of(&data, &libs), pfx });
            }
        }
    }
    None
}

/// The Proton folder recorded in `compatdata/<id>/config_info`, else the
/// newest Proton in any library.
fn proton_of(data: &Path, libs: &[PathBuf]) -> Option<PathBuf> {
    if let Ok(t) = fs::read_to_string(data.join("config_info")) {
        for line in t.lines() {
            for marker in ["/files/", "/dist/"] {
                if let Some(i) = line.find(marker) {
                    let dir = PathBuf::from(&line[..i]);
                    if wine_in(&dir).is_some() {
                        return Some(dir);
                    }
                }
            }
        }
    }
    let mut found: Vec<PathBuf> = libs
        .iter()
        .flat_map(|l| fs::read_dir(l.join("steamapps/common")).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("Proton")) && wine_in(p).is_some())
        .collect();
    found.sort();
    found.pop()
}

/// Proton's wine binary: `files/bin/wine` (Proton 5+) or `dist/bin/wine`.
pub fn wine_in(proton: &Path) -> Option<PathBuf> {
    ["files/bin/wine", "dist/bin/wine"].iter().map(|r| proton.join(r)).find(|p| p.is_file())
}

/// Read a DWORD value from a Wine `.reg` file.
pub fn reg_dword(reg: &str, key: &str, value: &str) -> Option<u32> {
    let header = format!("[{}]", key.replace('\\', "\\\\"));
    let mut in_key = false;
    for line in reg.lines() {
        if line.starts_with('[') {
            in_key = line.to_ascii_lowercase().starts_with(&header.to_ascii_lowercase());
            continue;
        }
        if in_key {
            let want = format!("\"{value}\"=dword:");
            if line.to_ascii_lowercase().starts_with(&want.to_ascii_lowercase()) {
                return u32::from_str_radix(line[want.len()..].trim(), 16).ok();
            }
        }
    }
    None
}

/// Is .NET Framework 4.8 or newer installed in the prefix?
pub fn net48_installed(pfx: &Path) -> bool {
    let Ok(reg) = fs::read_to_string(pfx.join("system.reg")) else { return false };
    reg_dword(&reg, r"Software\Microsoft\NET Framework Setup\NDP\v4\Full", "Release").is_some_and(|r| r >= NET48_RELEASE)
}

/// How the install will be run.
#[derive(Debug, Clone, PartialEq)]
pub enum Method {
    /// `protontricks <appid> -q dotnet48` (native or Flatpak).
    Protontricks { program: String, args: Vec<String> },
    /// winetricks with the game's Proton wine.
    Winetricks { program: String, args: Vec<String>, env: Vec<(String, String)> },
}

impl Method {
    pub fn describe(&self) -> String {
        match self {
            Method::Protontricks { program, args } | Method::Winetricks { program, args, .. } => format!("{program} {}", args.join(" ")),
        }
    }
}

fn on_path(cmd: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(cmd)).find(|p| p.is_file()))
}

fn flatpak_has(app: &str) -> bool {
    on_path("flatpak").is_some()
        && std::process::Command::new("flatpak").args(["info", app]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Pick a way to install .NET 4.8. `winetricks_script` is used when no
/// winetricks is installed (ragekit downloads it to its cache).
pub fn choose_method(prefix: &Prefix, winetricks_script: Option<&Path>) -> Result<Method, String> {
    let verb = ["-q", "--force", "dotnet48"].map(String::from);
    if let Some(p) = on_path("protontricks") {
        let mut args = vec![prefix.app_id.to_string()];
        args.extend(verb.iter().cloned());
        return Ok(Method::Protontricks { program: p.to_string_lossy().into_owned(), args });
    }
    if flatpak_has("com.github.Matoking.protontricks") {
        let mut args = vec!["run".into(), "com.github.Matoking.protontricks".into(), prefix.app_id.to_string()];
        args.extend(verb.iter().cloned());
        return Ok(Method::Protontricks { program: "flatpak".into(), args });
    }
    let proton = prefix.proton.as_ref().ok_or("couldn't find the Proton version this game uses; install protontricks instead")?;
    let wine = wine_in(proton).ok_or("Proton's wine wasn't found")?;
    let bin = wine.parent().unwrap();
    if on_path("cabextract").is_none() {
        return Err("winetricks needs cabextract to install .NET; install it (or protontricks) with your package manager first".into());
    }
    let (program, mut args) = match on_path("winetricks") {
        Some(w) => (w.to_string_lossy().into_owned(), Vec::new()),
        None => {
            let s = winetricks_script.ok_or("no winetricks or protontricks found")?;
            ("bash".to_string(), vec![s.to_string_lossy().into_owned()])
        }
    };
    args.extend(verb.iter().cloned());
    let wine64 = bin.join("wine64");
    let env = vec![
        ("WINEPREFIX".into(), prefix.pfx.to_string_lossy().into_owned()),
        ("WINE".into(), wine.to_string_lossy().into_owned()),
        ("WINE64".into(), if wine64.is_file() { wine64 } else { wine.clone() }.to_string_lossy().into_owned()),
        ("WINESERVER".into(), bin.join("wineserver").to_string_lossy().into_owned()),
        ("WINEDEBUG".into(), "-all".into()),
    ];
    Ok(Method::Winetricks { program, args, env })
}

/// Run the install, passing each output line to `log`. Blocks until done
/// (often 5–15 minutes: the installers are downloaded from Microsoft).
pub fn run(method: &Method, log: &mut dyn FnMut(&str)) -> Result<(), String> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let (program, args, env): (&str, &[String], &[(String, String)]) = match method {
        Method::Protontricks { program, args } => (program, args, &[]),
        Method::Winetricks { program, args, env } => (program, args, env),
    };
    let mut cmd = Command::new(program);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|e| format!("couldn't start {program}: {e}"))?;
    let err = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let tx2 = tx.clone();
    let t = std::thread::spawn(move || {
        for l in BufReader::new(err).lines().map_while(Result::ok) {
            let _ = tx2.send(l);
        }
    });
    let out = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for l in BufReader::new(out).lines().map_while(Result::ok) {
            let _ = tx.send(l);
        }
    });
    for line in rx {
        log(&line);
    }
    let _ = t.join();
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("{program} exited with {status}")) }
}

/// Where ragekit keeps a downloaded winetricks script.
pub fn winetricks_cache(home: &Path) -> PathBuf {
    home.join(".cache/ragekit/winetricks")
}

pub const WINETRICKS_URL: &str = "https://raw.githubusercontent.com/Winetricks/winetricks/master/src/winetricks";

/// The whole job for the game in `game_dir`: find its prefix, pick a
/// method (downloading winetricks if needed), install, and check.
pub fn install_net48(game_dir: &Path, app_ids: &[u32], home: &Path, log: &mut dyn FnMut(&str)) -> Result<String, String> {
    let prefix = find_prefix(game_dir, app_ids, home)
        .ok_or("no Proton prefix found for this game; start it once from Steam (with Proton) so Steam creates one, then try again")?;
    if net48_installed(&prefix.pfx) {
        return Ok(format!(".NET Framework 4.8 is already installed in {}", prefix.pfx.display()));
    }
    let mut script = None;
    let needs_script = on_path("protontricks").is_none() && on_path("winetricks").is_none() && !flatpak_has("com.github.Matoking.protontricks");
    if needs_script {
        let path = winetricks_cache(home);
        // Make sure the rest is in place before downloading anything.
        choose_method(&prefix, Some(&path))?;
        if !path.is_file() {
            log("Downloading winetricks…");
            use crate::tools::Fetch;
            let data = crate::tools::Http.get(WINETRICKS_URL, None).map_err(|e| e.to_string())?;
            fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::write(&path, data).map_err(|e| e.to_string())?;
        }
        script = Some(path);
    }
    let method = choose_method(&prefix, script.as_deref())?;
    log(&format!("Running: {}", method.describe()));
    log("This downloads the .NET installers from Microsoft and usually takes 5–15 minutes. Keep the game closed.");
    run(&method, log)?;
    if net48_installed(&prefix.pfx) {
        Ok(format!(".NET Framework 4.8 installed in {}", prefix.pfx.display()))
    } else {
        Err(format!("the installer finished, but .NET 4.8 isn't registered in {}; see the log above", prefix.pfx.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_and_prefix() {
        let reg = "WINE REGISTRY Version 2\n\n[Software\\\\Microsoft\\\\NET Framework Setup\\\\NDP\\\\v4\\\\Full] 1700000000\n#time=1\n\"Install\"=dword:00000001\n\"Release\"=dword:00080ff4\n\"Version\"=\"4.8.04084\"\n\n[Software\\\\Other]\n\"Release\"=dword:00000001\n";
        assert_eq!(reg_dword(reg, r"Software\Microsoft\NET Framework Setup\NDP\v4\Full", "Release"), Some(0x80ff4));
        assert_eq!(reg_dword(reg, r"Software\Microsoft\NET Framework Setup\NDP\v4\Full", "Missing"), None);

        let home = std::env::temp_dir().join(format!("ragekit-pfx-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let lib = home.join(".local/share/Steam");
        let game = lib.join("steamapps/common/Grand Theft Auto V");
        let data = lib.join("steamapps/compatdata/271590");
        let proton = lib.join("steamapps/common/Proton 9.0 (Beta)");
        fs::create_dir_all(&game).unwrap();
        fs::create_dir_all(data.join("pfx")).unwrap();
        fs::create_dir_all(proton.join("files/bin")).unwrap();
        fs::write(proton.join("files/bin/wine"), b"").unwrap();
        fs::write(data.join("config_info"), format!("9.0-300\n{}/files/share/fonts/\n", proton.display())).unwrap();
        fs::write(data.join("pfx/system.reg"), "WINE REGISTRY Version 2\n").unwrap();
        let p = find_prefix(&game, &[271590], &home).unwrap();
        assert_eq!(p.app_id, 271590);
        assert_eq!(p.proton.as_deref(), Some(proton.as_path()));
        assert!(!net48_installed(&p.pfx));
        fs::write(data.join("pfx/system.reg"), reg).unwrap();
        assert!(net48_installed(&p.pfx));
        assert!(find_prefix(&game, &[1], &home).is_none());
        fs::remove_dir_all(&home).unwrap();
    }
}
