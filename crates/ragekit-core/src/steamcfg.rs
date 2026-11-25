//! Steam launch options (`localconfig.vdf`), so the ASI loader DLLs run
//! under Proton: Wine prefers its own builtin `dinput8.dll` etc. unless the
//! game is started with `WINEDLLOVERRIDES="dinput8=n,b"`.
//!
//! Steam rewrites this file when it exits, so it is only changed while
//! Steam isn't running. The original is kept as `localconfig.vdf.ragekit.bak`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Is a Steam client running for this user?
pub fn steam_running() -> bool {
    let Ok(rd) = fs::read_dir("/proc") else { return false };
    rd.flatten().any(|e| {
        fs::read_to_string(e.path().join("comm")).is_ok_and(|c| matches!(c.trim(), "steam" | "steamwebhelper"))
    })
}

/// Every `localconfig.vdf` under the Steam installs in `home`.
pub fn local_configs(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in crate::games::steam_roots(home) {
        let Ok(root) = root.canonicalize() else { continue };
        if !seen.insert(root.clone()) {
            continue;
        }
        if let Ok(rd) = fs::read_dir(root.join("userdata")) {
            for e in rd.flatten() {
                let p = e.path().join("config/localconfig.vdf");
                if p.is_file() {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// The launch option text with the overrides for `dlls` and the game
/// arguments `args` (e.g. `-nobattleye`) merged into `existing`; other
/// options are kept.
pub fn merge_launch_options(existing: &str, dlls: &[String], args: &[&str]) -> String {
    let mut out = merge_overrides(existing, dlls);
    for a in args {
        if out.split_whitespace().any(|w| w.eq_ignore_ascii_case(a)) {
            continue;
        }
        if out.is_empty() {
            out = format!("%command% {a}");
        } else if out.contains("%command%") {
            out = format!("{out} {a}");
        } else {
            out = format!("%command% {out} {a}");
        }
    }
    out
}

fn merge_overrides(existing: &str, dlls: &[String]) -> String {
    let existing = existing.trim();
    let missing: Vec<&String> = dlls
        .iter()
        .filter(|d| {
            let l = existing.to_ascii_lowercase();
            // Already overridden in some form?
            !(l.contains("winedlloverrides") && l.contains(&d.to_ascii_lowercase()))
        })
        .collect();
    if missing.is_empty() {
        return existing.to_string();
    }
    let entry = format!("{}=n,b", missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(","));
    if let Some(i) = existing.find("WINEDLLOVERRIDES=") {
        let after = &existing[i + "WINEDLLOVERRIDES=".len()..];
        let (val, rest, quoted) = if let Some(q) = after.strip_prefix('"') {
            let end = q.find('"').unwrap_or(q.len());
            (&q[..end], &q[(end + 1).min(q.len())..], true)
        } else {
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            (&after[..end], &after[end..], false)
        };
        let _ = quoted;
        let merged = if val.is_empty() { entry } else { format!("{val};{entry}") };
        return format!("{}WINEDLLOVERRIDES=\"{merged}\"{rest}", &existing[..i]);
    }
    let prefix = format!("WINEDLLOVERRIDES=\"{entry}\"");
    if existing.contains("%command%") {
        format!("{prefix} {existing}")
    } else if existing.is_empty() {
        format!("{prefix} %command%")
    } else {
        // Plain game arguments: they go after %command%.
        format!("{prefix} %command% {existing}")
    }
}

// ---- a small text VDF editor that leaves everything else untouched ----------

#[derive(Debug, Clone)]
struct Tok {
    start: usize,
    end: usize,
    kind: TokKind,
}

#[derive(Debug, Clone, PartialEq)]
enum TokKind {
    Str(String),
    Open,
    Close,
}

fn lex(t: &str) -> Result<Vec<Tok>> {
    let b = t.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        match b[i] {
            b'{' => {
                out.push(Tok { start: i, end: i + 1, kind: TokKind::Open });
                i += 1;
            }
            b'}' => {
                out.push(Tok { start: i, end: i + 1, kind: TokKind::Close });
                i += 1;
            }
            b'"' => {
                let start = i;
                i += 1;
                let mut s = Vec::new();
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        s.push(b[i + 1]);
                        i += 2;
                    } else {
                        s.push(b[i]);
                        i += 1;
                    }
                }
                if i >= b.len() {
                    return Err(Error::Malformed("unterminated string in VDF".into()));
                }
                i += 1;
                out.push(Tok { start, end: i, kind: TokKind::Str(String::from_utf8_lossy(&s).into_owned()) });
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    Ok(out)
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Find the block for key path `keys` (case-insensitive): index of its
/// opening brace token.
fn find_block(toks: &[Tok], keys: &[&str]) -> Option<usize> {
    let mut i = 0;
    let mut depth_want = 0usize;
    let mut depth = 0usize;
    let mut level = 0usize;
    while i < toks.len() {
        match &toks[i].kind {
            TokKind::Str(k) if depth == depth_want && toks.get(i + 1).is_some_and(|t| t.kind == TokKind::Open) => {
                if k.eq_ignore_ascii_case(keys[level]) {
                    level += 1;
                    if level == keys.len() {
                        return Some(i + 1);
                    }
                    depth_want += 1;
                }
                depth += 1;
                i += 2;
                continue;
            }
            TokKind::Str(_) if toks.get(i + 1).is_some_and(|t| matches!(t.kind, TokKind::Str(_))) => {
                i += 2;
                continue;
            }
            TokKind::Open => depth += 1,
            TokKind::Close => {
                if depth == depth_want && level > 0 {
                    return None; // left the block we were searching
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Matching close brace for the open brace at token `open`.
fn block_end(toks: &[Tok], open: usize) -> Option<usize> {
    let mut d = 0;
    for (j, t) in toks.iter().enumerate().skip(open) {
        match t.kind {
            TokKind::Open => d += 1,
            TokKind::Close => {
                d -= 1;
                if d == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
    }
    None
}

/// Read `LaunchOptions` for `app` from localconfig text.
pub fn get_launch_options(text: &str, app: u32) -> Result<Option<String>> {
    let toks = lex(text)?;
    let Some(open) = find_block(&toks, &["UserLocalConfigStore", "Software", "Valve", "Steam", "apps", &app.to_string()]) else { return Ok(None) };
    let end = block_end(&toks, open).ok_or_else(|| Error::Malformed("unbalanced VDF".into()))?;
    let mut j = open + 1;
    let mut d = 0;
    while j < end {
        match &toks[j].kind {
            TokKind::Open => d += 1,
            TokKind::Close => d -= 1,
            TokKind::Str(k) if d == 0 => {
                if let Some(TokKind::Str(v)) = toks.get(j + 1).map(|t| &t.kind) {
                    if k.eq_ignore_ascii_case("LaunchOptions") {
                        return Ok(Some(v.clone()));
                    }
                    j += 2;
                    continue;
                }
            }
            _ => {}
        }
        j += 1;
    }
    Ok(None)
}

/// Set `LaunchOptions` for `app`, creating the app's block if needed.
pub fn set_launch_options(text: &str, app: u32, value: &str) -> Result<String> {
    let toks = lex(text)?;
    let app_s = app.to_string();
    if let Some(open) = find_block(&toks, &["UserLocalConfigStore", "Software", "Valve", "Steam", "apps", &app_s]) {
        let end = block_end(&toks, open).ok_or_else(|| Error::Malformed("unbalanced VDF".into()))?;
        let mut j = open + 1;
        let mut d = 0;
        while j < end {
            match &toks[j].kind {
                TokKind::Open => d += 1,
                TokKind::Close => d -= 1,
                TokKind::Str(k) if d == 0 => {
                    if let Some(v @ Tok { kind: TokKind::Str(_), .. }) = toks.get(j + 1) {
                        if k.eq_ignore_ascii_case("LaunchOptions") {
                            return Ok(format!("{}{}{}", &text[..v.start], quote(value), &text[v.end..]));
                        }
                        j += 2;
                        continue;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        // Insert after the opening brace, matching the next line's indent.
        let at = toks[open].end;
        let indent = indent_after(text, at);
        return Ok(format!("{}\n{indent}\"LaunchOptions\"\t\t{}{}", &text[..at], quote(value), &text[at..]));
    }
    let open = find_block(&toks, &["UserLocalConfigStore", "Software", "Valve", "Steam", "apps"])
        .ok_or_else(|| Error::NotFound("this Steam user has no game settings yet; start the game once from Steam".into()))?;
    let at = toks[open].end;
    let indent = indent_after(text, at);
    Ok(format!(
        "{}\n{indent}\"{app_s}\"\n{indent}{{\n{indent}\t\"LaunchOptions\"\t\t{}\n{indent}}}{}",
        &text[..at],
        quote(value),
        &text[at..]
    ))
}

fn indent_after(text: &str, at: usize) -> String {
    text[at..].trim_start_matches(['\r', '\n']).chars().take_while(|c| *c == '\t' || *c == ' ').collect::<String>()
}

/// Result of updating launch options for one Steam user.
#[derive(Debug)]
pub struct Applied {
    pub file: PathBuf,
    pub app: u32,
    pub before: Option<String>,
    pub after: String,
}

/// Merge the overrides into the launch options of `apps` for every Steam
/// user found that has played (or owns) them. Refuses while Steam runs.
pub fn apply_overrides(home: &Path, apps: &[u32], dlls: &[String], args: &[&str]) -> Result<Vec<Applied>> {
    if dlls.is_empty() && args.is_empty() {
        return Ok(Vec::new());
    }
    if steam_running() {
        return Err(Error::Invalid("Steam is running; quit Steam first (it overwrites its settings when it exits)".into()));
    }
    let mut out = Vec::new();
    for file in local_configs(home) {
        let mut text = fs::read_to_string(&file)?;
        let orig = text.clone();
        for &app in apps {
            let toks = lex(&text)?;
            let known = find_block(&toks, &["UserLocalConfigStore", "Software", "Valve", "Steam", "apps", &app.to_string()]).is_some();
            if !known && app != apps[0] {
                continue;
            }
            let before = get_launch_options(&text, app)?;
            let after = merge_launch_options(before.as_deref().unwrap_or(""), dlls, args);
            if before.as_deref() != Some(after.as_str()) {
                match set_launch_options(&text, app, &after) {
                    Ok(t) => text = t,
                    Err(Error::NotFound(_)) => continue,
                    Err(e) => return Err(e),
                }
            }
            out.push(Applied { file: file.clone(), app, before, after });
        }
        if text != orig {
            let mut bak = file.as_os_str().to_owned();
            bak.push(".ragekit.bak");
            fs::write(PathBuf::from(bak), &orig)?;
            crate::fsutil::write_atomic(&file, |w| {
                w.write_all(text.as_bytes())?;
                Ok(text.len() as u64)
            })?;
        }
    }
    Ok(out)
}


#[cfg(test)]
mod tests {
    use super::*;

    const CFG: &str = "\"UserLocalConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"apps\"\n\t\t\t\t{\n\t\t\t\t\t\"271590\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"LastPlayed\"\t\t\"1700000000\"\n\t\t\t\t\t\t\"LaunchOptions\"\t\t\"-windowed\"\n\t\t\t\t\t}\n\t\t\t\t\t\"12210\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"LastPlayed\"\t\t\"1\"\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n\t\"friends\"\n\t{\n\t\t\"apps\"\n\t\t{\n\t\t}\n\t}\n}\n";

    #[test]
    fn merge_rules() {
        let d = vec!["dinput8".to_string()];
        assert_eq!(merge_launch_options("", &d, &[]), "WINEDLLOVERRIDES=\"dinput8=n,b\" %command%");
        assert_eq!(merge_launch_options("-windowed", &d, &[]), "WINEDLLOVERRIDES=\"dinput8=n,b\" %command% -windowed");
        assert_eq!(merge_launch_options("gamemoderun %command%", &d, &[]), "WINEDLLOVERRIDES=\"dinput8=n,b\" gamemoderun %command%");
        assert_eq!(
            merge_launch_options("WINEDLLOVERRIDES=\"dxgi=n,b\" %command%", &d, &[]),
            "WINEDLLOVERRIDES=\"dxgi=n,b;dinput8=n,b\" %command%"
        );
        assert_eq!(merge_launch_options("", &d, &["-nobattleye"]), "WINEDLLOVERRIDES=\"dinput8=n,b\" %command% -nobattleye");
        assert_eq!(merge_launch_options("", &[], &["-nobattleye"]), "%command% -nobattleye");
        assert_eq!(
            merge_launch_options("WINEDLLOVERRIDES=\"dinput8,xinput1_4=n,b\" %command%", &d, &["-nobattleye"]),
            "WINEDLLOVERRIDES=\"dinput8,xinput1_4=n,b\" %command% -nobattleye"
        );
        assert_eq!(merge_launch_options("%command% -nobattleye", &[], &["-nobattleye"]), "%command% -nobattleye");
        let same = "WINEDLLOVERRIDES=\"dinput8=n,b\" %command%";
        assert_eq!(merge_launch_options(same, &d, &[]), same);
    }

    #[test]
    fn edit_vdf() {
        assert_eq!(get_launch_options(CFG, 271590).unwrap().as_deref(), Some("-windowed"));
        assert_eq!(get_launch_options(CFG, 12210).unwrap(), None);
        let v = "WINEDLLOVERRIDES=\"dinput8=n,b\" %command% -windowed";
        let t = set_launch_options(CFG, 271590, v).unwrap();
        assert_eq!(get_launch_options(&t, 271590).unwrap().as_deref(), Some(v));
        assert!(t.contains("\"LaunchOptions\"\t\t\"WINEDLLOVERRIDES=\\\"dinput8=n,b\\\" %command% -windowed\""));
        assert_eq!(t.len(), CFG.len() + (quote(v).len() - "\"-windowed\"".len()));
        let t = set_launch_options(&t, 12210, "X").unwrap();
        assert_eq!(get_launch_options(&t, 12210).unwrap().as_deref(), Some("X"));
        assert_eq!(get_launch_options(&t, 271590).unwrap().as_deref(), Some(v), "other app untouched");
        let t = set_launch_options(&t, 3240220, "Y").unwrap();
        assert_eq!(get_launch_options(&t, 3240220).unwrap().as_deref(), Some("Y"));
        // The friends/apps block isn't confused with Steam/apps.
        assert!(t.find("3240220").unwrap() < t.find("\"friends\"").unwrap());
    }
}
