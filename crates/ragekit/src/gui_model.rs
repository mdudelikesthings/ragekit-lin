//! 3D model viewer window: a small software renderer (z-buffered, lit,
//! textured), so it works everywhere egui does without a GPU code path.

use std::collections::HashMap;

use eframe::egui::{self, Color32, RichText};
use ragekit_core::model::Model;
use ragekit_core::textures;

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn mul(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    if l > 1e-12 { mul(a, 1.0 / l) } else { [0.0, 0.0, 1.0] }
}

struct Tex {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

pub enum ModelAction {
    None,
    Close,
    /// Show a message (title, body).
    Message(String, String),
}

pub struct ModelViewer {
    pub title: String,
    model: Model,
    /// Original bytes (for XML export of .ydr).
    data: Vec<u8>,
    visible: Vec<bool>,
    yaw: f32,
    pitch: f32,
    dist: f32,
    target: V3,
    radius: f32,
    wire: bool,
    textured: bool,
    tex: HashMap<String, Option<Tex>>,
    extra: Vec<textures::Dict>,
    image: Option<egui::TextureHandle>,
    rendered_for: Option<(usize, usize, [u32; 8])>,
    open: bool,
}

impl ModelViewer {
    pub fn new(title: String, model: Model, data: Vec<u8>, extra: Vec<textures::Dict>) -> ModelViewer {
        let (lo, hi) = model.bounds().unwrap_or(([-1.0; 3], [1.0; 3]));
        let target = mul(add(lo, hi), 0.5);
        let radius = (dot(sub(hi, lo), sub(hi, lo)).sqrt() * 0.5).max(0.01);
        let n = model.meshes.len();
        ModelViewer {
            title,
            model,
            data,
            visible: vec![true; n],
            yaw: -0.7,
            pitch: 0.35,
            dist: radius * 2.6,
            target,
            radius,
            wire: false,
            textured: true,
            tex: HashMap::new(),
            extra,
            image: None,
            rendered_for: None,
            open: true,
        }
    }

    fn reset(&mut self) {
        let (lo, hi) = self.model.bounds().unwrap_or(([-1.0; 3], [1.0; 3]));
        self.target = mul(add(lo, hi), 0.5);
        self.yaw = -0.7;
        self.pitch = 0.35;
        self.dist = self.radius * 2.6;
    }

    fn texture(&mut self, name: &str) -> Option<&Tex> {
        let key = name.to_ascii_lowercase();
        if !self.tex.contains_key(&key) {
            let found = self
                .model
                .textures
                .iter()
                .chain(self.extra.iter().flat_map(|d| d.textures.iter()))
                .find(|t| t.name.eq_ignore_ascii_case(name))
                .and_then(|t| textures::rgba(t).ok().map(|px| Tex { w: t.width as usize, h: t.height as usize, px }));
            self.tex.insert(key.clone(), found);
        }
        self.tex.get(&key).and_then(|t| t.as_ref())
    }

    /// Camera basis: eye, forward, right, up (Z is up in RAGE games).
    fn camera(&self) -> (V3, V3, V3, V3) {
        let dir = [self.pitch.cos() * self.yaw.cos(), self.pitch.cos() * self.yaw.sin(), self.pitch.sin()];
        let eye = add(self.target, mul(dir, self.dist));
        let f = norm(sub(self.target, eye));
        let r = norm(cross(f, [0.0, 0.0, 1.0]));
        let u = cross(r, f);
        (eye, f, r, u)
    }

    fn render(&mut self, w: usize, h: usize) -> egui::ColorImage {
        let (eye, f, r, u) = self.camera();
        let focal = (h as f32 * 0.5) / (22.5f32.to_radians()).tan();
        let near = (self.radius * 0.001).max(0.001);
        let light = norm(add(mul(f, -1.0), mul(u, 0.4)));
        let mut color = vec![0u8; w * h * 4];
        for px in color.chunks_exact_mut(4) {
            px.copy_from_slice(&[38, 40, 46, 255]);
        }
        let mut depth = vec![f32::INFINITY; w * h];
        let mut edges = Vec::new();
        let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
        let project = |p: V3| -> (f32, f32, f32) {
            let d = sub(p, eye);
            let z = dot(d, f);
            (cx + dot(d, r) * focal / z, cy - dot(d, u) * focal / z, z)
        };
        let palette = [[200, 200, 205], [190, 170, 150], [150, 180, 200], [170, 200, 160], [210, 180, 190]];
        let meshes = std::mem::take(&mut self.model.meshes);
        // Decode every texture we'll need first, then only read the cache.
        if self.textured {
            for m in &meshes {
                if let Some(t) = &m.texture {
                    self.texture(t);
                }
            }
        }
        let texmap = &self.tex;
        for (mi, m) in meshes.iter().enumerate() {
            if !self.visible.get(mi).copied().unwrap_or(true) {
                continue;
            }
            let tex: Option<&Tex> = if self.textured {
                m.texture.as_deref().and_then(|t| texmap.get(&t.to_ascii_lowercase())).and_then(|t| t.as_ref())
            } else {
                None
            };
            let base = palette[mi % palette.len()];
            let pv: Vec<(f32, f32, f32)> = m.positions.iter().map(|&p| project(p)).collect();
            let has_n = m.normals.len() == m.positions.len();
            let has_uv = m.uvs.len() == m.positions.len();
            for t in m.indices.chunks_exact(3) {
                let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
                let (pa, pb, pc) = (pv[a], pv[b], pv[c]);
                if pa.2 < near || pb.2 < near || pc.2 < near {
                    continue;
                }
                if self.wire && edges.len() < 600_000 {
                    for (p, q) in [(pa, pb), (pb, pc), (pc, pa)] {
                        edges.push((p, q));
                    }
                }
                let face_n = norm(cross(sub(m.positions[b], m.positions[a]), sub(m.positions[c], m.positions[a])));
                let area = (pb.0 - pa.0) * (pc.1 - pa.1) - (pc.0 - pa.0) * (pb.1 - pa.1);
                if area.abs() < 1e-9 {
                    continue;
                }
                let minx = pa.0.min(pb.0).min(pc.0).floor().max(0.0) as i32;
                let maxx = pa.0.max(pb.0).max(pc.0).ceil().min(w as f32 - 1.0) as i32;
                let miny = pa.1.min(pb.1).min(pc.1).floor().max(0.0) as i32;
                let maxy = pa.1.max(pb.1).max(pc.1).ceil().min(h as f32 - 1.0) as i32;
                if minx > maxx || miny > maxy {
                    continue;
                }
                let (iza, izb, izc) = (1.0 / pa.2, 1.0 / pb.2, 1.0 / pc.2);
                for y in miny..=maxy {
                    for x in minx..=maxx {
                        let (sx, sy) = (x as f32 + 0.5, y as f32 + 0.5);
                        let w0 = ((pb.0 - sx) * (pc.1 - sy) - (pc.0 - sx) * (pb.1 - sy)) / area;
                        let w1 = ((pc.0 - sx) * (pa.1 - sy) - (pa.0 - sx) * (pc.1 - sy)) / area;
                        let w2 = 1.0 - w0 - w1;
                        if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                            continue;
                        }
                        let iz = w0 * iza + w1 * izb + w2 * izc;
                        let z = 1.0 / iz;
                        let i = y as usize * w + x as usize;
                        if z >= depth[i] {
                            continue;
                        }
                        depth[i] = z;
                        let (k0, k1, k2) = (w0 * iza * z, w1 * izb * z, w2 * izc * z);
                        let n = if has_n {
                            let (na, nb, nc) = (m.normals[a], m.normals[b], m.normals[c]);
                            norm([na[0] * k0 + nb[0] * k1 + nc[0] * k2, na[1] * k0 + nb[1] * k1 + nc[1] * k2, na[2] * k0 + nb[2] * k1 + nc[2] * k2])
                        } else {
                            face_n
                        };
                        let shade = 0.3 + 0.7 * dot(n, light).abs();
                        let mut rgb = [base[0] as f32, base[1] as f32, base[2] as f32];
                        if let (Some(t), true) = (tex, has_uv) {
                            let (ua, ub, uc) = (m.uvs[a], m.uvs[b], m.uvs[c]);
                            let uu = ua[0] * k0 + ub[0] * k1 + uc[0] * k2;
                            let vv = ua[1] * k0 + ub[1] * k1 + uc[1] * k2;
                            let tx = ((uu.rem_euclid(1.0)) * t.w as f32) as usize % t.w.max(1);
                            let ty = ((vv.rem_euclid(1.0)) * t.h as f32) as usize % t.h.max(1);
                            let o = (ty * t.w + tx) * 4;
                            if let Some(p) = t.px.get(o..o + 3) {
                                rgb = [p[0] as f32, p[1] as f32, p[2] as f32];
                            }
                        }
                        let o = i * 4;
                        color[o] = (rgb[0] * shade).min(255.0) as u8;
                        color[o + 1] = (rgb[1] * shade).min(255.0) as u8;
                        color[o + 2] = (rgb[2] * shade).min(255.0) as u8;
                    }
                }
            }
        }
        self.model.meshes = meshes;
        // Wireframe: visible edges only (depth-tested with a small bias).
        for (p, q) in edges {
            let steps = ((q.0 - p.0).abs().max((q.1 - p.1).abs()).ceil() as usize).clamp(1, 4096);
            for k in 0..=steps {
                let t = k as f32 / steps as f32;
                let (x, y) = (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t);
                if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
                    continue;
                }
                let z = 1.0 / ((1.0 - t) / p.2 + t / q.2);
                let i = y as usize * w + x as usize;
                if z <= depth[i] * 1.004 + near {
                    color[i * 4..i * 4 + 3].copy_from_slice(&[25, 25, 30]);
                }
            }
        }
        egui::ColorImage::from_rgba_unmultiplied([w, h], &color)
    }

    fn key(&self) -> [u32; 8] {
        let vis = self.visible.iter().enumerate().fold(0u32, |a, (i, &v)| a.wrapping_mul(31).wrapping_add(v as u32 + i as u32));
        [self.yaw.to_bits(), self.pitch.to_bits(), self.dist.to_bits(), self.target[0].to_bits(), self.target[1].to_bits(), self.target[2].to_bits(), self.textured as u32 | (self.wire as u32) << 1, vis]
    }

    pub fn show(&mut self, ctx: &egui::Context) -> ModelAction {
        let mut action = ModelAction::None;
        let mut open = self.open;
        let title = format!("Model — {}", self.title);
        egui::Window::new(title).id(egui::Id::unique("model_viewer")).open(&mut open).default_size([980.0, 640.0]).resizable(true).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!(
                    "{} · {} meshes · {} triangles · {} vertices",
                    self.model.game,
                    self.model.meshes.len(),
                    self.model.triangles(),
                    self.model.vertices()
                )).strong());
                ui.separator();
                ui.checkbox(&mut self.textured, "Textures");
                ui.checkbox(&mut self.wire, "Wireframe");
                if ui.button("Reset view").clicked() {
                    self.reset();
                }
                if ui.button("Load textures…").on_hover_text("Use textures from a .ytd / .wtd file").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Texture dictionary", &["ytd", "wtd"]).pick_file() {
                        match std::fs::read(&p).map_err(|e| e.to_string()).and_then(|d| textures::load(&d).map_err(|e| e.to_string())) {
                            Ok(d) => {
                                self.extra.push(d);
                                self.tex.clear();
                                self.rendered_for = None;
                            }
                            Err(e) => action = ModelAction::Message("Textures".into(), format!("{}: {e}", p.display())),
                        }
                    }
                }
                if ui.button("Export OBJ…").clicked() {
                    action = self.export_obj();
                }
                if self.title.to_ascii_lowercase().ends_with(".ydr") && ui.button("Export XML…").on_hover_text("CodeWalker XML").clicked() {
                    action = self.export_xml();
                }
            });
            ui.separator();
            egui::Panel::left("model_meshes").resizable(true).default_size(230.0).show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(RichText::new("Meshes").weak());
                    for (i, m) in self.model.meshes.iter().enumerate() {
                        let label = format!("{}{}", m.label, m.texture.as_ref().map(|t| format!("  [{t}]")).unwrap_or_default());
                        ui.checkbox(&mut self.visible[i], RichText::new(label).small());
                    }
                    if !self.model.notes.is_empty() {
                        ui.separator();
                        for n in &self.model.notes {
                            ui.label(RichText::new(n).small().weak());
                        }
                    }
                    if !self.model.textures.is_empty() {
                        ui.separator();
                        ui.label(RichText::new(format!("{} embedded textures", self.model.textures.len())).weak());
                    }
                });
            });
            let size = ui.available_size();
            let (w, h) = ((size.x.max(64.0)) as usize, (size.y.max(64.0)) as usize);
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w as f32, h as f32), egui::Sense::click_and_drag());
            if resp.dragged_by(egui::PointerButton::Primary) {
                let d = resp.drag_delta();
                self.yaw -= d.x * 0.01;
                self.pitch = (self.pitch + d.y * 0.01).clamp(-1.55, 1.55);
            }
            if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
                let d = resp.drag_delta();
                let (_, _, r, u) = self.camera();
                let s = self.dist * 0.0015;
                self.target = add(self.target, add(mul(r, -d.x * s), mul(u, d.y * s)));
            }
            if resp.double_clicked() {
                self.reset();
            }
            if resp.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    self.dist = (self.dist * (-scroll * 0.002).exp()).clamp(self.radius * 0.05, self.radius * 50.0);
                }
            }
            let key = (w, h, self.key());
            if self.rendered_for != Some(key) || self.image.is_none() {
                let img = self.render(w, h);
                match &mut self.image {
                    Some(t) => t.set(img, egui::TextureOptions::NEAREST),
                    None => self.image = Some(ui.ctx().load_texture("model_view", img, egui::TextureOptions::NEAREST)),
                }
                self.rendered_for = Some(key);
            }
            if let Some(t) = &self.image {
                ui.painter().image(t.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
            ui.painter().text(
                rect.left_bottom() + egui::vec2(8.0, -8.0),
                egui::Align2::LEFT_BOTTOM,
                "Drag: rotate · Right-drag: pan · Scroll: zoom · Double-click: reset",
                egui::FontId::proportional(11.0),
                Color32::from_gray(150),
            );
        });
        self.open = open;
        if !open {
            return ModelAction::Close;
        }
        action
    }

    fn export_obj(&mut self) -> ModelAction {
        let stem = self.title.rsplit_once('.').map(|(s, _)| s).unwrap_or(&self.title).to_string();
        let Some(path) = rfd::FileDialog::new().set_file_name(format!("{stem}.obj")).add_filter("Wavefront OBJ", &["obj"]).save_file() else {
            return ModelAction::None;
        };
        let res = (|| -> std::io::Result<usize> {
            let dir = path.parent().unwrap_or(std::path::Path::new("."));
            let mtl = format!("{}.mtl", path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(stem.clone()));
            let (obj, mtl_text) = self.model.to_obj(&mtl);
            std::fs::write(&path, obj)?;
            std::fs::write(dir.join(&mtl), mtl_text)?;
            let mut n = 0;
            let names: Vec<String> = self.model.meshes.iter().filter_map(|m| m.texture.clone()).collect();
            for t in self.model.textures.iter().chain(self.extra.iter().flat_map(|d| d.textures.iter())) {
                if !names.iter().any(|x| x.eq_ignore_ascii_case(&t.name)) && !self.model.textures.iter().any(|x| std::ptr::eq(x, t)) {
                    continue;
                }
                if let (Ok(png), Ok(name)) = (textures::to_png(t), crate::ops::safe_name(&t.name)) {
                    std::fs::write(dir.join(format!("{name}.png")), png)?;
                    n += 1;
                }
            }
            Ok(n)
        })();
        match res {
            Ok(n) => ModelAction::Message("Export".into(), format!("Wrote {} with its .mtl and {n} textures.", path.display())),
            Err(e) => ModelAction::Message("Export".into(), format!("{}: {e}", path.display())),
        }
    }

    fn export_xml(&mut self) -> ModelAction {
        let stem = self.title.rsplit_once('.').map(|(s, _)| s).unwrap_or(&self.title).to_string();
        let Some(path) = rfd::FileDialog::new().set_file_name(format!("{stem}.ydr.xml")).save_file() else {
            return ModelAction::None;
        };
        match ragekit_core::model::ydr_to_xml(&self.data, path.parent()).map_err(|e| e.to_string()).and_then(|x| std::fs::write(&path, x).map_err(|e| e.to_string())) {
            Ok(()) => ModelAction::Message("Export".into(), format!("Wrote {}", path.display())),
            Err(e) => ModelAction::Message("Export".into(), e),
        }
    }
}
