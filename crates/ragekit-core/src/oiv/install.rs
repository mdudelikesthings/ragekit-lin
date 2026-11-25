//! Installing `.oiv` packages.
//!
//! Installing happens in two steps:
//! 1. [`plan`] applies every operation in memory and records a log; nothing
//!    on disk changes. Missing files and unsupported commands are errors;
//!    XPath / line patterns that match nothing are warnings (as in OpenIV).
//! 2. [`Plan::commit`] writes the result. Every file it replaces or deletes
//!    is first backed up (as a hard link, so it's instant) and an install
//!    record is saved, so [`uninstall`] can put everything back.
//!    If any write fails, files written so far are restored.
//!
//! GTA V installs go to the `mods` folder by default: archives are read
//! from the game and written to `mods/<same path>`, leaving the game's own
//! files untouched. GTA IV has no mods folder, so its files are edited in
//! place (with backups).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::package::{Metadata, Op, Package};
use super::text::TextDoc;
use super::xml::XmlDoc;
use crate::blob::Blob;
use crate::fsutil::{resolve_ci, stamp, write_atomic};
use crate::keys::Keys;
use crate::tree::{FileData, Format, RowKind, RpfData, Tree};
use crate::vfs::{self, Game};
use crate::{normalize_path, Error, Result};

#[derive(Debug, Clone)]
pub struct Options {
    pub game_dir: PathBuf,
    /// GTA V only: install into the `mods` folder (recommended).
    pub use_mods: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub level: Level,
    pub msg: String,
}

enum Pending {
    Data(Vec<u8>),
    FromPackage(String),
    Delete,
}

/// Result of applying a package in memory.
pub struct Plan {
    pub log: Vec<LogLine>,
    pub game: Option<Game>,
    pub use_mods: bool,
    game_dir: PathBuf,
    meta: Metadata,
    package_file: PathBuf,
    archives: BTreeMap<PathBuf, Tree>,
    loose: BTreeMap<PathBuf, Pending>,
}

impl Plan {
    pub fn errors(&self) -> usize {
        self.log.iter().filter(|l| l.level == Level::Error).count()
    }
    pub fn warnings(&self) -> usize {
        self.log.iter().filter(|l| l.level == Level::Warn).count()
    }
    /// Files that will be written (true) or deleted (false), relative to the game folder.
    pub fn changes(&self) -> Vec<(String, bool)> {
        let rel = |p: &Path| p.strip_prefix(&self.game_dir).unwrap_or(p).display().to_string();
        let mut v: Vec<(String, bool)> = self.archives.keys().map(|p| (rel(p), true)).collect();
        v.extend(self.loose.iter().map(|(p, w)| (rel(p), !matches!(w, Pending::Delete))));
        v.sort();
        v
    }
    fn info(&mut self, m: impl Into<String>) {
        self.log.push(LogLine { level: Level::Info, msg: m.into() });
    }
    fn warn(&mut self, m: impl Into<String>) {
        self.log.push(LogLine { level: Level::Warn, msg: m.into() });
    }
    fn error(&mut self, m: impl Into<String>) {
        self.log.push(LogLine { level: Level::Error, msg: m.into() });
    }
}

#[derive(Clone)]
enum Ctx {
    Top,
    Arc { key: PathBuf, prefix: String },
}

/// Names a package's XML edits mention, so hashes in binary files resolve
/// to the same names the package uses.
fn names_in(edits: &[super::package::XmlEdit]) -> Vec<String> {
    use super::package::XmlEdit;
    use xmltree::XMLNode;
    let mut out = Vec::new();
    let mut words = |s: &str| {
        for w in s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if w.len() > 1 && !w.chars().all(|c| c.is_ascii_digit()) {
                out.push(w.to_string());
            }
        }
    };
    fn walk(n: &XMLNode, words: &mut dyn FnMut(&str)) {
        match n {
            XMLNode::Element(e) => {
                words(&e.name);
                for (k, v) in &e.attributes {
                    words(k);
                    words(v);
                }
                for c in &e.children {
                    walk(c, words);
                }
            }
            XMLNode::Text(t) | XMLNode::CData(t) => words(t),
            _ => {}
        }
    }
    for e in edits {
        match e {
            XmlEdit::Add { xpath, nodes, .. } | XmlEdit::Replace { xpath, nodes } => {
                words(xpath);
                for n in nodes {
                    walk(n, &mut words);
                }
            }
            XmlEdit::Remove { xpath } => words(xpath),
        }
    }
    out.sort();
    out.dedup();
    out
}

fn join(a: &str, b: &str) -> String {
    let b = normalize_path(b);
    if a.is_empty() {
        b
    } else if b.is_empty() {
        a.to_string()
    } else {
        format!("{a}/{b}")
    }
}

/// Work out what installing `pkg` would do, without touching the disk.
pub fn plan(pkg: &Package, opts: &Options, keys: Arc<Keys>) -> Plan {
    let mut p = Plan {
        log: Vec::new(),
        game: None,
        use_mods: false,
        game_dir: opts.game_dir.clone(),
        meta: pkg.meta.clone(),
        package_file: pkg.path.clone(),
        archives: BTreeMap::new(),
        loose: BTreeMap::new(),
    };
    let detected = vfs::detect_game(&opts.game_dir);
    let Some((game, exe)) = detected else {
        p.error(format!("{} doesn't look like a GTA V or GTA IV folder (no game .exe found)", opts.game_dir.display()));
        return p;
    };
    p.game = Some(game);
    if !game.takes_oiv() {
        p.error(format!(".oiv packages are for GTA V and GTA IV; this folder is {}", game.label()));
        return p;
    }
    let pkg_v = pkg.meta.is_gta5();
    if pkg_v != (game == Game::GtaV) {
        p.error(format!(
            "this package is for {} (target \"{}\"), but the folder is {}",
            if pkg_v { "GTA V" } else { "GTA IV" },
            pkg.meta.target,
            game.label()
        ));
        return p;
    }
    if let Some(gv) = &pkg.meta.game_version {
        let enhanced = exe.file_name().is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().contains("enhanced"));
        let want_enh = gv == "enhanced" || gv == "gen9";
        let want_leg = gv == "legacy" || gv == "gen8";
        if (want_enh && !enhanced) || (want_leg && enhanced) {
            p.error(format!(
                "this package is for GTA V {} but the game here is {}",
                if want_enh { "Enhanced" } else { "Legacy" },
                if enhanced { "Enhanced" } else { "Legacy" }
            ));
            return p;
        }
    }
    p.use_mods = game == Game::GtaV && opts.use_mods;
    p.info(format!(
        "Installing \"{}\" {} into the {} ({})",
        pkg.meta.name,
        pkg.meta.version,
        if p.use_mods { "mods folder" } else { "game folder" },
        game.label()
    ));
    if p.use_mods {
        let setup = crate::games::check_mod_setup(&opts.game_dir);
        if setup.rpf_loader.is_none() {
            p.info("note: no mods-folder loader (OpenIV.asi, OpenRPF or RageOpenV) is installed; the game won't load files from mods/ until one is");
        }
    }
    let mut r = Runner { p, pkg, keys };
    r.run(&pkg.ops, &Ctx::Top);
    r.p
}

struct Runner<'a> {
    p: Plan,
    pkg: &'a Package,
    keys: Arc<Keys>,
}

impl Runner<'_> {
    fn game_dir(&self) -> &Path {
        &self.p.game_dir
    }

    /// Where a top-level archive lives: (file to write, file to read from).
    fn archive_paths(&self, rel: &str) -> (PathBuf, Option<PathBuf>) {
        let rel = normalize_path(rel);
        let rel = rel.strip_prefix("mods/").unwrap_or(&rel).to_string();
        let (game_path, game_exists) = resolve_ci(self.game_dir(), &rel);
        if self.p.use_mods {
            // Mirror the game's own casing inside mods/.
            let canon = game_path.strip_prefix(self.game_dir()).map(|r| r.to_string_lossy().into_owned()).unwrap_or(rel.clone());
            let (mods_dir, _) = resolve_ci(self.game_dir(), "mods");
            let (target, target_exists) = resolve_ci(&mods_dir, &canon);
            let source = if target_exists {
                Some(target.clone())
            } else if game_exists {
                Some(game_path)
            } else {
                None
            };
            (target, source)
        } else {
            (game_path.clone(), game_exists.then_some(game_path))
        }
    }

    /// Where a loose (non-archive) file goes.
    fn loose_path(&self, dest: &str) -> PathBuf {
        let d = normalize_path(dest);
        if self.p.use_mods {
            let to_mods = d.starts_with("mods/") || d.starts_with("update/") || d.starts_with("x64") || d.ends_with(".rpf");
            if to_mods {
                let d = d.strip_prefix("mods/").unwrap_or(&d);
                let (game_path, _) = resolve_ci(self.game_dir(), d);
                let canon = game_path.strip_prefix(self.game_dir()).map(|r| r.to_string_lossy().into_owned()).unwrap_or(d.to_string());
                let (mods_dir, _) = resolve_ci(self.game_dir(), "mods");
                return resolve_ci(&mods_dir, &canon).0;
            }
        }
        resolve_ci(self.game_dir(), &d).0
    }

    fn open_top(&mut self, path: &str, create: bool, format: Format) -> Result<PathBuf> {
        let (target, source) = self.archive_paths(path);
        if self.p.archives.contains_key(&target) {
            return Ok(target);
        }
        // The package may have just added this archive as a file.
        if let Some(Pending::FromPackage(src)) = self.p.loose.get(&target) {
            let src = src.clone();
            let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let tree = vfs::open_blob(Blob::from_vec(self.pkg.read(&src)?), &name, self.keys.clone())?;
            self.p.loose.remove(&target);
            self.p.archives.insert(target.clone(), tree);
            return Ok(target);
        }
        let tree = match source {
            Some(src) => {
                let t = vfs::open(&src, self.keys.clone())?;
                if src != target {
                    self.p.info(format!("{}: copying from the game into mods", self.rel(&target)));
                }
                t
            }
            None if create => {
                self.p.info(format!("{}: creating new archive", self.rel(&target)));
                Tree::new(format, self.keys.clone())
            }
            None => return Err(Error::NotFound(format!("archive {path}"))),
        };
        if !tree.format.writable() {
            return Err(Error::Invalid(format!("{path}: {} archives are read-only", tree.format.label())));
        }
        self.p.archives.insert(target.clone(), tree);
        Ok(target)
    }

    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(self.game_dir()).unwrap_or(p).display().to_string()
    }

    fn tree(&mut self, key: &Path) -> &mut Tree {
        self.p.archives.get_mut(key).expect("archive opened")
    }

    fn where_(&self, ctx: &Ctx, path: &str) -> String {
        match ctx {
            Ctx::Top => normalize_path(path),
            Ctx::Arc { key, prefix } => format!("{}/{}", self.rel(key), join(prefix, path)),
        }
    }

    /// Read a file for editing: Ok(Some(bytes)), Ok(None) if missing.
    fn read_for_edit(&mut self, ctx: &Ctx, path: &str) -> Result<Option<Vec<u8>>> {
        match ctx {
            Ctx::Arc { key, prefix } => {
                let p = join(prefix, path);
                let t = self.tree(key);
                match t.kind(&p) {
                    Ok(RowKind::File) => Ok(Some(t.read(&p)?)),
                    Ok(_) => Err(Error::Invalid(format!("{p} is not a file"))),
                    Err(Error::NotFound(_)) => Ok(None),
                    Err(e) => Err(e),
                }
            }
            Ctx::Top => {
                let target = self.loose_path(path);
                match self.p.loose.get(&target) {
                    Some(Pending::Data(d)) => Ok(Some(d.clone())),
                    Some(Pending::FromPackage(s)) => Ok(Some(self.pkg.read(s)?)),
                    Some(Pending::Delete) => Ok(None),
                    None => {
                        // Edit the game's copy if the mods copy doesn't exist yet.
                        let src = if target.exists() { target.clone() } else { resolve_ci(self.game_dir(), &normalize_path(path)).0 };
                        match fs::read(&src) {
                            Ok(d) => Ok(Some(d)),
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                            Err(e) => Err(e.into()),
                        }
                    }
                }
            }
        }
    }

    fn write_edited(&mut self, ctx: &Ctx, path: &str, data: Vec<u8>) -> Result<()> {
        match ctx {
            Ctx::Arc { key, prefix } => {
                let p = join(prefix, path);
                let key = key.clone();
                self.tree(&key).insert_bytes(&p, data)
            }
            Ctx::Top => {
                let target = self.loose_path(path);
                self.p.loose.insert(target, Pending::Data(data));
                Ok(())
            }
        }
    }

    fn run(&mut self, ops: &[Op], ctx: &Ctx) {
        for op in ops {
            if let Err(e) = self.run_op(op, ctx) {
                self.p.error(e.to_string());
            }
        }
    }

    fn run_op(&mut self, op: &Op, ctx: &Ctx) -> Result<()> {
        match op {
            Op::Archive { path, create, format, children } => {
                let child_ctx = match ctx {
                    Ctx::Top => {
                        let key = self.open_top(path, *create, *format).map_err(|e| Error::Invalid(format!("archive {path}: {e}")))?;
                        Ctx::Arc { key, prefix: String::new() }
                    }
                    Ctx::Arc { key, prefix } => {
                        let p = join(prefix, path);
                        let key = key.clone();
                        let shown = self.where_(ctx, path);
                        let keys = self.keys.clone();
                        let t = self.tree(&key);
                        match t.kind(&p) {
                            Ok(RowKind::Archive) => {}
                            Ok(_) => return Err(Error::Invalid(format!("{shown} is not an archive"))),
                            Err(Error::NotFound(_)) if *create => {
                                if t.format_at(&p) != Format::Rpf7 {
                                    return Err(Error::Invalid(format!("{shown}: nested archives only exist in GTA V")));
                                }
                                t.insert(&p, FileData::Rpf(RpfData::Tree(Box::new(Tree::new(Format::Rpf7, keys)))))?;
                                self.p.info(format!("{shown}: creating new archive"));
                            }
                            Err(Error::NotFound(_)) => return Err(Error::NotFound(format!("archive {shown}"))),
                            Err(e) => return Err(Error::Invalid(format!("{shown}: {e}"))),
                        }
                        Ctx::Arc { key, prefix: p }
                    }
                };
                self.run(children, &child_ctx);
                Ok(())
            }
            Op::Add { source, dest } => {
                let dest = if dest.trim().is_empty() { normalize_path(source).rsplit('/').next().unwrap_or("").to_string() } else { dest.clone() };
                if !self.pkg.has(source) {
                    return Err(Error::NotFound(format!("package file content/{source} (for {})", self.where_(ctx, &dest))));
                }
                match ctx {
                    Ctx::Arc { key, prefix } => {
                        let p = join(prefix, &dest);
                        let data = self.pkg.read(source)?;
                        let key = key.clone();
                        let existed = self.tree(&key).contains(&p);
                        self.tree(&key).insert_bytes(&p, data).map_err(|e| Error::Invalid(format!("{}: {e}", self.where_(ctx, &dest))))?;
                        self.p.info(format!("{} {}", if existed { "replace" } else { "add    " }, self.where_(ctx, &dest)));
                    }
                    Ctx::Top => {
                        let target = self.loose_path(&dest);
                        if self.p.archives.remove(&target).is_some() {
                            self.p.warn(format!("{}: earlier edits replaced by the package's copy", self.rel(&target)));
                        }
                        self.p.info(format!("copy    {}", self.rel(&target)));
                        self.p.loose.insert(target, Pending::FromPackage(source.clone()));
                    }
                }
                Ok(())
            }
            Op::Delete { target } => {
                match ctx {
                    Ctx::Arc { key, prefix } => {
                        let p = join(prefix, target);
                        let key = key.clone();
                        match self.tree(&key).remove(&p) {
                            Ok(()) => self.p.info(format!("delete  {}", self.where_(ctx, target))),
                            Err(Error::NotFound(_)) => self.p.warn(format!("{}: not found, nothing to delete", self.where_(ctx, target))),
                            Err(e) => return Err(e),
                        }
                    }
                    Ctx::Top => {
                        let t = self.loose_path(target);
                        let t = if t.exists() { t } else { resolve_ci(self.game_dir(), &normalize_path(target)).0 };
                        if t.exists() || self.p.loose.contains_key(&t) {
                            self.p.info(format!("delete  {}", self.rel(&t)));
                            self.p.archives.remove(&t);
                            self.p.loose.insert(t, Pending::Delete);
                        } else {
                            self.p.warn(format!("{target}: not found, nothing to delete"));
                        }
                    }
                }
                Ok(())
            }
            Op::Text { path, create, edits } => {
                let shown = self.where_(ctx, path);
                let data = match self.read_for_edit(ctx, path)? {
                    Some(d) => d,
                    None if *create => Vec::new(),
                    None => return Err(Error::NotFound(format!("text file {shown}"))),
                };
                let mut doc = TextDoc::parse(&data);
                for e in edits {
                    match doc.apply(e) {
                        Ok(m) => self.p.info(format!("text    {shown}: {m}")),
                        Err(m) => self.p.warn(format!("text    {shown}: {m}")),
                    }
                }
                self.write_edited(ctx, path, doc.to_bytes())
            }
            Op::Xml { path, pso, edits } => {
                let shown = self.where_(ctx, path);
                let data = self.read_for_edit(ctx, path)?.ok_or_else(|| Error::NotFound(format!("XML file {shown}")))?;
                // Binary metadata (PSO / RSC7 Meta / RBF) is edited as XML and converted back.
                let binary = crate::meta::detect(&data);
                let xml_bytes = match binary {
                    Some(kind) => {
                        crate::meta::check_round_trip(&data).map_err(|e| Error::Invalid(format!("{shown}: {e}")))?;
                        let (x, _) = crate::meta::to_xml(&data, &names_in(edits)).map_err(|e| Error::Invalid(format!("{shown}: {e}")))?;
                        self.p.info(format!("{}     {shown}: reading {} as XML", if *pso { "pso" } else { "xml" }, kind.label()));
                        x.into_bytes()
                    }
                    None => data.clone(),
                };
                let mut doc = XmlDoc::parse(&xml_bytes).map_err(|e| Error::Invalid(format!("{shown}: {e}")))?;
                for e in edits {
                    match doc.apply(e) {
                        Ok(m) => self.p.info(format!("xml     {shown}: {m}")),
                        Err(m) => self.p.warn(format!("xml     {shown}: {m}")),
                    }
                }
                let out = doc.to_bytes()?;
                let out = match binary {
                    Some(kind) => {
                        let b = crate::meta::from_xml(&String::from_utf8_lossy(&out), &data)
                            .map_err(|e| Error::Invalid(format!("{shown}: converting back to {}: {e}", kind.label())))?;
                        self.p.info(format!("{}     {shown}: saved as {}", if *pso { "pso" } else { "xml" }, kind.label()));
                        b
                    }
                    None => out,
                };
                self.write_edited(ctx, path, out)
            }
            Op::Defragmentation => {
                self.p.info("defragmentation: not needed (archives are always written compactly)");
                Ok(())
            }
        }
    }
}

/// One file changed by an install.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    /// Path relative to the game folder.
    pub path: String,
    /// Backup file name inside the record folder, if the file existed before.
    pub backup: Option<String>,
    /// (size, mtime ns) after installing; None if the install deleted it.
    pub after: Option<(u64, u64)>,
}

/// What an install changed, saved in `<game>/.ragekit/installs/<id>/record.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRecord {
    pub id: String,
    pub package_name: String,
    pub package_version: String,
    pub package_id: String,
    pub package_file: String,
    pub installed_at: u64,
    pub target: String,
    pub files: Vec<FileRecord>,
}

fn records_dir(game_dir: &Path) -> PathBuf {
    game_dir.join(".ragekit").join("installs")
}

fn slug(s: &str) -> String {
    let s: String = s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let s = s.split('-').filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-");
    if s.is_empty() {
        "package".into()
    } else {
        s.chars().take(40).collect()
    }
}

fn backup_file(src: &Path, dst: &Path) -> Result<()> {
    if fs::hard_link(src, dst).is_err() {
        fs::copy(src, dst)?;
    }
    Ok(())
}

/// Copy a backup over `target`, keeping the backup's modification time so
/// the result matches what earlier install records expect.
fn restore_file(backup: &Path, target: &Path) -> Result<()> {
    write_atomic(target, |w| {
        let mut f = fs::File::open(backup)?;
        Ok(std::io::copy(&mut f, w)?)
    })?;
    if let Ok(t) = fs::metadata(backup).and_then(|m| m.modified()) {
        let _ = fs::OpenOptions::new().write(true).open(target).and_then(|f| f.set_modified(t));
    }
    Ok(())
}

impl Plan {
    /// Write the planned changes. With errors in the plan this refuses
    /// unless `allow_errors` (failed steps are then skipped).
    pub fn commit(self, pkg: &Package, allow_errors: bool, progress: &mut dyn FnMut(&str)) -> Result<InstallRecord> {
        if self.errors() > 0 && !allow_errors {
            return Err(Error::Invalid(format!("the package has {} error(s); nothing was installed", self.errors())));
        }
        if self.game.is_none() {
            return Err(Error::Invalid("no game folder".into()));
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut id = format!("{now}-{}", slug(&self.meta.name));
        let mut n = 1;
        while records_dir(&self.game_dir).join(&id).exists() {
            n += 1;
            id = format!("{now}-{}-{n}", slug(&self.meta.name));
        }
        let rdir = records_dir(&self.game_dir).join(&id);
        fs::create_dir_all(rdir.join("files"))?;

        let mut targets: Vec<PathBuf> = self.archives.keys().cloned().collect();
        targets.extend(self.loose.keys().cloned());
        let rel = |p: &Path| p.strip_prefix(&self.game_dir).unwrap_or(p).to_string_lossy().replace('\\', "/");

        // Backups first.
        let mut files = Vec::new();
        for (i, t) in targets.iter().enumerate() {
            let backup = if t.is_file() {
                let name = format!("{i}");
                backup_file(t, &rdir.join("files").join(&name))?;
                Some(name)
            } else {
                None
            };
            files.push(FileRecord { path: rel(t), backup, after: None });
        }

        // Writes, rolling back on failure.
        let mut done: Vec<usize> = Vec::new();
        let result = (|| -> Result<()> {
            for (i, t) in targets.iter().enumerate() {
                progress(&rel(t));
                if let Some(tree) = self.archives.get(t) {
                    write_atomic(t, |w| tree.write(w))?;
                } else {
                    match &self.loose[t] {
                        Pending::Data(d) => {
                            write_atomic(t, |w| {
                                w.write_all(d)?;
                                Ok(d.len() as u64)
                            })?;
                        }
                        Pending::FromPackage(src) => {
                            write_atomic(t, |w| pkg.copy_to(src, w))?;
                        }
                        Pending::Delete => {
                            if t.exists() {
                                fs::remove_file(t)?;
                            }
                        }
                    }
                }
                done.push(i);
            }
            Ok(())
        })();
        if let Err(e) = result {
            for &i in done.iter().rev() {
                let t = &targets[i];
                match &files[i].backup {
                    Some(b) => {
                        let _ = restore_file(&rdir.join("files").join(b), t);
                    }
                    None => {
                        let _ = fs::remove_file(t);
                    }
                }
            }
            let _ = fs::remove_dir_all(&rdir);
            return Err(Error::Invalid(format!("install failed and was rolled back: {e}")));
        }

        for (i, t) in targets.iter().enumerate() {
            files[i].after = stamp(t);
        }
        let rec = InstallRecord {
            id,
            package_name: self.meta.name.clone(),
            package_version: self.meta.version.clone(),
            package_id: self.meta.id.clone(),
            package_file: self.package_file.display().to_string(),
            installed_at: now,
            target: if self.use_mods { "mods".into() } else { "game".into() },
            files,
        };
        let json = serde_json::to_vec_pretty(&rec).map_err(|e| Error::Invalid(e.to_string()))?;
        fs::write(rdir.join("record.json"), json)?;
        Ok(rec)
    }
}

/// Installed packages for a game folder, oldest first.
pub fn list(game_dir: &Path) -> Vec<InstallRecord> {
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir(records_dir(game_dir)) {
        for e in rd.flatten() {
            if let Ok(b) = fs::read(e.path().join("record.json")) {
                if let Ok(r) = serde_json::from_slice::<InstallRecord>(&b) {
                    v.push(r);
                }
            }
        }
    }
    v.sort_by(|a, b| a.installed_at.cmp(&b.installed_at).then(a.id.cmp(&b.id)));
    v
}

/// Undo an install. Refuses (listing the files) if something changed those
/// files since, unless `force`.
pub fn uninstall(game_dir: &Path, id: &str, force: bool) -> Result<Vec<String>> {
    let rdir = records_dir(game_dir).join(id);
    let rec: InstallRecord = serde_json::from_slice(&fs::read(rdir.join("record.json")).map_err(|_| Error::NotFound(format!("install {id}")))?)
        .map_err(|e| Error::Invalid(format!("install record: {e}")))?;
    let changed: Vec<&str> =
        rec.files.iter().filter(|f| stamp(&game_dir.join(&f.path)) != f.after).map(|f| f.path.as_str()).collect();
    if !changed.is_empty() && !force {
        return Err(Error::Invalid(format!(
            "these files changed after \"{}\" was installed (maybe by a later install):\n  {}\nUninstall later packages first, or force it.",
            rec.package_name,
            changed.join("\n  ")
        )));
    }
    let mut log = Vec::new();
    for f in rec.files.iter().rev() {
        let target = game_dir.join(&f.path);
        match &f.backup {
            Some(b) => {
                restore_file(&rdir.join("files").join(b), &target)?;
                log.push(format!("restored {}", f.path));
            }
            None => {
                if target.exists() {
                    fs::remove_file(&target)?;
                }
                log.push(format!("removed  {}", f.path));
            }
        }
    }
    fs::remove_dir_all(&rdir)?;
    Ok(log)
}
