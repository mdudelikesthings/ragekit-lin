//! Finding game installs on Linux, and checking a GTA V install's mod setup.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::vfs::{detect_game, Game};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub game: Game,
    pub path: PathBuf,
    pub enhanced: bool,
    pub source: &'static str,
}

impl Install {
    pub fn label(&self) -> String {
        let ed = if self.game == Game::GtaV { if self.enhanced { " Enhanced" } else { " Legacy" } } else { "" };
        format!("{}{ed} ({})", self.game.label(), self.source)
    }
}

fn is_enhanced(dir: &Path) -> bool {
    dir.join("GTA5_Enhanced.exe").exists() || dir.join("gta5_enhanced.exe").exists()
}

/// Steam library folders listed in a `libraryfolders.vdf`.
pub fn vdf_paths(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        let mut parts = l.split('"').filter(|s| !s.trim().is_empty());
        if parts.next() == Some("path") {
            if let Some(p) = parts.next() {
                out.push(PathBuf::from(p.replace("\\\\", "/")));
            }
        }
    }
    out
}

fn steam_roots(home: &Path) -> Vec<PathBuf> {
    [
        ".steam/steam",
        ".steam/root",
        ".local/share/Steam",
        ".var/app/com.valvesoftware.Steam/.local/share/Steam",
        "snap/steam/common/.local/share/Steam",
    ]
    .iter()
    .map(|r| home.join(r))
    .filter(|p| p.join("steamapps").is_dir())
    .collect()
}

/// Folders a game might be in, given a parent folder (checks it and one level down).
fn probe(dir: &Path, source: &'static str, out: &mut Vec<Install>) {
    let mut check = |d: &Path| {
        if let Some((game, _)) = detect_game(d) {
            out.push(Install { game, enhanced: is_enhanced(d), path: d.to_path_buf(), source });
        }
    };
    check(dir);
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                check(&e.path());
            }
        }
    }
}

const GAME_DIRS: [&str; 5] = [
    "Grand Theft Auto V",
    "Grand Theft Auto V Enhanced",
    "Grand Theft Auto IV",
    "Grand Theft Auto IV/GTAIV",
    "Grand Theft Auto Episodes from Liberty City",
];

/// Find GTA V / GTA IV installs: Steam libraries (native, Flatpak, Snap),
/// Rockstar / Epic launchers inside Proton prefixes, Heroic and Lutris.
pub fn find_installs() -> Vec<Install> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    find_installs_in(&home)
}

pub fn find_installs_in(home: &Path) -> Vec<Install> {
    let mut out = Vec::new();
    let mut libraries: BTreeSet<PathBuf> = BTreeSet::new();
    for root in steam_roots(home) {
        libraries.insert(root.clone());
        if let Ok(t) = fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
            libraries.extend(vdf_paths(&t));
        }
    }
    for lib in &libraries {
        let common = lib.join("steamapps/common");
        for g in GAME_DIRS {
            let d = common.join(g);
            if d.is_dir() {
                probe(&d, "Steam", &mut out);
            }
        }
        // Rockstar Launcher / Epic installed inside a Proton prefix.
        if let Ok(rd) = fs::read_dir(lib.join("steamapps/compatdata")) {
            for e in rd.flatten() {
                for vendor in ["Rockstar Games", "Epic Games"] {
                    let d = e.path().join("pfx/drive_c/Program Files").join(vendor);
                    if d.is_dir() {
                        probe(&d, "Proton prefix", &mut out);
                    }
                }
            }
        }
    }
    // Heroic (Epic): installed.json lists install paths.
    for f in [".config/heroic/legendaryConfig/legendary/installed.json", ".var/app/com.heroicgameslauncher.hgl/config/heroic/legendaryConfig/legendary/installed.json"] {
        if let Ok(t) = fs::read_to_string(home.join(f)) {
            for part in t.split("\"install_path\"").skip(1) {
                if let Some(p) = part.split('"').nth(1) {
                    probe(Path::new(p), "Heroic", &mut out);
                }
            }
        }
    }
    // Lutris / Heroic default game folders and Wine prefixes.
    for base in ["Games", "Games/Heroic", "Games/lutris"] {
        if let Ok(rd) = fs::read_dir(home.join(base)) {
            for e in rd.flatten() {
                let p = e.path();
                probe(&p, "Games folder", &mut out);
                for vendor in ["Rockstar Games", "Epic Games"] {
                    let d = p.join("drive_c/Program Files").join(vendor);
                    if d.is_dir() {
                        probe(&d, "Wine prefix", &mut out);
                    }
                }
            }
        }
    }
    // De-duplicate (symlinked Steam roots etc.).
    let mut seen = BTreeSet::new();
    out.retain(|i| seen.insert(i.path.canonicalize().unwrap_or_else(|_| i.path.clone())));
    out
}

/// What a GTA V folder has for loading mods.
#[derive(Debug, Clone, Default)]
pub struct ModSetup {
    pub enhanced: bool,
    /// ASI loader DLL found (dinput8.dll, xinput1_4.dll, dsound.dll …).
    pub asi_loader: Option<String>,
    /// Mods-folder loader plugin found (OpenIV.asi, OpenRPF.asi, RageOpenV.asi).
    pub rpf_loader: Option<String>,
    pub script_hook: bool,
    pub asi_plugins: Vec<String>,
    pub mods_folder: bool,
    pub mods_files: usize,
    /// Steam launch option Proton needs so the loader DLL is used.
    pub launch_option: Option<String>,
    pub problems: Vec<String>,
    pub tips: Vec<String>,
}

fn count_files(dir: &Path, limit: usize) -> usize {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(e.path());
            } else {
                n += 1;
                if n >= limit {
                    return n;
                }
            }
        }
    }
    n
}

pub fn check_mod_setup(dir: &Path) -> ModSetup {
    let mut s = ModSetup { enhanced: is_enhanced(dir), ..Default::default() };
    let mut names: Vec<String> = Vec::new();
    for sub in ["", "scripts", "plugins"] {
        if let Ok(rd) = fs::read_dir(dir.join(sub)) {
            for e in rd.flatten() {
                names.push(e.file_name().to_string_lossy().into_owned());
            }
        }
    }
    let has = |n: &str| names.iter().find(|x| x.eq_ignore_ascii_case(n)).cloned();
    s.asi_loader = ["dinput8.dll", "xinput1_4.dll", "dsound.dll", "version.dll", "winmm.dll"].iter().find_map(|n| has(n));
    s.script_hook = has("ScriptHookV.dll").is_some();
    s.asi_plugins = names.iter().filter(|n| n.to_ascii_lowercase().ends_with(".asi")).cloned().collect();
    s.asi_plugins.sort();
    s.asi_plugins.dedup();
    s.rpf_loader = s.asi_plugins.iter().find(|n| ["openiv.asi", "openrpf.asi", "rageopenv.asi"].contains(&n.to_ascii_lowercase().as_str())).cloned();
    let mods = dir.join("mods");
    s.mods_folder = mods.is_dir();
    s.mods_files = if s.mods_folder { count_files(&mods, 100_000) } else { 0 };

    if let Some(l) = &s.asi_loader {
        let stem = l.trim_end_matches(".dll").trim_end_matches(".DLL");
        s.launch_option = Some(format!("WINEDLLOVERRIDES=\"{stem}=n,b\" %command%"));
        s.tips.push(format!(
            "Under Proton, Wine prefers its own {l}. Add this to the game's Steam launch options so the ASI loader runs: {}",
            s.launch_option.as_deref().unwrap()
        ));
    }
    if s.mods_files > 0 && s.rpf_loader.is_none() {
        s.problems.push(format!(
            "The mods folder has {} file(s), but no mods-folder loader (OpenIV.asi, OpenRPF.asi or RageOpenV.asi) is installed, so the game won't load them.",
            s.mods_files
        ));
    }
    if s.rpf_loader.is_some() && s.asi_loader.is_none() {
        s.problems.push("A mods-folder loader is installed, but no ASI loader DLL was found to load it.".into());
    }
    if s.enhanced && s.rpf_loader.as_deref().is_some_and(|n| n.eq_ignore_ascii_case("openiv.asi")) {
        s.problems.push("OpenIV.asi doesn't work with GTA V Enhanced; use OpenRPF or RageOpenV instead.".into());
    }
    if s.enhanced && s.asi_loader.as_deref().is_some_and(|n| n.eq_ignore_ascii_case("dinput8.dll")) && !s.script_hook {
        s.tips.push("GTA V Enhanced usually needs an xinput1_4.dll or dsound.dll ASI loader rather than dinput8.dll.".into());
    }
    if s.rpf_loader.is_none() {
        s.tips.push("To load edited archives from mods/, install a mods-folder loader: RageOpenV (Legacy and Enhanced, open source) or OpenRPF (Enhanced).".into());
    }
    s.tips.push("Mods are for story mode only: GTA Online's anti-cheat flags modified files.".into());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vdf() {
        let t = r#""libraryfolders"
{
	"0"
	{
		"path"		"/home/u/.local/share/Steam"
		"label"		""
	}
	"1"
	{
		"path"		"/mnt/games/SteamLibrary"
	}
}"#;
        assert_eq!(vdf_paths(t), vec![PathBuf::from("/home/u/.local/share/Steam"), PathBuf::from("/mnt/games/SteamLibrary")]);
    }

    #[test]
    fn finds_steam_installs_and_checks_setup() {
        let home = std::env::temp_dir().join(format!("ragekit-home-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let lib2 = home.join("disk2/SteamLibrary");
        let v = home.join(".local/share/Steam/steamapps/common/Grand Theft Auto V");
        let iv = lib2.join("steamapps/common/Grand Theft Auto IV/GTAIV");
        fs::create_dir_all(&v).unwrap();
        fs::create_dir_all(&iv).unwrap();
        fs::write(v.join("GTA5.exe"), b"").unwrap();
        fs::write(iv.join("GTAIV.exe"), b"").unwrap();
        fs::write(
            home.join(".local/share/Steam/steamapps/libraryfolders.vdf"),
            format!("\"libraryfolders\"\n{{\n \"1\"\n {{\n  \"path\" \"{}\"\n }}\n}}\n", lib2.display()),
        )
        .unwrap();
        let found = find_installs_in(&home);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().any(|i| i.game == Game::GtaV && !i.enhanced));
        assert!(found.iter().any(|i| i.game == Game::GtaIV));

        // Mods without a loader is a problem.
        fs::create_dir_all(v.join("mods/update")).unwrap();
        fs::write(v.join("mods/update/update.rpf"), b"x").unwrap();
        let s = check_mod_setup(&v);
        assert_eq!(s.mods_files, 1);
        assert!(!s.problems.is_empty());
        fs::write(v.join("dinput8.dll"), b"").unwrap();
        fs::write(v.join("RageOpenV.asi"), b"").unwrap();
        let s = check_mod_setup(&v);
        assert!(s.problems.is_empty(), "{:?}", s.problems);
        assert_eq!(s.launch_option.as_deref(), Some("WINEDLLOVERRIDES=\"dinput8=n,b\" %command%"));
        fs::remove_dir_all(&home).unwrap();
    }
}
