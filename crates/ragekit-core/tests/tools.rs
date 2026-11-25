//! Installing modding tools (with a fake network) and Steam launch options.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use ragekit_core::tools::{self, Fetch, Target};
use ragekit_core::{Error, Result};

struct Fake(HashMap<String, Vec<u8>>);

impl Fetch for Fake {
    fn get(&self, url: &str, referer: Option<&str>) -> Result<Vec<u8>> {
        if url.contains("dev-c.com/files/") && referer.is_none() {
            return Ok(b"<html>hotlink</html>".to_vec());
        }
        self.0.get(url).cloned().ok_or_else(|| Error::NotFound(url.to_string()))
    }
}

fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (n, b) in files {
        z.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(b).unwrap();
    }
    z.finish().unwrap().into_inner()
}

fn game(name: &str, exe: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ragekit-tools-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(exe), b"MZ").unwrap();
    d
}

fn net() -> Fake {
    let mut m = HashMap::new();
    m.insert(
        "https://www.dev-c.com/gtav/scripthookv/".into(),
        br#"<a href="/files/ScriptHookV_3889.0_1158.13.zip">Download</a>"#.to_vec(),
    );
    m.insert(
        "https://www.dev-c.com/files/ScriptHookV_3889.0_1158.13.zip".into(),
        zip(&[
            ("bin/ScriptHookV.dll", b"shv"),
            ("bin/dinput8.dll", b"loader-legacy"),
            ("bin/xinput1_4.dll", b"loader-enhanced"),
            ("bin/NativeTrainer.asi", b"trainer"),
            ("readme.txt", b"hi"),
        ]),
    );
    m.insert(
        "https://api.github.com/repos/Chiheb-Bacha/RageOpenV/releases/latest".into(),
        br#"{"tag_name":"v1.0","assets":[{"name":"source.tar.gz","browser_download_url":"https://x/s"},{"name":"RageOpenV_v1.0.zip","browser_download_url":"https://gh/RageOpenV_v1.0.zip"}]}"#.to_vec(),
    );
    m.insert(
        "https://gh/RageOpenV_v1.0.zip".into(),
        zip(&[("RageOpenV_MoreLogs/RageOpenV.asi", b"logs"), ("RageOpenV.asi", b"release")]),
    );
    m.insert(
        "https://github.com/ThirteenAG/Ultimate-ASI-Loader/releases/download/Win32-latest/dinput8-Win32.zip".into(),
        zip(&[("dinput8.dll", b"ual32")]),
    );
    m.insert(
        "https://api.github.com/repos/ThirteenAG/GTAIV.EFLC.FusionFix/releases/latest".into(),
        br#"{"tag_name":"v5","assets":[{"name":"GTAIV.EFLC.FusionFix.zip","browser_download_url":"https://gh/ff.zip"}]}"#.to_vec(),
    );
    m.insert(
        "https://gh/ff.zip".into(),
        zip(&[("GTAIV.EFLC.FusionFix/plugins/GTAIV.EFLC.FusionFix.asi", b"ff"), ("GTAIV.EFLC.FusionFix/update/x.img", b"img")]),
    );
    Fake(m)
}

#[test]
fn devc_link() {
    assert_eq!(tools::find_devc_zip(r#"x <a href="/files/ScriptHookV_3889.0_1158.13.zip">"#).as_deref(), Some("ScriptHookV_3889.0_1158.13.zip"));
    assert_eq!(tools::find_devc_zip("nothing here"), None);
}

#[test]
fn script_hook_v_legacy_and_enhanced() {
    let n = net();
    let t = tools::tool("scripthookv").unwrap();
    for (exe, want_loader, other) in [("GTA5.exe", "dinput8.dll", "xinput1_4.dll"), ("GTA5_Enhanced.exe", "xinput1_4.dll", "dinput8.dll")] {
        let g = game(exe, exe);
        let target = Target::detect(&g).unwrap();
        let pkg = tools::download(t, target, &n).unwrap();
        assert_eq!(pkg.version.as_deref(), Some("3889.0 / 1158.13"));
        let files = tools::files_to_install(t, target, &pkg).unwrap();
        let names: Vec<&str> = files.iter().map(|(r, _)| r.as_str()).collect();
        assert!(names.contains(&"ScriptHookV.dll") && names.contains(&want_loader) && !names.contains(&other), "{names:?}");
        assert!(!names.contains(&"NativeTrainer.asi"));
        tools::install(&g, t, &files, pkg.version, "test").unwrap();
        assert_eq!(tools::needed_overrides(&g), vec![want_loader.trim_end_matches(".dll").to_string()]);
        std::fs::remove_dir_all(&g).unwrap();
    }
}

#[test]
fn rageopenv_picks_release_build_and_uninstalls_cleanly() {
    let n = net();
    let g = game("rov", "GTA5.exe");
    std::fs::write(g.join("RageOpenV.asi"), b"old user copy").unwrap();
    let t = tools::tool("rageopenv").unwrap();
    let (_, missing) = tools::check(&g, t, &[]);
    assert_eq!(missing, vec!["an ASI loader"]);
    let pkg = tools::download(t, Target::GtaVLegacy, &n).unwrap();
    let files = tools::files_to_install(t, Target::GtaVLegacy, &pkg).unwrap();
    assert_eq!(files, vec![("RageOpenV.asi".to_string(), b"release".to_vec())]);
    let rec = tools::install(&g, t, &files, pkg.version, "github").unwrap();
    assert_eq!(rec.version.as_deref(), Some("v1.0"));
    assert!(g.join("mods").is_dir());
    assert_eq!(std::fs::read(g.join("RageOpenV.asi")).unwrap(), b"release");
    assert_eq!(tools::installed(&g).len(), 1);
    // Reinstall replaces the record, still restoring the user's original.
    tools::install(&g, t, &files, None, "again").unwrap();
    tools::uninstall(&g, "rageopenv").unwrap();
    assert_eq!(std::fs::read(g.join("RageOpenV.asi")).unwrap(), b"old user copy");
    assert!(!g.join("mods").exists(), "empty mods folder it made is removed");
    assert!(tools::installed(&g).is_empty());
    std::fs::remove_dir_all(&g).unwrap();
}

#[test]
fn gta_iv_loader_and_fusionfix_keep_layout() {
    let n = net();
    let g = game("iv", "GTAIV.exe");
    assert_eq!(Target::detect(&g), Some(Target::GtaIV));
    let ual = tools::tool("asi-loader").unwrap();
    let pkg = tools::download(ual, Target::GtaIV, &n).unwrap();
    tools::install(&g, ual, &tools::files_to_install(ual, Target::GtaIV, &pkg).unwrap(), pkg.version, "github").unwrap();
    let ff = tools::tool("fusionfix").unwrap();
    assert!(tools::check(&g, ff, &[]).1.is_empty());
    let pkg = tools::download(ff, Target::GtaIV, &n).unwrap();
    let files = tools::files_to_install(ff, Target::GtaIV, &pkg).unwrap();
    let names: Vec<&str> = files.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(names, vec!["plugins/GTAIV.EFLC.FusionFix.asi", "update/x.img"]);
    tools::install(&g, ff, &files, pkg.version, "github").unwrap();
    assert!(g.join("plugins/GTAIV.EFLC.FusionFix.asi").is_file());
    assert_eq!(tools::needed_overrides(&g), vec!["dinput8"]);
    std::fs::remove_dir_all(&g).unwrap();
}

#[test]
fn manual_tools_and_bad_downloads() {
    let n = net();
    let t = tools::tool("openiv-asi").unwrap();
    assert!(tools::download(t, Target::GtaVLegacy, &n).is_err());
    let pkg = tools::Package { name: "OpenIV.asi".into(), data: b"asi".to_vec(), version: None };
    assert_eq!(tools::files_to_install(t, Target::GtaVLegacy, &pkg).unwrap().len(), 1);
    // A zip without the expected files is refused.
    let pkg = tools::Package { name: "x.zip".into(), data: zip(&[("other.txt", b"x")]), version: None };
    assert!(tools::files_to_install(tools::tool("rageopenv").unwrap(), Target::GtaVLegacy, &pkg).is_err());
}

#[test]
fn github_page_fallback_when_api_fails() {
    let mut m = HashMap::new();
    m.insert(
        "https://github.com/Chiheb-Bacha/RageOpenV/releases/latest".into(),
        br#"<include-fragment src="https://github.com/Chiheb-Bacha/RageOpenV/releases/expanded_assets/v1.0"></include-fragment>"#.to_vec(),
    );
    m.insert(
        "https://github.com/Chiheb-Bacha/RageOpenV/releases/expanded_assets/v1.0".into(),
        br#"<a href="/Chiheb-Bacha/RageOpenV/releases/download/v1.0/RageOpenV.zip">RageOpenV.zip</a><a href="/Chiheb-Bacha/RageOpenV/archive/refs/tags/v1.0.zip">src</a>"#.to_vec(),
    );
    m.insert("https://github.com/Chiheb-Bacha/RageOpenV/releases/download/v1.0/RageOpenV.zip".into(), zip(&[("RageOpenV.asi", b"r")]));
    let pkg = tools::download(tools::tool("rageopenv").unwrap(), Target::GtaVLegacy, &Fake(m)).unwrap();
    assert_eq!(pkg.version.as_deref(), Some("v1.0"));
    assert_eq!(pkg.name, "RageOpenV.zip");
}
