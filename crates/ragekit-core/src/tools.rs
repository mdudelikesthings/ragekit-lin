//! Automated installs of the usual modding prerequisites into a game
//! folder: ASI loaders, Script Hook V, mods-folder loaders (the open-source
//! RageOpenV in place of OpenIV.asi), Script Hook V .NET, and FusionFix for
//! GTA IV.
//!
//! Nothing is bundled. Each tool is downloaded from its author's own release
//! page (GitHub releases, or Alexander Blade's site for Script Hook V), or
//! installed from a zip / file the user already has. Every install is
//! recorded with backups of anything it overwrote, so it can be removed
//! again cleanly.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::vfs::Game;
use crate::{Error, Result};

/// Which build of a game a tool is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    GtaVLegacy,
    GtaVEnhanced,
    GtaIV,
}

impl Target {
    pub fn detect(dir: &Path) -> Option<Target> {
        let (game, _) = crate::vfs::detect_game(dir)?;
        Some(match game {
            Game::GtaV if dir.join("GTA5_Enhanced.exe").exists() || dir.join("gta5_enhanced.exe").exists() => Target::GtaVEnhanced,
            Game::GtaV => Target::GtaVLegacy,
            Game::GtaIV | Game::Eflc => Target::GtaIV,
            Game::MaxPayne3 => return None,
        })
    }
    pub fn label(self) -> &'static str {
        match self {
            Target::GtaVLegacy => "GTA V Legacy",
            Target::GtaVEnhanced => "GTA V Enhanced",
            Target::GtaIV => "GTA IV",
        }
    }
    /// Steam app ids, for launch options.
    pub fn steam_app_ids(self) -> &'static [u32] {
        match self {
            Target::GtaVLegacy => &[271590],
            Target::GtaVEnhanced => &[3240220],
            Target::GtaIV => &[12210, 12220],
        }
    }
}

/// Where a tool comes from.
#[derive(Debug, Clone, Copy)]
pub enum Source {
    /// Latest GitHub release of `repo`; the first asset whose name matches
    /// one of the wildcard patterns.
    GitHub { repo: &'static str, assets: &'static [&'static str] },
    /// A fixed URL.
    Url(&'static str),
    /// Alexander Blade's Script Hook V page: the newest zip linked there.
    DevC,
    /// Can't be downloaded automatically; install from a file.
    Manual { page: &'static str },
}

/// One installable tool.
#[derive(Debug, Clone, Copy)]
pub struct Tool {
    pub id: &'static str,
    pub name: &'static str,
    pub about: &'static str,
    pub targets: &'static [Target],
    pub source: Source,
    /// Wildcards (matched against the file name, case-insensitively) of the
    /// files to install from the archive; empty = everything, keeping the
    /// archive's folder layout below its top folder.
    pub files: &'static [&'static str],
    /// Other tool ids this one needs.
    pub needs: &'static [&'static str],
    /// Tools this one replaces (both shouldn't be installed).
    pub conflicts: &'static [&'static str],
    /// Make these folders after installing.
    pub folders: &'static [&'static str],
    pub page: &'static str,
}

pub const TOOLS: &[Tool] = &[
    Tool {
        id: "asi-loader",
        name: "Ultimate ASI Loader",
        about: "Loads .asi plugins (mods-folder loaders, script mods). Open source, by ThirteenAG.",
        targets: &[Target::GtaVLegacy, Target::GtaVEnhanced, Target::GtaIV],
        source: Source::GitHub { repo: "ThirteenAG/Ultimate-ASI-Loader", assets: &[] }, // URL picked per target below
        files: &["*.dll"],
        needs: &[],
        conflicts: &[],
        folders: &[],
        page: "https://github.com/ThirteenAG/Ultimate-ASI-Loader",
    },
    Tool {
        id: "scripthookv",
        name: "Script Hook V",
        about: "Runs script mods (trainers, Menyoo…). Includes its own ASI loader. By Alexander Blade; must match your game version.",
        targets: &[Target::GtaVLegacy, Target::GtaVEnhanced],
        source: Source::DevC,
        files: &["ScriptHookV.dll", "dinput8.dll", "xinput1_4.dll"],
        needs: &[],
        conflicts: &["asi-loader"],
        folders: &[],
        page: "https://www.dev-c.com/gtav/scripthookv/",
    },
    Tool {
        id: "rageopenv",
        name: "RageOpenV (mods folder loader)",
        about: "Open-source replacement for OpenIV.asi: loads edited archives from mods/. Works on Legacy and Enhanced. By Chiheb-Bacha.",
        targets: &[Target::GtaVLegacy, Target::GtaVEnhanced],
        source: Source::GitHub { repo: "Chiheb-Bacha/RageOpenV", assets: &["*RageOpenV*.zip", "*.zip"] },
        files: &["RageOpenV.asi"],
        needs: &["asi"],
        conflicts: &["openiv-asi"],
        folders: &["mods"],
        page: "https://github.com/Chiheb-Bacha/RageOpenV",
    },
    Tool {
        id: "openiv-asi",
        name: "OpenIV.asi (from your own copy)",
        about: "OpenIV's mods folder loader only ships inside OpenIV for Windows. Point ragekit at an OpenIV.asi you have; it doesn't work on Enhanced.",
        targets: &[Target::GtaVLegacy],
        source: Source::Manual { page: "https://openiv.com/" },
        files: &["OpenIV.asi"],
        needs: &["asi"],
        conflicts: &["rageopenv"],
        folders: &["mods"],
        page: "https://openiv.com/",
    },
    Tool {
        id: "shvdn",
        name: "Script Hook V .NET (Enhanced edition)",
        about: "Runs C#/.NET script mods from scripts/. Works on Legacy and Enhanced. Needs Script Hook V, and .NET Framework 4.8 in the Proton prefix.",
        targets: &[Target::GtaVLegacy, Target::GtaVEnhanced],
        source: Source::GitHub { repo: "Chiheb-Bacha/ScriptHookVDotNetEnhanced", assets: &["ScriptHookVDotNet*.zip", "*.zip"] },
        files: &["ScriptHookVDotNet.asi", "ScriptHookVDotNet*.dll", "ScriptHookVDotNet.ini", "MinHook*.dll"],
        needs: &["scripthookv"],
        conflicts: &[],
        folders: &["scripts"],
        page: "https://github.com/Chiheb-Bacha/ScriptHookVDotNetEnhanced",
    },
    Tool {
        id: "fusionfix",
        name: "FusionFix",
        about: "Fixes and quality-of-life improvements for GTA IV and EFLC (loaded as an ASI plugin). Open source, by ThirteenAG.",
        targets: &[Target::GtaIV],
        source: Source::GitHub { repo: "ThirteenAG/GTAIV.EFLC.FusionFix", assets: &["GTAIV.EFLC.FusionFix.zip", "GTAIV.EFLC.FusionFix*.zip"] },
        files: &[],
        needs: &["asi"],
        conflicts: &[],
        folders: &["plugins"],
        page: "https://github.com/ThirteenAG/GTAIV.EFLC.FusionFix",
    },
];

pub fn tool(id: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.id == id)
}

pub fn tools_for(target: Target) -> Vec<&'static Tool> {
    TOOLS.iter().filter(|t| t.targets.contains(&target)).collect()
}

fn wild(pat: &str, name: &str) -> bool {
    crate::vfs::wildcard(&pat.to_ascii_lowercase(), &name.to_ascii_lowercase())
}

// ---- downloading -----------------------------------------------------------

/// Fetches URLs. The default uses HTTPS; tests swap in a fake.
pub trait Fetch {
    fn get(&self, url: &str, referer: Option<&str>) -> Result<Vec<u8>>;
}

pub struct Http;

impl Http {
    fn fetch(url: &str, referer: Option<&str>, roots: ureq::tls::RootCerts) -> std::result::Result<Vec<u8>, String> {
        let tls = ureq::tls::TlsConfig::builder().root_certs(roots).build();
        let agent: ureq::Agent = ureq::Agent::config_builder().tls_config(tls).timeout_global(Some(std::time::Duration::from_secs(120))).build().into();
        let mut req = agent.get(url).header("User-Agent", concat!("ragekit/", env!("CARGO_PKG_VERSION")));
        if let Some(r) = referer {
            req = req.header("Referer", r);
        }
        let resp = req.call().map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        resp.into_body().into_reader().take(256 << 20).read_to_end(&mut out).map_err(|e| e.to_string())?;
        Ok(out)
    }
}

impl Fetch for Http {
    fn get(&self, url: &str, referer: Option<&str>) -> Result<Vec<u8>> {
        // The system's certificate store first (corporate proxies, distro CA
        // setups), then the bundled Mozilla list if the system has none.
        match Http::fetch(url, referer, ureq::tls::RootCerts::PlatformVerifier) {
            Ok(v) => Ok(v),
            Err(e) if e.contains("certificate") || e.contains("CA ") || e.contains("UnknownIssuer") => {
                Http::fetch(url, referer, ureq::tls::RootCerts::WebPki).map_err(|e2| Error::Invalid(format!("download failed: {url}: {e2}")))
            }
            Err(e) => Err(Error::Invalid(format!("download failed: {url}: {e}"))),
        }
    }
}

/// A downloaded (or local) package: its name and bytes.
pub struct Package {
    pub name: String,
    pub data: Vec<u8>,
    /// Version or tag, when known.
    pub version: Option<String>,
}

/// The download URL for a tool on this target, resolving GitHub "latest"
/// releases and the Script Hook V page.
pub fn resolve(tool: &Tool, target: Target, net: &dyn Fetch) -> Result<(String, Option<String>, Option<&'static str>)> {
    match tool.source {
        Source::GitHub { repo, .. } if tool.id == "asi-loader" => {
            let (tag, file) = match target {
                Target::GtaIV => ("Win32-latest", "dinput8-Win32.zip"),
                Target::GtaVEnhanced => ("x64-latest", "xinput1_4-x64.zip"),
                Target::GtaVLegacy => ("x64-latest", "dinput8-x64.zip"),
            };
            Ok((format!("https://github.com/{repo}/releases/download/{tag}/{file}"), Some(tag.into()), None))
        }
        Source::GitHub { repo, assets } => {
            let (tag, list) = match github_api_assets(repo, net) {
                Ok(v) => v,
                // Rate-limited or blocked API: read the release page instead.
                Err(api_err) => github_page_assets(repo, net).map_err(|_| api_err)?,
            };
            for pat in assets {
                for (n, u) in &list {
                    if wild(pat, n) {
                        return Ok((u.clone(), tag, None));
                    }
                }
            }
            Err(Error::NotFound(format!("{repo}: no matching download in the latest release")))
        }
        Source::Url(u) => Ok((u.to_string(), None, None)),
        Source::DevC => {
            const PAGE: &str = "https://www.dev-c.com/gtav/scripthookv/";
            let html = String::from_utf8_lossy(&net.get(PAGE, None)?).into_owned();
            let file = find_devc_zip(&html).ok_or_else(|| Error::NotFound("no Script Hook V download found on dev-c.com".into()))?;
            let version = file.trim_start_matches("ScriptHookV_").trim_end_matches(".zip").replace('_', " / ");
            Ok((format!("https://www.dev-c.com/files/{file}"), Some(version), Some(PAGE)))
        }
        Source::Manual { page } => Err(Error::Invalid(format!("{} can't be downloaded automatically; get it from {page} and install it from the file", tool.name))),
    }
}

type Assets = (Option<String>, Vec<(String, String)>);

fn github_api_assets(repo: &str, net: &dyn Fetch) -> Result<Assets> {
    let api = format!("https://api.github.com/repos/{repo}/releases/latest");
    let json: serde_json::Value = serde_json::from_slice(&net.get(&api, None)?).map_err(|e| Error::Invalid(format!("{repo}: unexpected reply from GitHub: {e}")))?;
    let tag = json["tag_name"].as_str().map(str::to_string);
    let list = json["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some((a["name"].as_str()?.to_string(), a["browser_download_url"].as_str()?.to_string())))
        .collect();
    Ok((tag, list))
}

/// Asset links from the HTML release page (no API quota needed).
fn github_page_assets(repo: &str, net: &dyn Fetch) -> Result<Assets> {
    let page = String::from_utf8_lossy(&net.get(&format!("https://github.com/{repo}/releases/latest"), None)?).into_owned();
    let marker = format!("/{repo}/releases/expanded_assets/");
    let at = page.find(&marker).ok_or_else(|| Error::NotFound(format!("{repo}: no release found")))?;
    let tag: String = page[at + marker.len()..].chars().take_while(|c| !matches!(c, '"' | '\'' | '<' | '>' | ' ')).collect();
    let html = String::from_utf8_lossy(&net.get(&format!("https://github.com{marker}{tag}"), None)?).into_owned();
    Ok((Some(tag), release_links(&html, repo)))
}

/// `(name, url)` for every `/repo/releases/download/...` link in `html`.
pub fn release_links(html: &str, repo: &str) -> Vec<(String, String)> {
    let marker = format!("/{repo}/releases/download/");
    let mut out: Vec<(String, String)> = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find(&marker) {
        let tail: String = rest[i..].chars().take_while(|c| !matches!(c, '"' | '\'' | '<' | '>' | ' ')).collect();
        rest = &rest[i + marker.len()..];
        let name = tail.rsplit('/').next().unwrap_or("").to_string();
        let url = format!("https://github.com{tail}");
        if !name.is_empty() && !out.iter().any(|(_, u)| *u == url) {
            out.push((name, url));
        }
    }
    out
}

/// `ScriptHookV_<ver>.zip` linked from the dev-c.com page.
pub fn find_devc_zip(html: &str) -> Option<String> {
    let i = html.find("ScriptHookV_")?;
    let rest = &html[i..];
    let end = rest.find(".zip")?;
    let name = &rest[..end + 4];
    name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)).then(|| name.to_string())
}

pub fn download(tool: &Tool, target: Target, net: &dyn Fetch) -> Result<Package> {
    let (url, version, referer) = resolve(tool, target, net)?;
    let data = net.get(&url, referer)?;
    let name = url.rsplit('/').next().unwrap_or("download").to_string();
    if !(data.starts_with(b"PK") || name.to_ascii_lowercase().ends_with(".asi") || name.to_ascii_lowercase().ends_with(".dll")) {
        return Err(Error::Invalid(format!(
            "{url} didn't return a zip (the site may require downloading it in a browser); download it yourself and install it from the file"
        )));
    }
    Ok(Package { name, data, version })
}

pub fn from_file(path: &Path) -> Result<Package> {
    Ok(Package { name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), data: fs::read(path)?, version: None })
}

// ---- planning --------------------------------------------------------------

/// Files a package would put into the game folder: (relative path, bytes).
pub fn files_to_install(tool: &Tool, target: Target, pkg: &Package) -> Result<Vec<(String, Vec<u8>)>> {
    let lname = pkg.name.to_ascii_lowercase();
    let mut all: Vec<(String, Vec<u8>)> = Vec::new();
    if pkg.data.starts_with(b"PK") {
        let mut z = zip::ZipArchive::new(Cursor::new(&pkg.data)).map_err(|e| Error::Invalid(format!("{}: not a usable zip: {e}", pkg.name)))?;
        for i in 0..z.len() {
            let mut f = z.by_index(i).map_err(|e| Error::Invalid(e.to_string()))?;
            if f.is_dir() {
                continue;
            }
            let Some(path) = f.enclosed_name() else { continue };
            let rel = path.to_string_lossy().replace('\\', "/");
            let mut b = Vec::new();
            f.read_to_end(&mut b)?;
            all.push((rel, b));
        }
    } else if lname.ends_with(".asi") || lname.ends_with(".dll") {
        all.push((pkg.name.clone(), pkg.data.clone()));
    } else {
        return Err(Error::Invalid(format!("{}: expected a .zip, .asi or .dll", pkg.name)));
    }
    let base = |r: &str| r.rsplit('/').next().unwrap_or(r).to_string();
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    if tool.files.is_empty() {
        // Keep the layout below a single top folder shared by every file.
        let top = all.first().and_then(|(r, _)| r.split_once('/').map(|(t, _)| t.to_string()));
        let strip = top.filter(|t| all.iter().all(|(r, _)| r.starts_with(&format!("{t}/"))));
        for (r, b) in all {
            let rel = match &strip {
                Some(t) => r[t.len() + 1..].to_string(),
                None => r,
            };
            let l = rel.to_ascii_lowercase();
            if l.ends_with("readme.txt") || l.ends_with(".md") || l.starts_with("source") {
                continue;
            }
            out.push((rel, b));
        }
    } else {
        for pat in tool.files {
            // Prefer the shallowest match (skips "…MoreLogs/" variants).
            let mut cands: Vec<&(String, Vec<u8>)> = all.iter().filter(|(r, _)| wild(pat, &base(r))).collect();
            cands.sort_by_key(|(r, _)| (r.matches('/').count(), r.to_ascii_lowercase().contains("log")));
            let mut seen = std::collections::HashSet::new();
            for (r, b) in cands {
                let name = base(r);
                if seen.insert(name.to_ascii_lowercase()) && !out.iter().any(|(o, _)| o.eq_ignore_ascii_case(&name)) {
                    out.push((name, b.clone()));
                }
            }
        }
    }
    // Script Hook V ships two ASI loaders; install the one this build uses.
    if tool.id == "scripthookv" {
        let has = |n: &str| out.iter().any(|(r, _)| r.eq_ignore_ascii_case(n));
        let drop = match target {
            Target::GtaVEnhanced if has("xinput1_4.dll") => Some("dinput8.dll"),
            _ => Some("xinput1_4.dll"),
        };
        if let Some(d) = drop {
            out.retain(|(r, _)| !r.eq_ignore_ascii_case(d));
        }
    }
    if out.is_empty() {
        return Err(Error::NotFound(format!("{}: the package has none of the expected files ({})", pkg.name, tool.files.join(", "))));
    }
    if out.iter().any(|(r, _)| r.contains("..") || r.starts_with('/')) {
        return Err(Error::Invalid("package has unsafe paths".into()));
    }
    Ok(out)
}

// ---- installing, records, removal ------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub tool: String,
    pub name: String,
    pub version: Option<String>,
    pub source: String,
    /// Installed files (relative), with whether a previous file was backed up.
    pub files: BTreeMap<String, bool>,
    pub folders: Vec<String>,
    pub when: u64,
}

fn records_dir(game: &Path) -> PathBuf {
    game.join(".ragekit").join("tools")
}

pub fn installed(game: &Path) -> Vec<Record> {
    let mut v: Vec<Record> = fs::read_dir(records_dir(game))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| fs::read(e.path().join("record.json")).ok())
        .filter_map(|b| serde_json::from_slice(&b).ok())
        .collect();
    v.sort_by(|a, b| a.tool.cmp(&b.tool));
    v
}

/// Install `files` for `tool`, backing up anything overwritten. Reinstalling
/// a tool first removes its previous install.
pub fn install(game: &Path, tool: &Tool, files: &[(String, Vec<u8>)], version: Option<String>, source: &str) -> Result<Record> {
    if installed(game).iter().any(|r| r.tool == tool.id) {
        uninstall(game, tool.id)?;
    }
    let dir = records_dir(game).join(tool.id);
    let backup = dir.join("backup");
    let mut rec = Record {
        tool: tool.id.into(),
        name: tool.name.into(),
        version,
        source: source.into(),
        files: BTreeMap::new(),
        folders: Vec::new(),
        when: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    };
    fs::create_dir_all(&dir)?;
    let mut done: Vec<PathBuf> = Vec::new();
    let res = (|| -> Result<()> {
        for (rel, bytes) in files {
            let (dest, exists) = crate::fsutil::resolve_ci(game, rel);
            if exists {
                let b = backup.join(rel);
                fs::create_dir_all(b.parent().unwrap())?;
                fs::copy(&dest, &b)?;
            }
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            crate::fsutil::write_atomic(&dest, |w| {
                w.write_all(bytes)?;
                Ok(bytes.len() as u64)
            })?;
            done.push(dest);
            rec.files.insert(rel.clone(), exists);
        }
        for f in tool.folders {
            let p = game.join(f);
            if !p.exists() {
                fs::create_dir_all(&p)?;
                rec.folders.push(f.to_string());
            }
        }
        Ok(())
    })();
    if let Err(e) = res {
        // Roll back what was written.
        for (rel, had) in &rec.files {
            let (dest, _) = crate::fsutil::resolve_ci(game, rel);
            if *had {
                let _ = fs::copy(backup.join(rel), &dest);
            } else {
                let _ = fs::remove_file(&dest);
            }
        }
        let _ = fs::remove_dir_all(&dir);
        return Err(e);
    }
    fs::write(dir.join("record.json"), serde_json::to_vec_pretty(&rec).map_err(|e| Error::Invalid(e.to_string()))?)?;
    Ok(rec)
}


/// Remove a tool: delete its files, restore what it replaced, and remove
/// folders it created if they're still empty.
pub fn uninstall(game: &Path, id: &str) -> Result<Record> {
    let dir = records_dir(game).join(id);
    let rec: Record = serde_json::from_slice(&fs::read(dir.join("record.json")).map_err(|_| Error::NotFound(format!("{id} isn't installed by ragekit")))?)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    for (rel, had) in &rec.files {
        let (dest, exists) = crate::fsutil::resolve_ci(game, rel);
        if *had {
            fs::copy(dir.join("backup").join(rel), &dest)?;
        } else if exists {
            fs::remove_file(&dest)?;
        }
    }
    for f in rec.folders.iter().rev() {
        let _ = fs::remove_dir(game.join(f)); // only if empty
    }
    fs::remove_dir_all(&dir)?;
    Ok(rec)
}

/// The DLLs that need a `native,builtin` override under Wine/Proton for the
/// installed loaders to run.
pub fn needed_overrides(game: &Path) -> Vec<String> {
    let mut v = Vec::new();
    for dll in ["dinput8", "xinput1_4", "dsound", "version", "winmm", "d3d9"] {
        let (_, exists) = crate::fsutil::resolve_ci(game, &format!("{dll}.dll"));
        if exists {
            v.push(dll.to_string());
        }
    }
    v
}

/// A one-line summary for tools a target can use.
pub fn recommended(target: Target) -> &'static [&'static str] {
    match target {
        Target::GtaVLegacy | Target::GtaVEnhanced => &["scripthookv", "rageopenv"],
        Target::GtaIV => &["asi-loader", "fusionfix"],
    }
}

/// Check a planned set of tools against what's installed: which conflicting
/// tools would be replaced, and which needs aren't met.
pub fn check(game: &Path, tool: &Tool, also: &[&str]) -> (Vec<String>, Vec<String>) {
    let inst: Vec<String> = installed(game).into_iter().map(|r| r.tool).collect();
    let has_loader = !needed_overrides(game).is_empty();
    let present = |id: &str| inst.iter().any(|t| t == id) || also.contains(&id);
    let conflicts = tool.conflicts.iter().filter(|c| present(c)).map(|c| c.to_string()).collect();
    let missing = tool
        .needs
        .iter()
        .filter(|n| match **n {
            "asi" => !(has_loader || present("asi-loader") || present("scripthookv")),
            other => !present(other),
        })
        .map(|n| if *n == "asi" { "an ASI loader".to_string() } else { tool_name(n) })
        .collect();
    (conflicts, missing)
}

fn tool_name(id: &str) -> String {
    tool(id).map(|t| t.name.to_string()).unwrap_or_else(|| id.to_string())
}

/// Download (or read `from`) and install one tool, first removing
/// conflicting tools ragekit installed. Returns the record and notes for
/// the user (replaced tools, unmet needs).
pub fn install_tool(game: &Path, id: &str, from: Option<&Path>, net: &dyn Fetch) -> Result<(Record, Vec<String>)> {
    let target = Target::detect(game).ok_or_else(|| Error::Invalid(format!("{} isn't a GTA V or GTA IV folder", game.display())))?;
    let tool = tool(id).ok_or_else(|| Error::NotFound(format!("no tool called {id}; try: {}", TOOLS.iter().map(|t| t.id).collect::<Vec<_>>().join(", "))))?;
    if !tool.targets.contains(&target) {
        return Err(Error::Invalid(format!("{} isn't for {}", tool.name, target.label())));
    }
    let mut notes = Vec::new();
    let (pkg, source) = match from {
        Some(p) => (from_file(p)?, format!("file {}", p.display())),
        None => {
            let p = download(tool, target, net)?;
            (p, "download".to_string())
        }
    };
    let files = files_to_install(tool, target, &pkg)?;
    let (conflicts, _) = check(game, tool, &[]);
    for c in conflicts {
        if installed(game).iter().any(|r| r.tool == c) {
            uninstall(game, &c)?;
            notes.push(format!("Removed {} (replaced by {})", tool_name(&c), tool.name));
        }
    }
    let rec = install(game, tool, &files, pkg.version.clone(), &source)?;
    let (_, missing) = check(game, tool, &[]);
    for m in missing {
        notes.push(format!("{} also needs {m}", tool.name));
    }
    Ok((rec, notes))
}
