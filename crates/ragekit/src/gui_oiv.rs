//! Package (.oiv) install dialog and installed-packages window.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use ragekit_core::oiv::{self, Level, Options, Package, Plan};

pub enum Stage {
    Review,
    Planning,
    Planned(Box<Plan>),
    Installing,
    Done(String),
}

pub struct PkgUi {
    pub pkg: Arc<Package>,
    pub icon: Option<egui::TextureHandle>,
    pub use_mods: bool,
    pub skip_failed: bool,
    pub stage: Stage,
}

/// What the dialog asks the app to do.
pub enum PkgAction {
    None,
    Close,
    Plan { pkg: Arc<Package>, opts: Options },
    Commit { pkg: Arc<Package>, plan: Box<Plan>, allow_errors: bool },
    OpenGameFolder,
}

fn argb(c: u32) -> Color32 {
    Color32::from_rgba_unmultiplied((c >> 16) as u8, (c >> 8) as u8, c as u8, 255)
}

impl PkgUi {
    pub fn new(ctx: &egui::Context, pkg: Package) -> PkgUi {
        let icon = pkg.icon.as_ref().and_then(|png| {
            let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?.to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let ci = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
            Some(ctx.load_texture("oiv-icon", ci, egui::TextureOptions::LINEAR))
        });
        PkgUi { pkg: Arc::new(pkg), icon, use_mods: true, skip_failed: false, stage: Stage::Review }
    }

    pub fn show(&mut self, ctx: &egui::Context, game: Option<(PathBuf, ragekit_core::vfs::Game)>) -> PkgAction {
        let mut action = PkgAction::None;
        let mut open = true;
        let m = self.pkg.meta.clone();
        // Never taller or wider than the screen, so the buttons stay reachable.
        let screen = ctx.content_rect();
        let max_h = (screen.height() - 40.0).max(240.0);
        let max_w = (screen.width() - 24.0).max(320.0);
        egui::Window::new("Install package")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([620.0_f32.min(max_w), 520.0_f32.min(max_h)])
            .max_height(max_h)
            .max_width(max_w)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                // Header band in the package's colors.
                let bg = m.header_color.map(argb).unwrap_or(Color32::from_rgb(39, 39, 39));
                let fg = if m.header_black_text { Color32::BLACK } else { Color32::WHITE };
                egui::Frame::new().fill(bg).inner_margin(10.0).corner_radius(4.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let icon_bg = m.icon_color.map(argb).unwrap_or(Color32::from_gray(46));
                        egui::Frame::new().fill(icon_bg).corner_radius(4.0).show(ui, |ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(72.0, 72.0), egui::Sense::hover());
                            if let Some(t) = &self.icon {
                                ui.painter().image(t.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                            } else {
                                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "OIV", egui::FontId::proportional(20.0), Color32::from_gray(150));
                            }
                        });
                        ui.vertical(|ui| {
                            ui.label(RichText::new(if m.name.is_empty() { "Unnamed package" } else { &m.name }).size(22.0).strong().color(fg));
                            ui.label(RichText::new(format!("Version {}", if m.version.is_empty() { "—" } else { &m.version })).color(fg));
                            if !m.author.is_empty() {
                                ui.label(RichText::new(format!("by {}", m.author)).color(fg));
                            }
                            ui.horizontal(|ui| {
                                for (k, url) in &m.author_links {
                                    if url.starts_with("http") {
                                        ui.hyperlink_to(RichText::new(k).color(fg).underline(), url);
                                    }
                                }
                            });
                        });
                    });
                });
                ui.add_space(6.0);

                match &mut self.stage {
                    Stage::Review => {
                        // Leave room below for the install options and buttons.
                        let desc_h = (ui.available_height() - 150.0).clamp(60.0, 180.0);
                        egui::ScrollArea::vertical().max_height(desc_h).show(ui, |ui| {
                            if !m.description.is_empty() {
                                ui.label(m.description.trim());
                            }
                            if !m.large_description.is_empty() {
                                ui.add_space(4.0);
                                ui.label(m.large_description.trim());
                            }
                            if !m.license.is_empty() {
                                ui.add_space(4.0);
                                ui.label(RichText::new(m.license.trim()).weak());
                            }
                        });
                        ui.separator();
                        let pkg_game = if m.is_gta5() { "GTA V" } else { "GTA IV" };
                        ui.label(format!(
                            "For {pkg_game}{} · {} steps",
                            m.game_version.as_ref().map(|g| format!(" ({g})")).unwrap_or_default(),
                            oiv::Op::count(&self.pkg.ops)
                        ));
                        match &game {
                            None => {
                                ui.colored_label(Color32::from_rgb(255, 170, 60), "Open your game folder first, so ragekit knows where to install.");
                                if ui.button("Open game folder…").clicked() {
                                    action = PkgAction::OpenGameFolder;
                                }
                            }
                            Some((dir, g)) => {
                                ui.label(format!("Install into: {} ({})", dir.display(), g.label()));
                                if *g == ragekit_core::vfs::Game::GtaV {
                                    ui.checkbox(&mut self.use_mods, "Use the mods folder (recommended: the game's own files stay untouched)");
                                } else {
                                    ui.label(RichText::new("GTA IV has no mods folder: files are changed in place, with backups so you can uninstall.").weak());
                                }
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    if ui.button(RichText::new("Check & install…").strong()).clicked() {
                                        action = PkgAction::Plan {
                                            pkg: self.pkg.clone(),
                                            opts: Options { game_dir: dir.clone(), use_mods: self.use_mods },
                                        };
                                    }
                                    ui.label(RichText::new("Nothing is changed until you confirm.").weak());
                                });
                            }
                        }
                    }
                    Stage::Planning => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Checking the package against your game…");
                        });
                    }
                    Stage::Planned(plan) => {
                        let (e, w) = (plan.errors(), plan.warnings());
                        let summary = if e > 0 {
                            RichText::new(format!("{e} error(s), {w} warning(s)")).color(Color32::LIGHT_RED)
                        } else if w > 0 {
                            RichText::new(format!("Ready, with {w} warning(s)")).color(Color32::from_rgb(255, 200, 80))
                        } else {
                            RichText::new("Ready to install").color(Color32::LIGHT_GREEN)
                        };
                        ui.label(summary.strong());
                        // The log and file list can be long: scroll them together and keep
                        // the checkbox and buttons below always on screen.
                        let reserve = if e > 0 { 70.0 } else { 44.0 };
                        let list_h = (ui.available_height() - reserve).max(80.0);
                        egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, true]).show(ui, |ui| {
                            for l in &plan.log {
                                let (c, tag) = match l.level {
                                    Level::Info => (ui.visuals().text_color(), ""),
                                    Level::Warn => (Color32::from_rgb(255, 200, 80), "warning: "),
                                    Level::Error => (Color32::LIGHT_RED, "error: "),
                                };
                                ui.label(RichText::new(format!("{tag}{}", l.msg)).color(c).monospace().size(11.5));
                            }
                            ui.separator();
                            let changes = plan.changes();
                            ui.label(format!("Files that will change ({}):", changes.len()));
                            for (f, write) in changes {
                                ui.label(RichText::new(format!("{} {f}", if write { "write " } else { "delete" })).monospace().size(11.5));
                            }
                        });
                        ui.separator();
                        if e > 0 {
                            ui.checkbox(&mut self.skip_failed, "Install anyway, skipping the failed steps");
                        }
                        ui.horizontal(|ui| {
                            let ok = e == 0 || self.skip_failed;
                            if ui.add_enabled(ok, egui::Button::new(RichText::new("Install").strong())).clicked() {
                                if let Stage::Planned(p) = std::mem::replace(&mut self.stage, Stage::Installing) {
                                    action = PkgAction::Commit { pkg: self.pkg.clone(), plan: p, allow_errors: self.skip_failed };
                                }
                            }
                            if ui.button("Back").clicked() {
                                self.stage = Stage::Review;
                            }
                        });
                    }
                    Stage::Installing => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Installing…");
                        });
                    }
                    Stage::Done(msg) => {
                        ui.label(RichText::new(msg.as_str()).color(Color32::LIGHT_GREEN));
                        if ui.button("Close").clicked() {
                            action = PkgAction::Close;
                        }
                    }
                }
            });
        if !open && !matches!(self.stage, Stage::Installing) {
            action = PkgAction::Close;
        }
        action
    }
}

/// The "Installed packages" window. Returns an id to uninstall (and force flag).
pub fn installed_window(ctx: &egui::Context, open: &mut bool, game: Option<&std::path::Path>) -> Option<(String, bool)> {
    let mut out = None;
    egui::Window::new("Installed packages").open(open).default_size([560.0, 300.0]).anchor(egui::Align2::CENTER_TOP, [0.0, 90.0]).show(ctx, |ui| {
        let Some(game) = game else {
            ui.label("Open a game folder to see its packages.");
            return;
        };
        let list = oiv::list(game);
        if list.is_empty() {
            ui.label(RichText::new("No packages have been installed with ragekit here.").weak());
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for r in list.iter().rev() {
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(format!("{} {}", r.package_name, r.package_version)).strong());
                            ui.label(RichText::new(format!("{} file(s) · {} · id {}", r.files.len(), r.target, r.id)).weak().small());
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Uninstall").clicked() {
                                out = Some((r.id.clone(), false));
                            }
                        });
                    });
                    egui::CollapsingHeader::new("Files").id_salt(&r.id).show(ui, |ui| {
                        for f in &r.files {
                            ui.label(RichText::new(format!("{} {}", if f.backup.is_some() { "changed" } else { "added  " }, f.path)).monospace().size(11.0));
                        }
                    });
                });
            }
        });
    });
    out
}
