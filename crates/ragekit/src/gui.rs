//! The graphical browser.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{anyhow, Result};
use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use ragekit_core::tree::node_from_disk;
use ragekit_core::vfs::{self, Game, Hit};
use ragekit_core::{Keys, RowKind, Tree};

use crate::gui_oiv::{installed_window, PkgAction, PkgUi, Stage};
use crate::gui_tex::{TexAction, TexSource, TexViewer};
use crate::ops::{self, human, join};

pub fn run(keys_dir: PathBuf, start: Option<PathBuf>) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ragekit")
            .with_app_id("ragekit")
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([700.0, 400.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("ragekit", options, Box::new(move |cc| Ok(Box::new(App::new(cc, keys_dir, start)))))
        .map_err(|e| anyhow!("could not open a window ({e}). Use `ragekit --help` for the command-line tool."))
}

/// A place in the browser: a folder on disk, or a path inside an archive file.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Loc {
    disk: PathBuf,
    /// None: list `disk` as a folder. Some(p): path `p` inside archive `disk`.
    inner: Option<String>,
}

impl Loc {
    fn disk(p: PathBuf) -> Loc {
        Loc { disk: p, inner: None }
    }
    fn display(&self) -> String {
        match &self.inner {
            None => self.disk.display().to_string(),
            Some(i) if i.is_empty() => self.disk.display().to_string(),
            Some(i) => format!("{}/{}", self.disk.display(), i),
        }
    }
    fn parent(&self) -> Option<Loc> {
        match &self.inner {
            None => self.disk.parent().map(|p| Loc::disk(p.to_path_buf())),
            Some(i) if i.is_empty() => self.disk.parent().map(|p| Loc::disk(p.to_path_buf())),
            Some(i) => Some(Loc { disk: self.disk.clone(), inner: Some(i.rsplit_once('/').map(|x| x.0).unwrap_or("").to_string()) }),
        }
    }
    fn child(&self, name: &str, kind: RowKind) -> Loc {
        match &self.inner {
            None if kind == RowKind::Folder => Loc::disk(self.disk.join(name)),
            None => Loc { disk: self.disk.join(name), inner: Some(String::new()) },
            Some(i) => Loc { disk: self.disk.clone(), inner: Some(join(i, name)) },
        }
    }
    /// Path of a child inside the archive.
    fn inner_child(&self, name: &str) -> Option<String> {
        self.inner.as_ref().map(|i| join(i, name))
    }
}

#[derive(Clone)]
struct DRow {
    name: String,
    kind: RowKind,
    size: u64,
    type_desc: String,
    detail: String,
}

struct Opened {
    tree: Tree,
    dirty: bool,
}

enum Preview {
    Text { name: String, text: String, truncated: bool },
    Hex { name: String, dump: String, size: usize },
    Info { name: String, lines: Vec<String> },
    Error(String),
}

enum JobDone {
    Msg(String),
    Saved { disk: PathBuf, msg: String },
    Keys { keys: Keys, msg: String },
    Plan(Box<ragekit_core::oiv::Plan>),
    Installed(String),
}

struct Job {
    label: String,
    progress: Arc<Mutex<String>>,
    handle: Option<JoinHandle<Result<JobDone>>>,
}

struct Search {
    pattern: String,
    results: Arc<Mutex<Vec<Hit>>>,
    errors: Arc<Mutex<Vec<String>>>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    shown: bool,
}

enum Dialog {
    None,
    Rename { path: String, name: String },
    NewFolder { name: String },
    ConfirmDelete { paths: Vec<String> },
    ModsCopy { archive: PathBuf, mods_target: PathBuf },
    ConfirmQuit,
    ForceUninstall { id: String, body: String },
    Message { title: String, body: String },
}

struct AddonUi {
    folder: PathBuf,
    name: String,
    scan: Result<ragekit_core::dlc::VehiclePack, String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortCol {
    Name,
    Size,
    Type,
}

struct App {
    keys_dir: PathBuf,
    keys: Arc<Keys>,
    root: Option<PathBuf>,
    game: Option<Game>,
    loc: Loc,
    back: Vec<Loc>,
    forward: Vec<Loc>,
    address: String,
    opened: HashMap<PathBuf, Opened>,
    cache: HashMap<Loc, Result<Vec<DRow>, String>>,
    selected: BTreeSet<String>,
    anchor: Option<usize>,
    filter: String,
    preview: Option<Preview>,
    preview_for: Option<(Loc, String)>,
    edit_mode: bool,
    settings: crate::settings::Settings,
    installs: Vec<ragekit_core::games::Install>,
    setup_window: bool,
    hash_window: Option<String>,
    addon: Option<AddonUi>,
    sort: (SortCol, bool),
    scroll_to: Option<usize>,
    job: Option<Job>,
    search: Option<Search>,
    search_text: String,
    keys_window: bool,
    pkg_ui: Option<PkgUi>,
    tex_viewer: Option<TexViewer>,
    installs_window: bool,
    pending_uninstall: Option<String>,
    dialog: Dialog,
    status: String,
    allow_quit: bool,
    sync_tree: bool,
    ctx: egui::Context,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, keys_dir: PathBuf, start: Option<PathBuf>) -> App {
        let keys = Arc::new(Keys::load_dir(&keys_dir).unwrap_or_default());
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let mut app = App {
            keys_dir,
            keys,
            root: None,
            game: None,
            loc: Loc::disk(home.clone()),
            back: Vec::new(),
            forward: Vec::new(),
            address: String::new(),
            opened: HashMap::new(),
            cache: HashMap::new(),
            selected: BTreeSet::new(),
            anchor: None,
            filter: String::new(),
            preview: None,
            preview_for: None,
            edit_mode: false,
            settings: crate::settings::Settings::load(),
            installs: ragekit_core::games::find_installs(),
            setup_window: false,
            hash_window: None,
            addon: None,
            sort: (SortCol::Name, true),
            scroll_to: None,
            job: None,
            search: None,
            search_text: String::new(),
            keys_window: false,
            pkg_ui: None,
            tex_viewer: None,
            installs_window: false,
            pending_uninstall: None,
            dialog: Dialog::None,
            status: "Open a game folder or an archive to start.".into(),
            allow_quit: false,
            sync_tree: true,
            ctx: cc.egui_ctx.clone(),
        };
        app.apply_view();
        // First run: open a detected game.
        let start = start.or_else(ops::load_last_root).or_else(|| app.installs.first().map(|i| i.path.clone()));
        let is_pkg = start.as_ref().is_some_and(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("oiv")));
        if is_pkg {
            // Show the game folder behind the install dialog.
            app.open_path(&ops::load_last_root().unwrap_or(home));
            app.open_package(start.as_ref().unwrap());
        } else {
            let start = start.unwrap_or(home);
            app.open_path(&start);
        }
        app
    }

    // ---- navigation ---------------------------------------------------

    fn open_path(&mut self, p: &Path) {
        let p = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        if !p.exists() {
            self.status = format!("{} not found", p.display());
            return;
        }
        if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("oiv")) {
            self.open_package(&p);
            return;
        }
        if p.is_dir() && vfs::detect_game(&p).is_some() || vfs::is_archive_name(&p.to_string_lossy()) {
            self.settings.touch_recent(p.clone());
        }
        // Use the game folder above this path as the root, if there is one.
        let game_root = p.ancestors().find(|a| a.is_dir() && vfs::detect_game(a).is_some()).map(Path::to_path_buf);
        let fallback = if p.is_dir() { p.clone() } else { p.parent().unwrap_or(Path::new("/")).to_path_buf() };
        let root = game_root.unwrap_or(fallback);
        if self.root.as_ref() != Some(&root) {
            self.set_root(&root);
        }
        if p.is_dir() {
            self.go(Loc::disk(p.clone()));
        } else if vfs::is_archive_name(&p.to_string_lossy()) {
            self.go(Loc { disk: p.clone(), inner: Some(String::new()) });
        } else {
            self.go(Loc::disk(p.parent().unwrap().to_path_buf()));
            self.selected = [p.file_name().unwrap().to_string_lossy().into_owned()].into();
        }
    }

    fn set_root(&mut self, p: &Path) {
        self.root = Some(p.to_path_buf());
        self.game = vfs::detect_game(p).map(|g| g.0);
        self.status = match self.game {
            Some(g) => format!("{} folder: {}", g.label(), p.display()),
            None => format!("Opened {}", p.display()),
        };
        if self.game.is_some() {
            ops::save_last_root(p);
        }
    }

    fn go(&mut self, loc: Loc) {
        if loc != self.loc {
            self.back.push(self.loc.clone());
            self.forward.clear();
        }
        self.set_loc(loc);
    }

    fn set_loc(&mut self, loc: Loc) {
        self.address = loc.display();
        if loc.disk != self.loc.disk {
            self.edit_mode = false;
        }
        self.loc = loc;
        self.sync_tree = true;
        self.selected.clear();
        self.anchor = None;
        self.filter.clear();
        if let Some(s) = &mut self.search {
            s.shown = false;
        }
        if self.loc.inner.is_none() {
            self.edit_mode = false;
        }
    }

    fn go_address(&mut self) {
        let text = self.address.trim().to_string();
        let path = PathBuf::from(&text);
        if path.exists() {
            self.open_path(&path);
            return;
        }
        // Path inside an archive: find the longest prefix that is a file.
        let mut cur = path.clone();
        while let Some(parent) = cur.parent() {
            if cur.is_file() {
                let inner = path.strip_prefix(&cur).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                self.go(Loc { disk: cur, inner: Some(inner) });
                return;
            }
            cur = parent.to_path_buf();
        }
        self.status = format!("{text} not found");
    }

    // ---- data ---------------------------------------------------------

    fn tree(&mut self, disk: &Path) -> Result<&mut Opened> {
        if !self.opened.contains_key(disk) {
            let tree = vfs::open(disk, self.keys.clone())?;
            self.opened.insert(disk.to_path_buf(), Opened { tree, dirty: false });
        }
        Ok(self.opened.get_mut(disk).unwrap())
    }

    fn rows(&mut self, loc: &Loc) -> Result<Vec<DRow>, String> {
        if let Some(r) = self.cache.get(loc) {
            return r.clone();
        }
        let r = self.load_rows(loc).map_err(|e| format!("{e:#}"));
        self.cache.insert(loc.clone(), r.clone());
        r
    }

    fn load_rows(&mut self, loc: &Loc) -> Result<Vec<DRow>> {
        match &loc.inner {
            None => {
                let mut rows = Vec::new();
                for e in fs::read_dir(&loc.disk)?.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        continue;
                    }
                    let Ok(md) = e.metadata() else { continue };
                    let kind = if md.is_dir() {
                        RowKind::Folder
                    } else if vfs::is_archive_name(&name) {
                        RowKind::Archive
                    } else {
                        RowKind::File
                    };
                    rows.push(DRow {
                        type_desc: match kind {
                            RowKind::Folder => "Folder".into(),
                            _ => ops::describe(&name).to_string(),
                        },
                        size: if md.is_dir() { 0 } else { md.len() },
                        detail: String::new(),
                        kind,
                        name,
                    });
                }
                rows.sort_by(|a, b| {
                    (a.kind == RowKind::File)
                        .cmp(&(b.kind == RowKind::File))
                        .then_with(|| (a.kind == RowKind::Archive).cmp(&(b.kind == RowKind::Archive)))
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                Ok(rows)
            }
            Some(inner) => {
                let inner = inner.clone();
                let o = self.tree(&loc.disk)?;
                Ok(o
                    .tree
                    .list(&inner)?
                    .into_iter()
                    .map(|r| DRow {
                        type_desc: match r.kind {
                            RowKind::Folder => "Folder".into(),
                            RowKind::Archive => "Archive".into(),
                            RowKind::File => {
                                let d = ops::describe(&r.name);
                                if d.is_empty() { r.type_label.to_string() } else { d.to_string() }
                            }
                        },
                        name: r.name,
                        kind: r.kind,
                        size: r.size,
                        detail: r.detail,
                    })
                    .collect())
            }
        }
    }

    fn invalidate(&mut self, disk: &Path) {
        self.cache.retain(|l, _| l.disk != disk && !l.disk.starts_with(disk));
        self.preview = None;
        self.preview_for = None;
    }

    fn reload_keys(&mut self, keys: Keys) {
        self.keys = Arc::new(keys);
        // Re-open clean archives with the new keys.
        self.opened.retain(|_, o| o.dirty);
        self.cache.clear();
    }

    fn current_archive_writable(&self) -> Option<bool> {
        self.loc.inner.as_ref()?;
        self.opened.get(&self.loc.disk).map(|o| o.tree.format_at(&self.loc.inner.clone().unwrap_or_default()).writable() && o.tree.format.writable())
    }

    fn dirty_count(&self) -> usize {
        self.opened.values().filter(|o| o.dirty).count()
    }

    // ---- packages -----------------------------------------------------

    fn open_package(&mut self, p: &Path) {
        match ragekit_core::oiv::Package::open(p) {
            Ok(pkg) => self.pkg_ui = Some(PkgUi::new(&self.ctx, pkg)),
            Err(e) => self.error("Package", &format!("{}: {e}", p.display())),
        }
    }

    /// Drop cached listings and clean archives after files changed on disk.
    fn refresh_all(&mut self) {
        self.opened.retain(|_, o| o.dirty);
        self.cache.clear();
        self.preview = None;
        self.preview_for = None;
    }

    fn game_target(&self) -> Option<(PathBuf, Game)> {
        let r = self.root.as_ref()?;
        vfs::detect_game(r).map(|(g, _)| (r.clone(), g))
    }

    fn packages_ui(&mut self, ctx: &egui::Context) {
        let game = self.game_target();
        let action = match &mut self.pkg_ui {
            Some(p) => p.show(ctx, game.clone()),
            None => PkgAction::None,
        };
        match action {
            PkgAction::None => {}
            PkgAction::Close => self.pkg_ui = None,
            PkgAction::OpenGameFolder => {
                if let Some(p) = rfd::FileDialog::new().set_title("Open game folder").pick_folder() {
                    self.set_root(&p);
                    self.go(Loc::disk(p));
                }
            }
            PkgAction::Plan { pkg, opts } => {
                if let Some(p) = &mut self.pkg_ui {
                    p.stage = Stage::Planning;
                }
                let keys = self.keys.clone();
                self.start_job("Checking package", move |_| Ok(JobDone::Plan(Box::new(ragekit_core::oiv::plan(&pkg, &opts, keys)))));
            }
            PkgAction::Commit { pkg, plan, allow_errors } => {
                if self.dirty_count() > 0 {
                    // Don't let unsaved edits and the install fight over the same files.
                    if let Some(p) = &mut self.pkg_ui {
                        p.stage = Stage::Planned(plan);
                    }
                    self.error("Install", "Save or discard your unsaved archive edits first.");
                    return;
                }
                self.start_job("Installing", move |prog| {
                    let rec = plan.commit(&pkg, allow_errors, &mut |f| *prog.lock().unwrap() = f.to_string())?;
                    Ok(JobDone::Installed(format!("Installed \"{}\" ({} file(s) changed). You can undo it under Packages > Installed packages.", rec.package_name, rec.files.len())))
                });
            }
        }

        let root = self.game_target().map(|g| g.0);
        if self.installs_window {
            let mut open = true;
            if let Some((id, force)) = installed_window(ctx, &mut open, root.as_deref()) {
                self.uninstall(id, force);
            }
            self.installs_window = open;
        }
    }

    fn uninstall(&mut self, id: String, force: bool) {
        let Some((game, _)) = self.game_target() else { return };
        if self.dirty_count() > 0 {
            self.error("Uninstall", "Save or discard your unsaved archive edits first.");
            return;
        }
        // Release our maps of files that are about to be replaced.
        self.refresh_all();
        self.pending_uninstall = None;
        let id2 = id.clone();
        self.start_job("Uninstalling", move |_| match ragekit_core::oiv::uninstall(&game, &id2, force) {
            Ok(log) => Ok(JobDone::Installed(format!("Uninstalled: {} file(s) put back as they were", log.len()))),
            Err(e) => Err(anyhow!("{e}")),
        });
        if !force {
            self.pending_uninstall = Some(id);
        }
    }

    // ---- view & tools ---------------------------------------------------

    fn apply_view(&mut self) {
        self.ctx.set_visuals(if self.settings.dark { egui::Visuals::dark() } else { egui::Visuals::light() });
        self.ctx.set_zoom_factor(self.settings.ui_scale.clamp(0.6, 2.0));
    }

    fn setup_ui(&mut self, ctx: &egui::Context) {
        if !self.setup_window {
            return;
        }
        let Some(root) = self.root.clone() else { return };
        let s = ragekit_core::games::check_mod_setup(&root);
        let mut open = true;
        egui::Window::new("Mod setup").open(&mut open).default_width(560.0).anchor(egui::Align2::CENTER_TOP, [0.0, 90.0]).show(ctx, |ui| {
            let ok = |b: bool| if b { RichText::new("found").color(Color32::LIGHT_GREEN) } else { RichText::new("missing").color(Color32::from_rgb(255, 170, 60)) };
            let found = |n: &Option<String>| match n {
                Some(n) => RichText::new(n.as_str()).color(Color32::LIGHT_GREEN),
                None => RichText::new("missing").color(Color32::from_rgb(255, 170, 60)),
            };
            ui.label(RichText::new(format!("GTA V {}", if s.enhanced { "Enhanced" } else { "Legacy" })).strong());
            egui::Grid::new("setup").num_columns(2).spacing([20.0, 4.0]).show(ui, |ui| {
                ui.label("ASI loader");
                ui.label(found(&s.asi_loader));
                ui.end_row();
                ui.label("Mods-folder loader");
                ui.label(found(&s.rpf_loader));
                ui.end_row();
                ui.label("Script Hook V");
                ui.label(ok(s.script_hook));
                ui.end_row();
                ui.label("mods folder");
                ui.label(if s.mods_folder { format!("{} file(s)", s.mods_files) } else { "not created yet".into() });
                ui.end_row();
                ui.label("ASI plugins");
                ui.label(if s.asi_plugins.is_empty() { "none".into() } else { s.asi_plugins.join(", ") });
                ui.end_row();
            });
            for p in &s.problems {
                ui.colored_label(Color32::LIGHT_RED, p);
            }
            if let Some(opt) = &s.launch_option {
                ui.separator();
                ui.label("Steam launch options for Proton:");
                ui.horizontal(|ui| {
                    ui.code(opt);
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(opt.clone());
                    }
                });
            }
            ui.separator();
            for t in &s.tips {
                ui.label(RichText::new(t).weak());
            }
        });
        self.setup_window = open;
    }

    fn tools_ui(&mut self, ctx: &egui::Context) {
        if let Some(text) = &mut self.hash_window {
            let mut open = true;
            egui::Window::new("Hash calculator").open(&mut open).default_width(420.0).show(ctx, |ui| {
                ui.label("Model names, texture names and text labels are stored as Jenkins (joaat) hashes of their lowercase name.");
                ui.add(egui::TextEdit::multiline(text).hint_text("One name per line").desired_rows(4).desired_width(f32::INFINITY));
                egui::Grid::new("hashes").striped(true).show(ui, |ui| {
                    for n in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                        let h = ragekit_core::crypto::joaat(n);
                        ui.label(n);
                        ui.code(format!("0x{h:08X}"));
                        ui.label(format!("{h}"));
                        ui.label(format!("{}", h as i32));
                        if ui.small_button("Copy").clicked() {
                            ui.ctx().copy_text(format!("0x{h:08X}"));
                        }
                        ui.end_row();
                    }
                });
            });
            if !open {
                self.hash_window = None;
            }
        }
        let mut close = false;
        let mut install: Option<(ragekit_core::dlc::VehiclePack, bool)> = None;
        if let Some(a) = &mut self.addon {
            let mut open = true;
            egui::Window::new("Add-on vehicle pack").open(&mut open).default_width(560.0).anchor(egui::Align2::CENTER_TOP, [0.0, 90.0]).show(ctx, |ui| {
                ui.label(format!("From {}", a.folder.display()));
                ui.horizontal(|ui| {
                    ui.label("Pack name");
                    if ui.text_edit_singleline(&mut a.name).changed() {
                        a.name = ragekit_core::dlc::clean_name(&a.name);
                    }
                });
                match &mut a.scan {
                    Err(e) => {
                        ui.colored_label(Color32::LIGHT_RED, e.as_str());
                        ui.label("Expected: .yft and .ytd model files, plus vehicles.meta (and optionally handling, carcols, carvariations, vehiclelayouts .meta).");
                    }
                    Ok(p) => {
                        p.name = a.name.clone();
                        ui.label(RichText::new(format!("{} model/texture file(s) go into x64/vehicles.rpf:", p.stream.len())).strong());
                        egui::ScrollArea::vertical().max_height(120.0).id_salt("addon-stream").show(ui, |ui| {
                            for (f, d) in &p.stream {
                                ui.label(RichText::new(format!("{f}  ({})", human(d.len() as u64))).monospace().size(11.5));
                            }
                        });
                        ui.label(RichText::new("Data files registered in content.xml:").strong());
                        for (f, _, t) in &p.data {
                            ui.label(RichText::new(format!("{f}  ->  {t}")).monospace().size(11.5));
                        }
                        if !p.skipped.is_empty() {
                            ui.label(RichText::new(format!("Ignored: {}", p.skipped.join(", "))).weak());
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            let can_install = self.game == Some(Game::GtaV);
                            if ui.add_enabled(can_install, egui::Button::new(RichText::new("Install into the game").strong())).on_disabled_hover_text("Open your GTA V folder first").clicked() {
                                install = Some((std::mem::take(p), true));
                            }
                            if ui.button("Save as .oiv…").clicked() {
                                install = Some((std::mem::take(p), false));
                            }
                        });
                        ui.label(RichText::new("Installing adds dlcpacks:/NAME/ to dlclist.xml in mods/update/update.rpf. Undo it under Packages > Installed packages.").weak());
                    }
                }
            });
            if !open {
                close = true;
            }
        }
        if close {
            self.addon = None;
        }
        if let Some((pack, to_game)) = install {
            self.addon = None;
            let keys = self.keys.clone();
            if to_game {
                let Some((game, _)) = self.game_target() else { return };
                if self.dirty_count() > 0 {
                    return self.error("Add-on pack", "Save or discard your unsaved archive edits first.");
                }
                self.refresh_all();
                self.start_job("Installing add-on pack", move |prog| {
                    let tmp = std::env::temp_dir().join(format!("ragekit-addon-{}-{}", std::process::id(), pack.name));
                    let _ = fs::remove_dir_all(&tmp);
                    ragekit_core::dlc::write_package_dir(&pack, &tmp, keys.clone())?;
                    let pkg = ragekit_core::oiv::Package::open(&tmp)?;
                    let plan = ragekit_core::oiv::plan(&pkg, &ragekit_core::oiv::Options { game_dir: game, use_mods: true }, keys);
                    if plan.errors() > 0 {
                        let msgs: Vec<String> = plan.log.iter().filter(|l| l.level == ragekit_core::oiv::Level::Error).map(|l| l.msg.clone()).collect();
                        return Err(anyhow!("{}", msgs.join("\n")));
                    }
                    let rec = plan.commit(&pkg, false, &mut |f| *prog.lock().unwrap() = f.to_string())?;
                    let _ = fs::remove_dir_all(&tmp);
                    Ok(JobDone::Installed(format!("Installed add-on pack \"{}\" ({} file(s) changed). Spawn it by its model name.", pack.name, rec.files.len())))
                });
            } else if let Some(out) = rfd::FileDialog::new().set_file_name(format!("{}.oiv", pack.name)).add_filter("OpenIV package", &["oiv"]).save_file() {
                match ragekit_core::dlc::write_oiv(&pack, &out, keys) {
                    Ok(()) => self.status = format!("Saved {}", out.display()),
                    Err(e) => self.error("Add-on pack", &e.to_string()),
                }
            }
        }
    }

    /// Open a file with the desktop's default app (extracting it first if
    /// it's inside an archive). With `as_text`, open its XML/text form.
    fn open_external(&mut self, name: &str, as_text: bool) {
        let target = match (&self.loc.inner, as_text) {
            (None, false) => self.loc.disk.join(name),
            _ => {
                let Some(data) = self.selected_bytes(name) else { return };
                let dir = std::env::temp_dir().join(format!("ragekit-open-{}", std::process::id()));
                let _ = fs::create_dir_all(&dir);
                if as_text {
                    match ragekit_core::convert::to_text(&data) {
                        Ok((t, c)) => {
                            let p = dir.join(format!("{name}.{}", c.ext()));
                            let _ = fs::write(&p, t);
                            p
                        }
                        Err(e) => return self.error("Open", &e.to_string()),
                    }
                } else {
                    let p = dir.join(name);
                    let _ = fs::write(&p, data);
                    p
                }
            }
        };
        match std::process::Command::new("xdg-open").arg(&target).spawn() {
            Ok(_) => self.status = format!("Opened {}", target.display()),
            Err(e) => self.error("Open", &format!("couldn't run xdg-open: {e}")),
        }
    }

    // ---- textures -----------------------------------------------------

    fn open_textures(&mut self, name: &str) {
        let Some(data) = self.selected_bytes(name) else { return };
        match ragekit_core::textures::load(&data) {
            Ok(dict) => {
                let source = match &self.loc.inner {
                    None => TexSource::Disk(self.loc.disk.join(name)),
                    Some(i) => TexSource::Archive { disk: self.loc.disk.clone(), inner: join(i, name) },
                };
                self.tex_viewer = Some(TexViewer::new(name.to_string(), source, dict));
            }
            Err(e) => self.error("Textures", &format!("{name}: {e}")),
        }
    }

    fn textures_ui(&mut self, ctx: &egui::Context) {
        let Some(v) = &mut self.tex_viewer else { return };
        let can_edit = match &v.source {
            TexSource::Disk(_) => true,
            TexSource::Archive { disk, .. } => {
                self.edit_mode && &self.loc.disk == disk && self.opened.get(disk).is_some_and(|o| o.tree.format.writable())
            }
        };
        match v.show(ctx, can_edit) {
            TexAction::None => {}
            TexAction::Close => self.tex_viewer = None,
            TexAction::Message(t, m) => self.dialog = Dialog::Message { title: t, body: m },
            TexAction::Save(bytes) => match v.source.clone() {
                TexSource::Archive { disk, inner } => {
                    let res = self.tree(&disk).map_err(|e| e.to_string()).and_then(|o| {
                        o.tree.insert_bytes(&inner, bytes).map_err(|e| e.to_string())?;
                        o.dirty = true;
                        Ok(())
                    });
                    self.invalidate(&disk);
                    match res {
                        Ok(()) => self.status = format!("Updated {inner} — remember to Save the archive"),
                        Err(e) => self.error("Textures", &e),
                    }
                }
                TexSource::Disk(path) => {
                    let res = (|| -> Result<()> {
                        let mut b = path.as_os_str().to_owned();
                        b.push(".bak");
                        let b = PathBuf::from(b);
                        if !b.exists() {
                            fs::copy(&path, &b)?;
                        }
                        ops::write_atomic(&path, |w| {
                            w.write_all(&bytes)?;
                            Ok(bytes.len() as u64)
                        })?;
                        Ok(())
                    })();
                    self.cache.clear();
                    match res {
                        Ok(()) => self.status = format!("Saved {} (backup: .bak)", path.display()),
                        Err(e) => self.error("Textures", &format!("{e:#}")),
                    }
                }
            },
        }
    }

    // ---- binary metadata as XML ----------------------------------------

    /// Bytes of a file in the current location (small files only).
    fn selected_bytes(&mut self, name: &str) -> Option<Vec<u8>> {
        let rows = self.rows(&self.loc.clone()).ok()?;
        let row = rows.iter().find(|r| r.name == name)?;
        if row.kind != RowKind::File || row.size > 64 << 20 {
            return None;
        }
        match self.loc.inner.clone() {
            None => fs::read(self.loc.disk.join(name)).ok(),
            Some(i) => {
                let disk = self.loc.disk.clone();
                self.tree(&disk).ok()?.tree.read(&join(&i, name)).ok()
            }
        }
    }

    fn export_xml(&mut self, name: &str) {
        let Some(data) = self.selected_bytes(name) else { return };
        let (xml, conv) = match ragekit_core::convert::to_text(&data) {
            Ok(x) => x,
            Err(e) => return self.error("Export", &e.to_string()),
        };
        let Some(dest) = rfd::FileDialog::new().set_file_name(format!("{name}.{}", conv.ext())).save_file() else { return };
        match fs::write(&dest, xml) {
            Ok(()) => self.status = format!("Saved {}", dest.display()),
            Err(e) => self.error("Export as XML", &e.to_string()),
        }
    }

    fn import_xml(&mut self, name: &str) {
        let Some(original) = self.selected_bytes(name) else { return };
        let Some(src) = rfd::FileDialog::new().set_title(format!("XML or text to convert into {name}")).add_filter("XML / text", &["xml", "meta", "txt"]).pick_file() else { return };
        let xml = match fs::read_to_string(&src) {
            Ok(x) => x,
            Err(e) => return self.error("Replace from XML", &e.to_string()),
        };
        let bytes = match ragekit_core::convert::from_text(&xml, &original) {
            Ok(b) => b,
            Err(e) => return self.error("Replace from XML", &format!("{e:#}")),
        };
        let Some(path) = self.loc.inner_child(name) else { return };
        self.edit(|t| t.insert_bytes(&path, bytes).map(|_| format!("Replaced {name} from {} — remember to Save", src.display())));
    }

    // ---- preview ------------------------------------------------------

    fn update_preview(&mut self) {
        let single = if self.selected.len() == 1 { self.selected.iter().next().cloned() } else { None };
        let Some(name) = single else {
            self.preview = None;
            self.preview_for = None;
            return;
        };
        let key = (self.loc.clone(), name.clone());
        if self.preview_for.as_ref() == Some(&key) {
            return;
        }
        self.preview_for = Some(key);
        let rows = self.rows(&self.loc.clone()).unwrap_or_default();
        let Some(row) = rows.iter().find(|r| r.name == name).cloned() else { return };
        let mut lines = vec![format!("Type: {}", if row.type_desc.is_empty() { "—" } else { &row.type_desc })];
        if row.kind != RowKind::Folder {
            lines.push(format!("Size: {} ({} bytes)", human(row.size), row.size));
        }
        if !row.detail.is_empty() {
            lines.push(row.detail.clone());
        }
        if row.kind != RowKind::File || row.size > 64 << 20 {
            self.preview = Some(Preview::Info { name, lines });
            return;
        }
        let data = match &self.loc.inner {
            None => fs::read(self.loc.disk.join(&name)).map_err(|e| e.to_string()),
            Some(i) => {
                let p = join(i, &name);
                let disk = self.loc.disk.clone();
                match self.tree(&disk) {
                    Ok(o) => o.tree.read(&p).map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                }
            }
        };
        self.preview = Some(match data {
            Err(e) => Preview::Error(e),
            Ok(d) => {
                if ragekit_core::textures::is_ytd(&d) {
                    if let Ok(dict) = ragekit_core::textures::load(&d) {
                        lines.push(format!("{} textures{} · double-click to open the texture viewer", dict.textures.len(), if dict.enhanced { " (Enhanced)" } else { "" }));
                        for t in dict.textures.iter().take(200) {
                            lines.push(format!("  {}  {}x{} {}", t.name, t.width, t.height, ragekit_core::textures::format_name(t.format)));
                        }
                        self.preview = Some(Preview::Info { name, lines });
                        return;
                    }
                }
                if let Some(conv) = ragekit_core::convert::kind(&d) {
                    self.preview = Some(match ragekit_core::convert::to_text(&d) {
                        Ok((x, _)) => {
                            let truncated = x.len() > 2 << 20;
                            let text = x.chars().take(2 << 20).collect();
                            Preview::Text { name: format!("{name}  ({} shown as {})", conv.label(), if conv.ext() == "xml" { "XML" } else { "text" }), text, truncated }
                        }
                        Err(e) => Preview::Error(format!("{name}: {} file, but it couldn't be read: {e}", conv.label())),
                    });
                    return;
                }
                let looks_text = ops::is_text_name(&name) || (!d.is_empty() && d.iter().take(4096).all(|&b| b == b'\n' || b == b'\r' || b == b'\t' || (0x20..0x7F).contains(&b)));
                if looks_text {
                    let truncated = d.len() > 2 << 20;
                    let text = String::from_utf8_lossy(&d[..d.len().min(2 << 20)]).into_owned();
                    Preview::Text { name, text, truncated }
                } else {
                    Preview::Hex { name, dump: hexdump(&d[..d.len().min(8192)]), size: d.len() }
                }
            }
        });
    }

    // ---- jobs ---------------------------------------------------------

    fn start_job(&mut self, label: &str, f: impl FnOnce(Arc<Mutex<String>>) -> Result<JobDone> + Send + 'static) {
        if self.job.is_some() {
            self.status = "Wait for the current task to finish.".into();
            return;
        }
        let progress = Arc::new(Mutex::new(String::new()));
        let p2 = progress.clone();
        let handle = std::thread::spawn(move || f(p2));
        self.job = Some(Job { label: label.into(), progress, handle: Some(handle) });
    }

    fn poll_job(&mut self) {
        let Some(job) = &mut self.job else { return };
        if !job.handle.as_ref().is_some_and(|h| h.is_finished()) {
            return;
        }
        let res = job.handle.take().unwrap().join();
        let label = job.label.clone();
        self.job = None;
        match res {
            Err(_) => self.error(&label, "the task crashed"),
            Ok(Err(e)) => {
                if let Some(p) = &mut self.pkg_ui {
                    if matches!(p.stage, Stage::Planning | Stage::Installing) {
                        p.stage = Stage::Review;
                    }
                }
                self.refresh_all();
                if label == "Uninstalling" {
                    if let Some(id) = self.pending_uninstall.take() {
                        self.dialog = Dialog::ForceUninstall { id, body: format!("{e:#}") };
                        return;
                    }
                }
                self.error(&label, &format!("{e:#}"))
            }
            Ok(Ok(JobDone::Msg(m))) => self.status = m,
            Ok(Ok(JobDone::Saved { disk, msg })) => {
                self.opened.remove(&disk);
                self.invalidate(&disk);
                self.status = msg;
            }
            Ok(Ok(JobDone::Keys { keys, msg })) => {
                self.reload_keys(keys);
                self.dialog = Dialog::Message { title: "Keys".into(), body: msg };
            }
            Ok(Ok(JobDone::Plan(plan))) => {
                if let Some(p) = &mut self.pkg_ui {
                    p.stage = Stage::Planned(plan);
                }
            }
            Ok(Ok(JobDone::Installed(msg))) => {
                self.refresh_all();
                self.status = msg.clone();
                if let Some(p) = &mut self.pkg_ui {
                    p.stage = Stage::Done(msg);
                }
            }
        }
    }

    fn error(&mut self, title: &str, body: &str) {
        self.status = format!("{title}: {body}");
        self.dialog = Dialog::Message { title: title.to_string(), body: body.to_string() };
    }

    // ---- actions ------------------------------------------------------

    fn activate(&mut self, row: &DRow) {
        match row.kind {
            RowKind::Folder | RowKind::Archive => {
                let l = self.loc.child(&row.name, row.kind);
                self.go(l);
            }
            RowKind::File => {
                self.selected = [row.name.clone()].into();
                if row.name.to_ascii_lowercase().ends_with(".ytd") {
                    self.open_textures(&row.name.clone());
                }
            }
        }
    }

    fn extract_selected(&mut self, names: Vec<String>) {
        if names.is_empty() {
            return;
        }
        let Some(dest) = rfd::FileDialog::new().set_title("Extract to folder").pick_folder() else { return };
        let loc = self.loc.clone();
        let opts = ops::ExtractOpts { expand: self.settings.expand_nested, convert: self.settings.convert_on_extract };
        match &loc.inner {
            None => {
                // Loose files: copy.
                let mut n = 0;
                for name in &names {
                    let src = loc.disk.join(name);
                    if src.is_file() && fs::copy(&src, dest.join(name)).is_ok() {
                        n += 1;
                    }
                }
                self.status = format!("Copied {n} files to {}", dest.display());
            }
            Some(inner) => {
                let tree = match self.tree(&loc.disk) {
                    Ok(o) => o.tree.clone(),
                    Err(e) => return self.error("Extract", &format!("{e:#}")),
                };
                let inner = inner.clone();
                self.start_job("Extracting", move |prog| {
                    let (mut n, mut failed) = (0, Vec::new());
                    for name in &names {
                        let (c, f) = ops::extract_opts(&tree, &join(&inner, name), &dest.join(ops::safe_name(name)?), &opts, &mut |p| {
                            *prog.lock().unwrap() = p.to_string();
                        })?;
                        n += c;
                        failed.extend(f);
                    }
                    if failed.is_empty() {
                        Ok(JobDone::Msg(format!("Extracted {n} files to {}", dest.display())))
                    } else {
                        Err(anyhow!("Extracted {n} files; {} failed:\n{}", failed.len(), failed.join("\n")))
                    }
                });
            }
        }
    }

    fn edit(&mut self, f: impl FnOnce(&mut Tree) -> ragekit_core::Result<String>) {
        let disk = self.loc.disk.clone();
        let res = match self.tree(&disk) {
            Ok(o) => {
                let r = f(&mut o.tree);
                if r.is_ok() {
                    o.dirty = true;
                }
                r.map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        };
        self.invalidate(&disk);
        match res {
            Ok(m) => self.status = m,
            Err(e) => self.error("Edit", &e),
        }
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>, replace_target: Option<String>) {
        let Some(inner) = self.loc.inner.clone() else { return };
        let keys = self.keys.clone();
        self.edit(|t| {
            let mut n = 0;
            for p in &paths {
                let dest = match &replace_target {
                    Some(target) => target.clone(),
                    None => join(&inner, &p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()),
                };
                let fmt = t.format_at(&dest);
                let node = node_from_disk(p, fmt, &keys)?;
                t.insert_node(&dest, node)?;
                n += 1;
            }
            Ok(format!("{} {n} item(s) — remember to Save", if replace_target.is_some() { "Replaced" } else { "Added" }))
        });
    }

    fn save_current(&mut self) {
        let disk = self.loc.disk.clone();
        let Some(o) = self.opened.get(&disk) else { return };
        if !o.dirty {
            return;
        }
        let tree = o.tree.clone();
        self.start_job("Saving", move |prog| {
            *prog.lock().unwrap() = format!("Writing {}", disk.display());
            let (n, bak) = ops::save(&tree, &disk, true)?;
            let mut msg = format!("Saved {} ({})", disk.display(), human(n));
            if let Some(b) = bak {
                msg.push_str(&format!(" · backup at {}", b.display()));
            }
            Ok(JobDone::Saved { disk, msg })
        });
    }

    fn try_enable_edit(&mut self) {
        if self.loc.inner.is_none() {
            return;
        }
        if self.current_archive_writable() == Some(false) {
            self.error("Edit mode", "This archive format is read-only (GTA IV audio archives).");
            return;
        }
        // OpenIV-style: for GTA V, offer to work on a copy in the mods folder.
        if let (Some(root), Some(Game::GtaV)) = (&self.root, self.game) {
            let mods = root.join("mods");
            if !self.loc.disk.starts_with(&mods) {
                if let Ok(rel) = self.loc.disk.strip_prefix(root) {
                    self.dialog = Dialog::ModsCopy { archive: self.loc.disk.clone(), mods_target: mods.join(rel) };
                    return;
                }
            }
        }
        self.edit_mode = true;
    }

    fn start_search(&mut self) {
        let pattern = self.search_text.trim().to_string();
        if pattern.is_empty() {
            return;
        }
        if let Some(s) = &self.search {
            s.cancel.store(true, Ordering::Relaxed);
        }
        let root = self.root.clone().unwrap_or_else(|| self.loc.disk.clone());
        let results = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (r2, e2, c2, keys, p2) = (results.clone(), errors.clone(), cancel.clone(), self.keys.clone(), pattern.clone());
        let handle = std::thread::spawn(move || {
            vfs::search(&root, &p2, keys, &c2, &mut |h| r2.lock().unwrap().push(h), &mut |e| e2.lock().unwrap().push(e));
        });
        self.search = Some(Search { pattern, results, errors, cancel, handle: Some(handle), shown: true });
    }
}

fn hexdump(d: &[u8]) -> String {
    let mut s = String::with_capacity(d.len() * 4);
    for (i, chunk) in d.chunks(16).enumerate() {
        s.push_str(&format!("{:08x}  ", i * 16));
        for j in 0..16 {
            match chunk.get(j) {
                Some(b) => s.push_str(&format!("{b:02x} ")),
                None => s.push_str("   "),
            }
        }
        s.push(' ');
        for &b in chunk {
            s.push(if (0x20..0x7F).contains(&b) { b as char } else { '.' });
        }
        s.push('\n');
    }
    s
}

/// Draw a small icon for a row kind and return the label response space.
fn paint_icon(ui: &mut egui::Ui, kind: RowKind) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 14.0), egui::Sense::hover());
    let p = ui.painter();
    let r = rect.shrink2(egui::vec2(1.0, 1.5));
    match kind {
        RowKind::Folder => {
            let c = Color32::from_rgb(230, 185, 80);
            let tab = egui::Rect::from_min_size(r.min, egui::vec2(r.width() * 0.45, 3.0));
            p.rect_filled(tab, 1.0, c);
            p.rect_filled(egui::Rect::from_min_max(r.min + egui::vec2(0.0, 2.0), r.max), 1.5, c);
        }
        RowKind::Archive => {
            let c = Color32::from_rgb(196, 140, 90);
            p.rect_filled(r, 1.5, c);
            p.rect_filled(egui::Rect::from_min_max(r.min, egui::pos2(r.max.x, r.min.y + 3.5)), 1.0, Color32::from_rgb(150, 100, 60));
            p.rect_filled(egui::Rect::from_center_size(r.center() + egui::vec2(0.0, 1.0), egui::vec2(2.0, r.height() - 3.0)), 0.0, Color32::from_rgb(240, 220, 160));
        }
        RowKind::File => {
            let r = egui::Rect::from_center_size(r.center(), egui::vec2(9.0, r.height()));
            p.rect_filled(r, 1.0, Color32::from_gray(170));
            for k in 0..3 {
                let y = r.min.y + 3.5 + k as f32 * 2.8;
                p.line_segment([egui::pos2(r.min.x + 2.0, y), egui::pos2(r.max.x - 2.0, y)], egui::Stroke::new(1.0, Color32::from_gray(90)));
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        self.poll_job();
        if self.job.is_some() || self.search.as_ref().is_some_and(|s| s.handle.as_ref().is_some_and(|h| !h.is_finished())) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // Unsaved changes on close.
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty_count() > 0 && !self.allow_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dialog = Dialog::ConfirmQuit;
        }

        // Drag and drop into an archive in edit mode.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        let (pkgs, dropped): (Vec<PathBuf>, Vec<PathBuf>) =
            dropped.into_iter().partition(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("oiv")));
        if let Some(p) = pkgs.first() {
            self.open_package(p);
        }
        if !dropped.is_empty() {
            if self.edit_mode && self.loc.inner.is_some() {
                self.add_paths(dropped, None);
            } else if dropped.len() == 1 {
                self.open_path(&dropped[0]);
            } else {
                self.status = "Turn on Edit mode inside an archive to add files by dropping them.".into();
            }
        }

        // Keyboard shortcuts.
        let (back, up, ctrl_s, del, ctrl_f) = ctx.input(|i| {
            (
                i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::Backspace),
                i.modifiers.command && i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::Delete),
                i.modifiers.command && i.key_pressed(egui::Key::F),
            )
        });
        if back {
            if let Some(l) = self.back.pop() {
                self.forward.push(self.loc.clone());
                self.set_loc(l);
            }
        }
        if up && !ctx.egui_wants_keyboard_input() {
            if let Some(p) = self.loc.parent() {
                self.go(p);
            }
        }
        if ctrl_s && self.edit_mode {
            self.save_current();
        }
        let (f5, f2) = ctx.input(|i| (i.key_pressed(egui::Key::F5), i.key_pressed(egui::Key::F2)));
        if f5 {
            let d = self.loc.disk.clone();
            if !self.opened.get(&d).is_some_and(|o| o.dirty) {
                self.opened.remove(&d);
            }
            self.cache.clear();
            self.installs = ragekit_core::games::find_installs();
            self.status = "Refreshed".into();
        }
        if f2 && self.edit_mode && self.selected.len() == 1 && matches!(self.dialog, Dialog::None) {
            let name = self.selected.iter().next().unwrap().clone();
            if let Some(path) = self.loc.inner_child(&name) {
                self.dialog = Dialog::Rename { path, name };
            }
        }
        if del && self.edit_mode && !self.selected.is_empty() && matches!(self.dialog, Dialog::None) {
            let paths = self.selected.iter().filter_map(|n| self.loc.inner_child(n)).collect();
            self.dialog = Dialog::ConfirmDelete { paths };
        }

        self.top_bar(root, ctrl_f);
        self.status_bar(root);
        self.side_tree(root);
        self.update_preview();
        self.preview_panel(root);
        self.center(root);
        self.keys_ui(ctx);
        self.packages_ui(ctx);
        self.textures_ui(ctx);
        self.setup_ui(ctx);
        self.tools_ui(ctx);
        self.dialogs(ctx);
    }
}

impl App {
    fn top_bar(&mut self, root: &mut egui::Ui, focus_search: bool) {
        egui::Panel::top("toolbar").show(root, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Open game folder…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().set_title("Open game folder").pick_folder() {
                        self.open_path(&p);
                    }
                }
                if ui.button("Open archive…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Archives", &["rpf", "img"]).pick_file() {
                        self.open_path(&p);
                    }
                }
                ui.separator();
                ui.menu_button("Games", |ui| {
                    if self.installs.is_empty() {
                        ui.label(RichText::new("No installs found automatically").weak());
                    }
                    for i in self.installs.clone() {
                        if ui.button(i.label()).on_hover_text(i.path.display().to_string()).clicked() {
                            self.open_path(&i.path);
                            ui.close();
                        }
                    }
                    if ui.button("Search again").clicked() {
                        self.installs = ragekit_core::games::find_installs();
                    }
                    ui.separator();
                    ui.label(RichText::new("Recent").weak());
                    for r in self.settings.recent.clone().into_iter().filter(|r| r.exists()) {
                        let label = r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        if ui.button(label).on_hover_text(r.display().to_string()).clicked() {
                            self.open_path(&r);
                            ui.close();
                        }
                    }
                    ui.separator();
                    let v = self.game == Some(Game::GtaV);
                    if ui.add_enabled(v, egui::Button::new("Check mod setup…")).on_hover_text("ASI loader, mods-folder loader, Proton launch options").clicked() {
                        self.setup_window = true;
                        ui.close();
                    }
                });
                ui.menu_button("Tools", |ui| {
                    if ui.button("Add-on vehicle pack…").on_hover_text("Turn a folder of .yft/.ytd/.meta files into an installable DLC").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().set_title("Folder with the vehicle's files").pick_folder() {
                            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let scan = ragekit_core::dlc::scan(&dir, &name).map_err(|e| e.to_string());
                            self.addon = Some(AddonUi { folder: dir, name: ragekit_core::dlc::clean_name(&name), scan });
                        }
                        ui.close();
                    }
                    if ui.button("Hash calculator…").clicked() {
                        self.hash_window = Some(String::new());
                        ui.close();
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.checkbox(&mut self.settings.dark, "Dark theme").changed() {
                        self.apply_view();
                        self.settings.save();
                    }
                    ui.horizontal(|ui| {
                        ui.label("Text size");
                        for (l, z) in [("S", 0.85), ("M", 1.0), ("L", 1.2), ("XL", 1.4)] {
                            if ui.selectable_label((self.settings.ui_scale - z).abs() < 0.01, l).clicked() {
                                self.settings.ui_scale = z;
                                self.apply_view();
                                self.settings.save();
                            }
                        }
                    });
                });
                ui.menu_button("Packages", |ui| {
                    if ui.button("Install package (.oiv)…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("OpenIV package", &["oiv"]).pick_file() {
                            self.open_package(&p);
                        }
                        ui.close();
                    }
                    if ui.button("Installed packages…").clicked() {
                        self.installs_window = true;
                        ui.close();
                    }
                });
                let keys_label = if self.keys.has_ng() || self.keys.has_iv() { "Keys" } else { "Keys (setup needed)" };
                if ui.button(keys_label).clicked() {
                    self.keys_window = true;
                }
                if let Some(g) = self.game {
                    ui.separator();
                    ui.label(RichText::new(g.label()).strong());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let in_archive = self.loc.inner.is_some();
                    let mut edit = self.edit_mode;
                    let resp = ui.add_enabled(in_archive, egui::Checkbox::new(&mut edit, "Edit mode"));
                    if resp.changed() {
                        if edit {
                            self.try_enable_edit();
                        } else {
                            self.edit_mode = false;
                        }
                    }
                    resp.on_hover_text("Edit the archive you're in: add, replace, rename and delete files, then Save.");
                    ui.separator();
                    let r = ui.add(egui::TextEdit::singleline(&mut self.search_text).hint_text("Search all archives…  (Ctrl+F)").desired_width(220.0));
                    if focus_search {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.start_search();
                    }
                });
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.back.is_empty(), egui::Button::new("  <  ")).on_hover_text("Back").clicked() {
                    let l = self.back.pop().unwrap();
                    self.forward.push(self.loc.clone());
                    self.set_loc(l);
                }
                if ui.add_enabled(!self.forward.is_empty(), egui::Button::new("  >  ")).on_hover_text("Forward").clicked() {
                    let l = self.forward.pop().unwrap();
                    self.back.push(self.loc.clone());
                    self.set_loc(l);
                }
                if ui.add_enabled(self.loc.parent().is_some(), egui::Button::new(" Up ")).on_hover_text("Up").clicked() {
                    let p = self.loc.parent().unwrap();
                    self.go(p);
                }
                if ui.button("Refresh").on_hover_text("Refresh").clicked() {
                    let d = self.loc.disk.clone();
                    if !self.opened.get(&d).is_some_and(|o| o.dirty) {
                        self.opened.remove(&d);
                    }
                    self.invalidate(&d);
                    self.cache.clear();
                }
                let r = ui.add(egui::TextEdit::singleline(&mut self.address).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.go_address();
                }
            });
            ui.add_space(2.0);
            if self.edit_mode {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("EDIT MODE").color(Color32::from_rgb(255, 170, 60)).strong());
                    if ui.button("Add files…").clicked() {
                        if let Some(files) = rfd::FileDialog::new().set_title("Add files").pick_files() {
                            self.add_paths(files, None);
                        }
                    }
                    if ui.button("Add folder…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().set_title("Add folder").pick_folder() {
                            self.add_paths(vec![dir], None);
                        }
                    }
                    let one_file = self.selected.len() == 1;
                    if ui.add_enabled(one_file, egui::Button::new("Replace…")).clicked() {
                        let target = self.loc.inner_child(self.selected.iter().next().unwrap());
                        if let Some(f) = rfd::FileDialog::new().set_title("Replace with").pick_file() {
                            self.add_paths(vec![f], target);
                        }
                    }
                    if ui.button("New folder").clicked() {
                        self.dialog = Dialog::NewFolder { name: String::new() };
                    }
                    if ui.add_enabled(one_file, egui::Button::new("Rename")).clicked() {
                        let name = self.selected.iter().next().unwrap().clone();
                        self.dialog = Dialog::Rename { path: self.loc.inner_child(&name).unwrap(), name };
                    }
                    if ui.add_enabled(!self.selected.is_empty(), egui::Button::new("Delete")).clicked() {
                        let paths = self.selected.iter().filter_map(|n| self.loc.inner_child(n)).collect();
                        self.dialog = Dialog::ConfirmDelete { paths };
                    }
                    ui.separator();
                    let dirty = self.opened.get(&self.loc.disk).is_some_and(|o| o.dirty);
                    if ui.add_enabled(dirty && self.job.is_none(), egui::Button::new(RichText::new("Save").strong())).clicked() {
                        self.save_current();
                    }
                    if ui.add_enabled(dirty, egui::Button::new("Discard changes")).clicked() {
                        let d = self.loc.disk.clone();
                        self.opened.remove(&d);
                        self.invalidate(&d);
                        self.status = "Changes discarded".into();
                    }
                    ui.label(RichText::new("Drop files here to add them").weak());
                });
                ui.add_space(2.0);
            }
        });
    }

    fn status_bar(&mut self, root: &mut egui::Ui) {
        egui::Panel::bottom("status").show(root, |ui| {
            ui.horizontal(|ui| {
                if let Some(job) = &self.job {
                    ui.spinner();
                    let p = job.progress.lock().unwrap().clone();
                    ui.label(format!("{}… {}", job.label, p));
                } else {
                    ui.label(&self.status);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !self.selected.is_empty() {
                        if let Ok(rows) = self.cache.get(&self.loc).cloned().unwrap_or(Ok(vec![])) {
                            let total: u64 = rows.iter().filter(|r| self.selected.contains(&r.name)).map(|r| r.size).sum();
                            ui.label(RichText::new(format!("{} selected · {}", self.selected.len(), human(total))).weak());
                            ui.separator();
                        }
                    }
                    let d = self.dirty_count();
                    if d > 0 {
                        ui.label(RichText::new(format!("{d} unsaved archive(s)")).color(Color32::from_rgb(255, 170, 60)));
                    }
                    if let Some(o) = self.opened.get(&self.loc.disk) {
                        if self.loc.inner.is_some() {
                            ui.label(RichText::new(format!("{} · {}", o.tree.format.game(), o.tree.origin)).weak());
                        }
                    }
                });
            });
        });
    }

    fn side_tree(&mut self, root: &mut egui::Ui) {
        egui::Panel::left("tree").resizable(true).default_size(260.0).min_size(160.0).show(root, |ui| {
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                let Some(root) = self.root.clone() else {
                    ui.label("No folder open");
                    return;
                };
                let label = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
                self.tree_node(ui, Loc::disk(root), &label, RowKind::Folder, 0);
            });
            self.sync_tree = false;
        });
    }

    fn tree_node(&mut self, ui: &mut egui::Ui, loc: Loc, label: &str, kind: RowKind, depth: usize) {
        if depth > 24 {
            return;
        }
        let id = ui.make_persistent_id(("tree", &loc));
        let is_current = loc == self.loc;
        let (cur, me) = (self.loc.display(), loc.display());
        let on_path = cur.starts_with(&format!("{me}/")) || (cur == me && kind == RowKind::Archive);
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, depth == 0);
        if self.sync_tree && on_path && !state.is_open() {
            // Reveal the path to the current location.
            state.set_open(true);
        }
        let header = state.show_header(ui, |ui| {
            paint_icon(ui, kind);
            let text = RichText::new(label);
            let text = if is_current { text.strong().color(ui.visuals().selection.stroke.color) } else { text };
            if ui.add(egui::Label::new(text).sense(egui::Sense::click()).truncate()).clicked() {
                self.go(loc.clone());
            }
        });
        header.body(|ui| {
            let rows = match self.rows(&loc) {
                Ok(r) => r,
                Err(e) => {
                    ui.label(RichText::new(e).color(Color32::LIGHT_RED).small());
                    return;
                }
            };
            for r in rows.iter().filter(|r| r.kind != RowKind::File) {
                let child = loc.child(&r.name, r.kind);
                self.tree_node(ui, child, &r.name, r.kind, depth + 1);
            }
        });
    }

    fn preview_panel(&mut self, root: &mut egui::Ui) {
        egui::Panel::right("preview").resizable(true).default_size(380.0).min_size(300.0).show(root, |ui| {
            match &self.preview {
                None => {
                    ui.add_space(8.0);
                    if self.selected.len() > 1 {
                        ui.label(format!("{} items selected", self.selected.len()));
                    } else {
                        ui.label(RichText::new("Select a file to preview it.").weak());
                    }
                }
                Some(Preview::Error(e)) => {
                    ui.colored_label(Color32::LIGHT_RED, e);
                }
                Some(Preview::Info { name, lines }) => {
                    ui.heading(name);
                    for l in lines {
                        ui.label(l);
                    }
                }
                Some(Preview::Text { name, text, truncated }) => {
                    ui.heading(name);
                    if *truncated {
                        ui.label(RichText::new("Showing the first 2 MB").weak());
                    }
                    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(text.as_str()).monospace().size(12.0)).extend());
                    });
                }
                Some(Preview::Hex { name, dump, size }) => {
                    ui.heading(name);
                    ui.label(RichText::new(format!("{} · showing the first {} bytes", human(*size as u64), (*size).min(8192))).weak());
                    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(dump).monospace().size(11.0)).extend());
                    });
                }
            }
        });
    }

    fn center(&mut self, root: &mut egui::Ui) {
        egui::CentralPanel::default().show(root, |ui| {
            if let Some(s) = &self.search {
                if s.shown {
                    self.search_results(ui);
                    return;
                }
            }
            let loc = self.loc.clone();
            let rows = match self.rows(&loc) {
                Ok(r) => r,
                Err(e) => {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.colored_label(Color32::LIGHT_RED, &e);
                        if e.contains("missing key") {
                            ui.add_space(8.0);
                            if ui.button("Set up keys…").clicked() {
                                self.keys_window = true;
                            }
                        }
                    });
                    return;
                }
            };
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter this folder").desired_width(200.0));
                ui.label(RichText::new(format!("{} items", rows.len())).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let sel: Vec<String> = self.selected.iter().cloned().collect();
                    if ui.add_enabled(!sel.is_empty(), egui::Button::new("Extract selected…")).clicked() {
                        self.extract_selected(sel);
                    }
                    if self.loc.inner.is_some() && ui.button("Extract all…").clicked() {
                        let all = rows.iter().map(|r| r.name.clone()).collect();
                        self.extract_selected(all);
                    }
                    let a = ui.checkbox(&mut self.settings.expand_nested, "Unpack nested archives");
                    let b = ui.checkbox(&mut self.settings.convert_on_extract, "Also save as XML/text")
                        .on_hover_text("Binary metadata (.ymt, .ytyp, .ymap…) also gets an .xml copy, .gxt2 a .txt copy");
                    if a.changed() || b.changed() {
                        self.settings.save();
                    }
                });
            });
            let filter = self.filter.to_lowercase();
            let mut view: Vec<&DRow> = rows.iter().filter(|r| filter.is_empty() || r.name.to_lowercase().contains(&filter)).collect();
            let (col, asc) = self.sort;
            view.sort_by(|a, b| {
                let folders = (a.kind == RowKind::File).cmp(&(b.kind == RowKind::File));
                let o = match col {
                    SortCol::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                    SortCol::Size => a.size.cmp(&b.size),
                    SortCol::Type => a.type_desc.cmp(&b.type_desc).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                };
                folders.then(if asc { o } else { o.reverse() })
            });
            let mut activate: Option<DRow> = None;

            // Keyboard: arrows move, Enter opens, Ctrl+A selects all.
            if !ui.ctx().egui_wants_keyboard_input() && matches!(self.dialog, Dialog::None) && !view.is_empty() {
                let (down, upk, enter, all) = ui.input(|i| {
                    (i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::Enter), i.modifiers.command && i.key_pressed(egui::Key::A))
                });
                let cur = self.anchor.filter(|&a| a < view.len());
                if down || upk {
                    let n = match cur {
                        None => 0,
                        Some(c) if down => (c + 1).min(view.len() - 1),
                        Some(c) => c.saturating_sub(1),
                    };
                    self.selected = [view[n].name.clone()].into();
                    self.anchor = Some(n);
                    self.scroll_to = Some(n);
                }
                if enter {
                    if let Some(c) = cur {
                        activate = Some(view[c].clone());
                    }
                }
                if all {
                    self.selected = view.iter().map(|r| r.name.clone()).collect();
                }
            }
            let mut table = TableBuilder::new(ui)
                .striped(true)
                .sense(egui::Sense::click());
            if let Some(i) = self.scroll_to.take() {
                table = table.scroll_to_row(i, None);
            }
            let mut sort_click: Option<SortCol> = None;
            let arrow = |c: SortCol| if col == c { if asc { " ^" } else { " v" } } else { "" };
            table
                .column(Column::remainder().at_least(220.0).clip(true))
                .column(Column::exact(90.0))
                .column(Column::initial(180.0).clip(true))
                .column(Column::remainder().at_least(80.0).clip(true))
                .header(20.0, |mut h| {
                    for (c, label) in [(SortCol::Name, "Name"), (SortCol::Size, "Size"), (SortCol::Type, "Type")] {
                        h.col(|ui| {
                            if ui.add(egui::Label::new(RichText::new(format!("{label}{}", arrow(c))).strong()).sense(egui::Sense::click())).on_hover_text("Sort").clicked() {
                                sort_click = Some(c);
                            }
                        });
                    }
                    h.col(|ui| {
                        ui.strong("Details");
                    });
                })
                .body(|body| {
                    body.rows(20.0, view.len(), |mut row| {
                        let i = row.index();
                        let r = view[i];
                        row.set_selected(self.selected.contains(&r.name));
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                paint_icon(ui, r.kind);
                                ui.add(egui::Label::new(&r.name).selectable(false).truncate());
                            });
                        });
                        row.col(|ui| {
                            if r.kind != RowKind::Folder {
                                ui.label(human(r.size));
                            }
                        });
                        row.col(|ui| {
                            ui.add(egui::Label::new(RichText::new(&r.type_desc).weak()).truncate());
                        });
                        row.col(|ui| {
                            ui.add(egui::Label::new(RichText::new(&r.detail).weak()).truncate());
                        });
                        let resp = row.response();
                        if resp.double_clicked() {
                            activate = Some(r.clone());
                        } else if resp.clicked() {
                            let mods = resp.ctx.input(|i| i.modifiers);
                            if mods.command {
                                if !self.selected.remove(&r.name) {
                                    self.selected.insert(r.name.clone());
                                }
                                self.anchor = Some(i);
                            } else if mods.shift && self.anchor.is_some() {
                                let a = self.anchor.unwrap();
                                let (lo, hi) = (a.min(i), a.max(i));
                                self.selected = view[lo..=hi.min(view.len() - 1)].iter().map(|r| r.name.clone()).collect();
                            } else {
                                self.selected = [r.name.clone()].into();
                                self.anchor = Some(i);
                            }
                        }
                        if resp.secondary_clicked() && !self.selected.contains(&r.name) {
                            self.selected = [r.name.clone()].into();
                        }
                        resp.context_menu(|ui| self.menu_items(ui));
                    });
                });
            if let Some(c) = sort_click {
                self.sort = if self.sort.0 == c { (c, !self.sort.1) } else { (c, true) };
            }
            if let Some(r) = activate {
                self.activate(&r);
            }
        });
    }

    fn menu_items(&mut self, ui: &mut egui::Ui) {
        {
            let sel: Vec<String> = self.selected.iter().cloned().collect();
            if ui.add_enabled(!sel.is_empty(), egui::Button::new("Extract…")).clicked() {
                self.extract_selected(sel.clone());
                ui.close();
            }
            let conv = if sel.len() == 1 { self.selected_bytes(&sel[0]).and_then(|d| ragekit_core::convert::kind(&d)) } else { None };
            let is_meta = conv.is_some();
            let tname = conv.map(|c| if c.ext() == "xml" { "XML" } else { "text" }).unwrap_or("XML");
            let is_file = sel.len() == 1 && self.cache.get(&self.loc).and_then(|r| r.as_ref().ok()).is_some_and(|rows| rows.iter().any(|r| r.name == sel[0] && r.kind == RowKind::File));
            if is_file && ui.button("Open with default app").clicked() {
                self.open_external(&sel[0].clone(), false);
                ui.close();
            }
            if is_meta && ui.button(format!("Open as {tname} in editor")).clicked() {
                self.open_external(&sel[0].clone(), true);
                ui.close();
            }
            if self.loc.inner.is_none() && ui.button("Show in file manager").clicked() {
                let _ = std::process::Command::new("xdg-open").arg(&self.loc.disk).spawn();
                ui.close();
            }
            if sel.len() == 1 && sel[0].to_ascii_lowercase().ends_with(".ytd") && ui.button("Open texture viewer").clicked() {
                self.open_textures(&sel[0].clone());
                ui.close();
            }
            if is_meta && ui.button(format!("Export as {tname}…")).clicked() {
                self.export_xml(&sel[0]);
                ui.close();
            }
            if is_meta && self.edit_mode && ui.button(format!("Replace from {tname}…")).clicked() {
                self.import_xml(&sel[0]);
                ui.close();
            }
            if ui.add_enabled(sel.len() == 1, egui::Button::new("Copy path")).clicked() {
                let p = match self.loc.inner_child(&sel[0]) {
                    Some(i) => format!("{}/{}", self.loc.disk.display(), i),
                    None => self.loc.disk.join(&sel[0]).display().to_string(),
                };
                ui.ctx().copy_text(p);
                ui.close();
            }
            if self.edit_mode {
                ui.separator();
                if ui.add_enabled(sel.len() == 1, egui::Button::new("Replace…")).clicked() {
                    let target = self.loc.inner_child(&sel[0]);
                    if let Some(f) = rfd::FileDialog::new().pick_file() {
                        self.add_paths(vec![f], target);
                    }
                    ui.close();
                }
                if ui.add_enabled(sel.len() == 1, egui::Button::new("Rename…")).clicked() {
                    self.dialog = Dialog::Rename { path: self.loc.inner_child(&sel[0]).unwrap(), name: sel[0].clone() };
                    ui.close();
                }
                if ui.add_enabled(!sel.is_empty(), egui::Button::new("Delete")).clicked() {
                    self.dialog = Dialog::ConfirmDelete { paths: sel.iter().filter_map(|n| self.loc.inner_child(n)).collect() };
                    ui.close();
                }
            }
        }
    }

    fn search_results(&mut self, ui: &mut egui::Ui) {
        let s = self.search.as_ref().unwrap();
        let running = s.handle.as_ref().is_some_and(|h| !h.is_finished());
        let results = s.results.lock().unwrap().clone();
        let nerr = s.errors.lock().unwrap().len();
        let pattern = s.pattern.clone();
        let mut close = false;
        let mut goto: Option<Hit> = None;
        ui.horizontal(|ui| {
            if running {
                ui.spinner();
            }
            ui.heading(format!("Search: “{pattern}” — {} result(s)", results.len()));
            if nerr > 0 {
                ui.label(RichText::new(format!("({nerr} archives skipped)")).weak())
                    .on_hover_text(self.search.as_ref().unwrap().errors.lock().unwrap().join("\n"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close results").clicked() {
                    close = true;
                }
                if running && ui.button("Stop").clicked() {
                    self.search.as_ref().unwrap().cancel.store(true, Ordering::Relaxed);
                }
            });
        });
        let root = self.root.clone();
        TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .column(Column::remainder().clip(true))
            .body(|body| {
                body.rows(20.0, results.len(), |mut row| {
                    let h = &results[row.index()];
                    let f = root.as_ref().and_then(|r| h.file.strip_prefix(r).ok()).unwrap_or(&h.file).display().to_string();
                    row.col(|ui| {
                        let text = if h.inner.is_empty() { f } else { format!("{f}/{}", h.inner) };
                        ui.add(egui::Label::new(text).selectable(false).truncate());
                    });
                    if row.response().double_clicked() || row.response().clicked() {
                        goto = Some(h.clone());
                    }
                });
            });
        if let Some(h) = goto {
            let (parent, name) = if h.inner.is_empty() {
                (Loc::disk(h.file.parent().unwrap_or(Path::new("/")).to_path_buf()), h.file.file_name().unwrap().to_string_lossy().into_owned())
            } else {
                let (p, n) = h.inner.rsplit_once('/').unwrap_or(("", &h.inner));
                (Loc { disk: h.file.clone(), inner: Some(p.to_string()) }, n.to_string())
            };
            self.go(parent);
            // The case shown in listings may differ from the search hit.
            let rows = self.rows(&self.loc.clone()).unwrap_or_default();
            let actual = rows.iter().find(|r| r.name.eq_ignore_ascii_case(&name)).map(|r| r.name.clone()).unwrap_or(name);
            self.selected = [actual].into();
            if let Some(s) = &mut self.search {
                s.shown = false;
            }
        }
        if close {
            if let Some(s) = &self.search {
                s.cancel.store(true, Ordering::Relaxed);
            }
            self.search = None;
        }
    }

    fn keys_ui(&mut self, ctx: &egui::Context) {
        if !self.keys_window {
            return;
        }
        let mut open = true;
        egui::Window::new("Keys").open(&mut open).collapsible(false).resizable(false).default_width(460.0).anchor(egui::Align2::CENTER_TOP, [0.0, 90.0]).show(ctx, |ui| {
            ui.label("Rockstar's decryption keys aren't included. ragekit reads them from your own copy of the game.");
            ui.add_space(6.0);
            let k = &self.keys;
            let mark = |b: bool| if b { RichText::new("found").color(Color32::LIGHT_GREEN) } else { RichText::new("missing").color(Color32::LIGHT_RED) };
            egui::Grid::new("keys").num_columns(2).spacing([20.0, 4.0]).show(ui, |ui| {
                ui.label("GTA V AES key");
                ui.label(mark(k.has_aes()));
                ui.end_row();
                ui.label("GTA V NG keys (most archives)");
                ui.label(mark(k.has_ng()));
                ui.end_row();
                ui.label("GTA IV / EFLC key");
                ui.label(mark(k.has_iv()));
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.label(RichText::new(format!("Stored in {}", self.keys_dir.display())).weak().small());
            ui.separator();

            let root_exe = self.root.as_ref().and_then(|r| vfs::detect_game(r)).map(|g| g.1);
            if ui.add_enabled(root_exe.is_some(), egui::Button::new("Find keys in the open game's .exe")).clicked() {
                let exe = root_exe.clone().unwrap();
                self.keys_job(move |k| {
                    let rep = k.search_bytes(&fs::read(&exe)?);
                    Ok(format!(
                        "Searched {}.\nGTA V AES: {}   GTA IV: {}",
                        exe.display(),
                        if rep.aes { "found" } else { "not found" },
                        if rep.iv_aes { "found" } else { "not found" }
                    ))
                });
            }
            if ui.button("Choose a game .exe…").clicked() {
                if let Some(exe) = rfd::FileDialog::new().add_filter("Game executable", &["exe"]).pick_file() {
                    self.keys_job(move |k| {
                        let rep = k.search_bytes(&fs::read(&exe)?);
                        Ok(format!("GTA V AES: {}   GTA IV: {}", rep.aes, rep.iv_aes))
                    });
                }
            }
            if ui.button("Scan the running game (GTA V NG keys)").clicked() {
                match ops::find_game_pid() {
                    None => self.error("Keys", "No running GTA V found. Start the game, then try again."),
                    Some(pid) => self.keys_job(move |k| {
                        let rep = k.search_process(pid)?;
                        Ok(format!("NG keys {}/101, NG tables {}/272", rep.ng_keys_found, rep.ng_tables_found))
                    }),
                }
            }
            ui.label(
                RichText::new("Reading another program's memory usually needs root. If the scan fails, run:\nsudo ragekit keys extract --running")
                    .weak()
                    .small(),
            );
            if ui.button("Load CodeWalker's magic.dat…").clicked() {
                if let Some(f) = rfd::FileDialog::new().add_filter("magic.dat", &["dat"]).pick_file() {
                    self.keys_job(move |k| {
                        k.load_codewalker_magic(&fs::read(&f)?)?;
                        Ok("Loaded GTA V NG keys from magic.dat".into())
                    });
                }
            }
            if ui.button("Import a CodeWalker / GTAUtil keys folder…").clicked() {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    self.keys_job(move |k| {
                        k.merge(Keys::load_dir(&d)?);
                        Ok(format!("Imported keys from {}", d.display()))
                    });
                }
            }
        });
        if !open {
            self.keys_window = false;
        }
    }

    fn keys_job(&mut self, f: impl FnOnce(&mut Keys) -> Result<String> + Send + 'static) {
        let mut keys = (*self.keys).clone();
        let dir = self.keys_dir.clone();
        self.start_job("Finding keys", move |prog| {
            *prog.lock().unwrap() = "searching".into();
            let msg = f(&mut keys)?;
            keys.verify()?;
            keys.save_dir(&dir)?;
            Ok(JobDone::Keys { keys, msg })
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let mut action: Option<Box<dyn FnOnce(&mut App)>> = None;
        let modal = |title: &str| egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]);
        match &mut self.dialog {
            Dialog::None => {}
            Dialog::Message { title, body } => {
                modal(title).show(ctx, |ui| {
                    ui.set_max_width(520.0);
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| ui.label(body.as_str()));
                    if ui.button("OK").clicked() {
                        close = true;
                    }
                });
            }
            Dialog::Rename { path, name } => {
                modal("Rename").show(ctx, |ui| {
                    let r = ui.text_edit_singleline(name);
                    if !r.has_focus() {
                        r.request_focus();
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Rename").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let (p, n) = (path.clone(), name.clone());
                            action = Some(Box::new(move |a: &mut App| a.edit(|t| t.rename(&p, &n).map(|_| format!("Renamed to {n} — remember to Save")))));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            }
            Dialog::NewFolder { name } => {
                modal("New folder").show(ctx, |ui| {
                    let r = ui.text_edit_singleline(name);
                    if !r.has_focus() {
                        r.request_focus();
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Create").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let n = name.clone();
                            action = Some(Box::new(move |a: &mut App| {
                                if let Some(p) = a.loc.inner_child(&n) {
                                    a.edit(|t| t.mkdir(&p).map(|_| format!("Created {n} — remember to Save")));
                                }
                            }));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            }
            Dialog::ConfirmDelete { paths } => {
                modal("Delete").show(ctx, |ui| {
                    ui.label(format!("Delete {} item(s) from the archive?", paths.len()));
                    for p in paths.iter().take(8) {
                        ui.label(RichText::new(p.as_str()).monospace().small());
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            let ps = paths.clone();
                            action = Some(Box::new(move |a: &mut App| {
                                a.edit(|t| {
                                    for p in &ps {
                                        t.remove(p)?;
                                    }
                                    Ok(format!("Deleted {} item(s) — remember to Save", ps.len()))
                                });
                                a.selected.clear();
                            }));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            }
            Dialog::ModsCopy { archive, mods_target } => {
                modal("Edit a copy in the mods folder?").show(ctx, |ui| {
                    ui.set_max_width(520.0);
                    ui.label("Editing the game's original archives can break the game or online play. Mod loaders (OpenIV.asi, OpenRPF, RageOpenV) load edited copies from the mods folder instead.");
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("Copy to {}", mods_target.display())).monospace().small());
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Copy to mods and edit").strong()).clicked() {
                            let (src, dst) = (archive.clone(), mods_target.clone());
                            action = Some(Box::new(move |a: &mut App| {
                                let inner = a.loc.inner.clone();
                                let res = (|| -> Result<()> {
                                    if !dst.exists() {
                                        fs::create_dir_all(dst.parent().unwrap())?;
                                        fs::copy(&src, &dst)?;
                                    }
                                    Ok(())
                                })();
                                match res {
                                    Ok(()) => {
                                        a.go(Loc { disk: dst.clone(), inner });
                                        a.edit_mode = true;
                                        a.cache.clear();
                                        a.status = format!("Editing {}", dst.display());
                                    }
                                    Err(e) => a.error("Copy to mods", &format!("{e:#}")),
                                }
                            }));
                            close = true;
                        }
                        if ui.button("Edit the original").clicked() {
                            action = Some(Box::new(|a: &mut App| a.edit_mode = true));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            }
            Dialog::ForceUninstall { id, body } => {
                modal("Can't uninstall cleanly").show(ctx, |ui| {
                    ui.set_max_width(520.0);
                    ui.label(body.as_str());
                    ui.horizontal(|ui| {
                        if ui.button("Uninstall anyway").clicked() {
                            let id = id.clone();
                            action = Some(Box::new(move |a: &mut App| a.uninstall(id, true)));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            }
            Dialog::ConfirmQuit => {
                modal("Unsaved changes").show(ctx, |ui| {
                    ui.label(format!("{} archive(s) have unsaved changes.", self.opened.values().filter(|o| o.dirty).count()));
                    ui.horizontal(|ui| {
                        if ui.button("Quit without saving").clicked() {
                            action = Some(Box::new(|a: &mut App| {
                                a.allow_quit = true;
                            }));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            }
        }
        if close {
            self.dialog = Dialog::None;
        }
        if let Some(a) = action {
            a(self);
        }
    }
}
