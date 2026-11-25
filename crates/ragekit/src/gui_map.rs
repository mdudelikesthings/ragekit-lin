//! Table editor for text map data (`.ide` item definitions and `.ipl`
//! placements), section by section.

use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
use ragekit_core::mapdata::{self, MapFile};

use crate::gui_tex::TexSource;

pub enum MapAction {
    None,
    Close,
    Save(Vec<u8>),
}

pub struct MapEditor {
    pub title: String,
    pub source: TexSource,
    file: MapFile,
    section: usize,
    filter: String,
    pub changed: bool,
    new_section: String,
    open: bool,
}

impl MapEditor {
    pub fn new(title: String, source: TexSource, file: MapFile) -> MapEditor {
        // Start on the first section with rows.
        let section = (0..file.sections.len()).find(|&i| !file.rows(i).is_empty()).unwrap_or(0);
        MapEditor { title, source, file, section, filter: String::new(), changed: false, new_section: String::new(), open: true }
    }

    pub fn show(&mut self, ctx: &egui::Context, can_edit: bool) -> MapAction {
        let mut action = MapAction::None;
        let mut open = self.open;
        egui::Window::new(format!("Map data — {}{}", self.title, if self.changed { " *" } else { "" }))
            .id(egui::Id::unique("map_editor"))
            .open(&mut open)
            .default_size([1000.0, 560.0])
            .resizable(true)
            .show(ctx, |ui| {
                ui.set_min_size(egui::vec2(760.0, 420.0));
                ui.horizontal_wrapped(|ui| {
                    for i in 0..self.file.sections.len() {
                        let n = self.file.rows(i).len();
                        let label = format!("{} ({n})", self.file.sections[i].name);
                        if ui.selectable_label(self.section == i, label).clicked() {
                            self.section = i;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter rows").desired_width(200.0));
                    if can_edit {
                        if ui.button("Add row").clicked() && !self.file.sections.is_empty() {
                            let width = self.file.rows(self.section).iter().map(|r| r.1.len()).max().unwrap_or(1);
                            self.file.insert_row(self.section, None, vec![String::new(); width]);
                            self.changed = true;
                        }
                        ui.add(egui::TextEdit::singleline(&mut self.new_section).hint_text("new section, e.g. inst").desired_width(140.0));
                        if ui.add_enabled(!self.new_section.trim().is_empty(), egui::Button::new("Add section")).clicked() {
                            self.section = self.file.add_section(self.new_section.trim());
                            self.new_section.clear();
                            self.changed = true;
                        }
                    } else {
                        ui.label(RichText::new("Read-only — turn on Edit mode to change this file").weak());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(can_edit && self.changed, egui::Button::new(RichText::new("Save").strong())).clicked() {
                            action = MapAction::Save(self.file.to_text().into_bytes());
                            self.changed = false;
                        }
                        if ui.button("Export text…").clicked() {
                            if let Some(p) = rfd::FileDialog::new().set_file_name(&self.title).save_file() {
                                let _ = std::fs::write(p, self.file.to_text());
                            }
                        }
                    });
                });
                ui.separator();
                if self.file.sections.is_empty() {
                    ui.label("This file has no sections.");
                    return;
                }
                let si = self.section.min(self.file.sections.len() - 1);
                let needle = self.filter.to_ascii_lowercase();
                let rows: Vec<(usize, Vec<String>)> = self
                    .file
                    .rows(si)
                    .into_iter()
                    .filter(|(_, f)| needle.is_empty() || f.iter().any(|x| x.to_ascii_lowercase().contains(&needle)))
                    .map(|(i, f)| (i, f.clone()))
                    .collect();
                let width = rows.iter().map(|r| r.1.len()).max().unwrap_or(0).max(1);
                let names = mapdata::columns(self.file.kind, &self.file.sections[si].name, width);
                let mut edits: Vec<(usize, usize, String)> = Vec::new();
                let mut remove = None;
                let mut dup = None;
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    let mut tb = TableBuilder::new(ui).auto_shrink([false, false]).min_scrolled_height(300.0).striped(true).resizable(true).column(Column::exact(if can_edit { 56.0 } else { 0.0 }));
                    for _ in 0..width {
                        tb = tb.column(Column::initial(96.0).at_least(40.0).clip(true));
                    }
                    tb.header(20.0, |mut h| {
                        h.col(|_| {});
                        for n in &names {
                            h.col(|ui| {
                                ui.label(RichText::new(n).small().strong());
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(22.0, rows.len(), |mut row| {
                            let (line, fields) = &rows[row.index()];
                            row.col(|ui| {
                                if can_edit {
                                    ui.horizontal(|ui| {
                                    if ui.small_button("+").on_hover_text("Duplicate this row").clicked() {
                                        dup = Some((*line, fields.clone()));
                                    }
                                    if ui.small_button("x").on_hover_text("Delete this row").clicked() {
                                        remove = Some(*line);
                                    }
                                    });
                                }
                            });
                            for c in 0..width {
                                row.col(|ui| {
                                    let mut v = fields.get(c).cloned().unwrap_or_default();
                                    if can_edit {
                                        if ui.add(egui::TextEdit::singleline(&mut v).desired_width(f32::INFINITY)).changed() {
                                            edits.push((*line, c, v));
                                        }
                                    } else {
                                        ui.label(v);
                                    }
                                });
                            }
                        });
                    });
                });
                for (line, c, v) in edits {
                    self.file.set_field(si, line, c, &v);
                    self.changed = true;
                }
                if let Some((line, f)) = dup {
                    self.file.insert_row(si, Some(line), f);
                    self.changed = true;
                }
                if let Some(line) = remove {
                    self.file.remove_line(si, line);
                    self.changed = true;
                }
            });
        self.open = open;
        if !open {
            return MapAction::Close;
        }
        action
    }
}
