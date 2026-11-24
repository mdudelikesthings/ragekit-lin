//! Texture dictionary (.ytd) viewer window.

use std::collections::HashMap;

use eframe::egui::{self, Color32, RichText};
use ragekit_core::textures::{self, Dict};

use crate::ops::human;

/// Where the dictionary came from (so the app knows where to save it).
#[derive(Clone)]
pub enum TexSource {
    Disk(std::path::PathBuf),
    Archive { disk: std::path::PathBuf, inner: String },
}

pub struct TexViewer {
    pub title: String,
    pub source: TexSource,
    pub dict: Dict,
    pub selected: usize,
    pub changed: bool,
    cache: HashMap<usize, Result<egui::TextureHandle, String>>,
    fit: bool,
    filter: String,
    open: bool,
}

pub enum TexAction {
    None,
    Close,
    /// Write these bytes back to `source`.
    Save(Vec<u8>),
    Message(String, String),
}

impl TexViewer {
    pub fn new(title: String, source: TexSource, dict: Dict) -> TexViewer {
        TexViewer { title, source, dict, selected: 0, changed: false, cache: HashMap::new(), fit: true, filter: String::new(), open: true }
    }

    fn handle(&mut self, ctx: &egui::Context, i: usize) -> Result<egui::TextureHandle, String> {
        if let Some(h) = self.cache.get(&i) {
            return h.clone();
        }
        let t = &self.dict.textures[i];
        let r = textures::rgba(t).map(|px| {
            let ci = egui::ColorImage::from_rgba_unmultiplied([t.width as usize, t.height as usize], &px);
            ctx.load_texture(format!("tex-{i}-{}", t.name), ci, egui::TextureOptions::LINEAR)
        });
        let r = r.map_err(|e| e.to_string());
        self.cache.insert(i, r.clone());
        r
    }

    fn invalidate(&mut self) {
        self.cache.clear();
        self.changed = true;
        if self.selected >= self.dict.textures.len() {
            self.selected = self.dict.textures.len().saturating_sub(1);
        }
    }

    /// `can_edit`: whether changes may be written back.
    pub fn show(&mut self, ctx: &egui::Context, can_edit: bool) -> TexAction {
        let mut action = TexAction::None;
        let mut open = self.open;
        let editable = can_edit && !self.dict.enhanced;
        egui::Window::new(format!("Textures — {}{}", self.title, if self.changed { " *" } else { "" }))
            .open(&mut open)
            .default_size([920.0, 600.0])
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!(
                        "{} textures{}",
                        self.dict.textures.len(),
                        if self.dict.enhanced { " · Enhanced (gen9): view and export only" } else { "" }
                    )).weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if editable && ui.add_enabled(self.changed, egui::Button::new(RichText::new("Apply changes").strong())).clicked() {
                            match self.dict.save() {
                                Ok(b) => {
                                    self.changed = false;
                                    action = TexAction::Save(b);
                                }
                                Err(e) => action = TexAction::Message("Textures".into(), e.to_string()),
                            }
                        }
                        if ui.button("Export all…").clicked() {
                            if let Some(dir) = rfd::FileDialog::new().set_title("Export all textures to").pick_folder() {
                                let mut n = 0;
                                for t in &self.dict.textures {
                                    let (b, ext) = match textures::to_png(t) {
                                        Ok(b) => (b, "png"),
                                        Err(_) => (textures::to_dds(t), "dds"),
                                    };
                                    if std::fs::write(dir.join(format!("{}.{ext}", t.name)), b).is_ok() {
                                        n += 1;
                                    }
                                }
                                action = TexAction::Message("Export".into(), format!("Exported {n} textures to {}", dir.display()));
                            }
                        }
                        if !can_edit && !self.dict.enhanced {
                            ui.label(RichText::new("Turn on Edit mode to change textures").weak());
                        }
                    });
                });
                ui.separator();
                egui::Panel::left("tex-list").resizable(true).default_size(300.0).show(ui, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter").desired_width(f32::INFINITY));
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        let f = self.filter.to_lowercase();
                        for (i, t) in self.dict.textures.iter().enumerate() {
                            if !f.is_empty() && !t.name.to_lowercase().contains(&f) {
                                continue;
                            }
                            let text = format!("{}\n{}x{} · {}", t.name, t.width, t.height, textures::format_name(t.format));
                            if ui.selectable_label(i == self.selected, text).clicked() {
                                self.selected = i;
                            }
                        }
                    });
                });
                egui::CentralPanel::default().show(ui, |ui| {
                    if self.dict.textures.is_empty() {
                        ui.label("This dictionary has no textures.");
                    } else {
                        let i = self.selected.min(self.dict.textures.len() - 1);
                        let t = self.dict.textures[i].clone();
                        ui.horizontal_wrapped(|ui| {
                            ui.heading(&t.name);
                            ui.label(format!(
                                "{}x{} · {} · {} mip{} · {}",
                                t.width,
                                t.height,
                                textures::format_name(t.format),
                                t.levels,
                                if t.levels == 1 { "" } else { "s" },
                                human(t.pixel_data.len() as u64)
                            ));
                        });
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.fit, "Fit to window");
                            if ui.button("Export PNG…").clicked() {
                                match textures::to_png(&t) {
                                    Ok(b) => {
                                        if let Some(p) = rfd::FileDialog::new().set_file_name(format!("{}.png", t.name)).save_file() {
                                            let _ = std::fs::write(p, b);
                                        }
                                    }
                                    Err(e) => action = TexAction::Message("Export".into(), e.to_string()),
                                }
                            }
                            if ui.button("Export DDS…").clicked() {
                                if let Some(p) = rfd::FileDialog::new().set_file_name(format!("{}.dds", t.name)).save_file() {
                                    let _ = std::fs::write(p, textures::to_dds(&t));
                                }
                            }
                            if editable {
                                ui.separator();
                                if ui.button("Replace…").on_hover_text("PNG, JPEG, TGA, BMP or DDS. Keeps the texture's name and format.").clicked() {
                                    if let Some(p) = rfd::FileDialog::new().add_filter("Images", &["png", "dds", "jpg", "jpeg", "tga", "bmp", "webp"]).pick_file() {
                                        match std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| self.dict.replace(&t.name, &b).map_err(|e| e.to_string())) {
                                            Ok(()) => self.invalidate(),
                                            Err(e) => action = TexAction::Message("Replace texture".into(), e),
                                        }
                                    }
                                }
                                if ui.button("Add…").clicked() {
                                    if let Some(files) = rfd::FileDialog::new().add_filter("Images", &["png", "dds", "jpg", "jpeg", "tga", "bmp", "webp"]).pick_files() {
                                        for p in files {
                                            let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                                            if let Err(e) = std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| self.dict.add(&name, &b).map_err(|e| e.to_string())) {
                                                action = TexAction::Message("Add texture".into(), format!("{name}: {e}"));
                                            }
                                        }
                                        self.invalidate();
                                    }
                                }
                                if ui.button("Remove").clicked() {
                                    if self.dict.remove(&t.name).is_ok() {
                                        self.invalidate();
                                    }
                                }
                            }
                        });
                        ui.separator();
                        match self.handle(ui.ctx(), i) {
                            Ok(h) => {
                                egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                                    let avail = ui.available_size();
                                    let mut size = egui::vec2(t.width as f32, t.height as f32);
                                    if self.fit {
                                        let s = (avail.x / size.x).min(avail.y / size.y).min(4.0);
                                        size *= s.max(0.01);
                                    }
                                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                                    // Checkerboard behind transparent pixels.
                                    let p = ui.painter_at(rect);
                                    let cell = 12.0;
                                    let (nx, ny) = ((rect.width() / cell).ceil() as i32, (rect.height() / cell).ceil() as i32);
                                    for y in 0..ny.min(400) {
                                        for x in 0..nx.min(400) {
                                            let c = if (x + y) % 2 == 0 { Color32::from_gray(70) } else { Color32::from_gray(50) };
                                            let r = egui::Rect::from_min_size(rect.min + egui::vec2(x as f32 * cell, y as f32 * cell), egui::vec2(cell, cell));
                                            p.rect_filled(r.intersect(rect), 0.0, c);
                                        }
                                    }
                                    p.image(h.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                                });
                            }
                            Err(e) => {
                                ui.colored_label(Color32::LIGHT_RED, format!("Can't display this texture: {e}"));
                                ui.label("It can still be exported as DDS.");
                            }
                        }
                    }
                });
            });
        if !open {
            return TexAction::Close;
        }
        action
    }
}
