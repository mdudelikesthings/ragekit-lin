//! 3D models: GTA V drawables (`.ydr`, `.ydd`, `.yft`) and GTA IV drawables
//! (`.wdr`, `.wdd`, `.wft`), flattened to triangle meshes for viewing and
//! for export to Wavefront OBJ.
//!
//! GTA V parsing uses the public-domain `rage-formats` crate. GTA IV
//! parsing follows SparkIV's documentation of the RSC5 structures
//! (independent implementation).

use rage_formats::ydd::{self, Drawable, DrawableKind};
use rage_formats::YtdTexture;

use crate::rsc5::Rsc5;
use crate::{Error, Result};

/// One drawable piece with a single material.
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    /// e.g. "adder / model 0 / geometry 2".
    pub label: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// Triangle list.
    pub indices: Vec<u32>,
    /// Diffuse texture name, if the shader names one.
    pub texture: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Model {
    pub game: &'static str,
    pub meshes: Vec<Mesh>,
    /// Textures embedded in the model file.
    pub textures: Vec<YtdTexture>,
    /// Facts for the info panel.
    pub notes: Vec<String>,
}

pub fn is_model_name(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    [".ydr", ".ydd", ".yft", ".wdr", ".wdd", ".wft"].iter().any(|e| l.ends_with(e))
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Invalid(e.to_string())
}

impl Model {
    pub fn triangles(&self) -> usize {
        self.meshes.iter().map(|m| m.indices.len() / 3).sum()
    }
    pub fn vertices(&self) -> usize {
        self.meshes.iter().map(|m| m.positions.len()).sum()
    }
    /// Axis-aligned bounds of all vertices.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut it = self.meshes.iter().flat_map(|m| m.positions.iter()).filter(|p| p.iter().all(|c| c.is_finite()));
        let first = *it.next()?;
        Some(it.fold((first, first), |(mut lo, mut hi), p| {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
            (lo, hi)
        }))
    }

    /// Wavefront OBJ text plus a matching MTL (textures referenced as
    /// `<name>.png`, next to the files).
    pub fn to_obj(&self, mtl_name: &str) -> (String, String) {
        use std::fmt::Write;
        let mut obj = String::new();
        let mut mtl = String::new();
        let _ = writeln!(obj, "# exported by ragekit ({})\nmtllib {mtl_name}", self.game);
        let mut materials = std::collections::BTreeSet::new();
        let mut base = 1usize;
        for (i, m) in self.meshes.iter().enumerate() {
            let _ = writeln!(obj, "o mesh{i}");
            let mat = m.texture.clone().unwrap_or_else(|| "default".into());
            materials.insert(mat.clone());
            for p in &m.positions {
                let _ = writeln!(obj, "v {} {} {}", p[0], p[1], p[2]);
            }
            for t in &m.uvs {
                let _ = writeln!(obj, "vt {} {}", t[0], 1.0 - t[1]);
            }
            for n in &m.normals {
                let _ = writeln!(obj, "vn {} {} {}", n[0], n[1], n[2]);
            }
            let _ = writeln!(obj, "usemtl {mat}");
            let (has_t, has_n) = (m.uvs.len() == m.positions.len(), m.normals.len() == m.positions.len());
            for tri in m.indices.chunks_exact(3) {
                let f = |i: u32| {
                    let k = base + i as usize;
                    match (has_t, has_n) {
                        (true, true) => format!("{k}/{k}/{k}"),
                        (true, false) => format!("{k}/{k}"),
                        (false, true) => format!("{k}//{k}"),
                        (false, false) => format!("{k}"),
                    }
                };
                let _ = writeln!(obj, "f {} {} {}", f(tri[0]), f(tri[1]), f(tri[2]));
            }
            base += m.positions.len();
        }
        for mat in materials {
            let _ = writeln!(mtl, "newmtl {mat}\nKd 1 1 1");
            if mat != "default" {
                let _ = writeln!(mtl, "map_Kd {mat}.png");
            }
            mtl.push('\n');
        }
        (obj, mtl)
    }
}

/// CodeWalker-style XML of a GTA V `.ydr` (the format CodeWalker and most
/// modern tools import and export). With `dds_dir`, embedded textures are
/// written there as `.dds` files, as CodeWalker does.
pub fn ydr_to_xml(data: &[u8], dds_dir: Option<&std::path::Path>) -> Result<String> {
    rage_formats::dump_ydr_xml(data, &rage_formats::NameTable::core(), dds_dir).map_err(|e| Error::Invalid(format!("{e:#}")))
}

/// Build a `.ydr` from CodeWalker-style XML; embedded textures are read
/// from `.dds` files in `dds_dir` (usually the XML's folder).
pub fn ydr_from_xml(xml: &str, dds_dir: Option<&std::path::Path>) -> Result<Vec<u8>> {
    rage_formats::build_ydr_from_xml(xml, dds_dir).map_err(|e| Error::Invalid(format!("{e:#}")))
}

/// Load a model from a file's bytes; `name` picks the kind.
pub fn load(name: &str, data: &[u8]) -> Result<Model> {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "ydr" | "ydd" | "yft" => load_v(&ext, data),
        "wdr" | "wdd" | "wft" => load_iv(&ext, data),
        _ => Err(Error::Invalid(format!("{name}: not a model file"))),
    }
}

// ---- GTA V ---------------------------------------------------------------

fn load_v(ext: &str, data: &[u8]) -> Result<Model> {
    if let Ok(Some(rage_formats::gen9::Edition::Gen9)) = rage_formats::gen9::Edition::of_file(ext, data) {
        return Err(Error::Invalid("Enhanced (gen9) models can't be shown yet".into()));
    }
    let mut model = Model { game: "GTA V", ..Default::default() };
    if ext == "yft" {
        let frag = rage_formats::yft::parse_yft(data).map_err(err)?;
        let name = frag.name.clone();
        for (pi, part) in frag.render_parts().into_iter().enumerate() {
            let label = if pi == 0 { name.clone() } else { format!("{name} / part {pi}") };
            add_drawable(&mut model, &label, part.drawable, &|bone: usize, p: [f32; 3]| {
                let mut v = rage_formats::Vec3::new(p[0], p[1], p[2]);
                if let Some(b) = part.bone_transforms.get(bone) {
                    let q = b.transform_point(v);
                    v = rage_formats::Vec3::new(q.x, q.y, q.z);
                }
                let q = part.transform.transform_point(v);
                [q.x, q.y, q.z]
            });
        }
        if let Some(d) = &frag.drawable {
            embedded_v(&mut model, d);
        }
        model.notes.push(format!("Fragment with {} physics children", frag.children.len()));
    } else {
        let kind = if ext == "ydd" { DrawableKind::Ydd } else { DrawableKind::Ydr };
        let entries = ydd::parse_drawables(data, kind).map_err(err)?;
        if entries.len() > 1 {
            model.notes.push(format!("{} drawables in the dictionary", entries.len()));
        }
        for e in &entries {
            add_drawable(&mut model, &e.name, &e.drawable, &|_, p| p);
            embedded_v(&mut model, &e.drawable);
        }
    }
    Ok(model)
}

fn embedded_v(model: &mut Model, d: &Drawable) {
    if let Some(sg) = &d.shader_group {
        for t in &sg.textures {
            if !model.textures.iter().any(|x| x.name.eq_ignore_ascii_case(&t.name)) {
                model.textures.push(t.clone());
            }
        }
    }
}

fn add_drawable(model: &mut Model, label: &str, d: &Drawable, place: &dyn Fn(usize, [f32; 3]) -> [f32; 3]) {
    let Some(lod) = d.best_lod() else { return };
    for (mi, m) in lod.models.iter().enumerate() {
        let bone = if m.is_skinned() { usize::MAX } else { m.bone_index() };
        for (gi, g) in m.geometries.iter().enumerate() {
            let (Some(vb), Some(ib)) = (&g.vertex_buffer, &g.index_buffer) else { continue };
            let Ok(verts) = vb.to_unified_vertices() else {
                model.notes.push(format!("{label}: model {mi} geometry {gi}: unreadable vertex format"));
                continue;
            };
            let mut mesh = Mesh {
                label: format!("{label} / model {mi} / geometry {gi}"),
                texture: d.diffuse_texture_name(g.shader_id).map(str::to_string),
                ..Default::default()
            };
            for v in &verts {
                let p = place(bone, [v.position.x, v.position.y, v.position.z]);
                let o = place(bone, [0.0; 3]);
                let n = place(bone, [v.normal.x, v.normal.y, v.normal.z]);
                mesh.positions.push(p);
                mesh.normals.push([n[0] - o[0], n[1] - o[1], n[2] - o[2]]);
                mesh.uvs.push([v.texcoord0.x, v.texcoord0.y]);
            }
            let n = verts.len() as u32;
            mesh.indices = ib.indices.iter().copied().take(ib.indices.len() / 3 * 3).collect();
            if mesh.indices.iter().any(|&i| i >= n) {
                model.notes.push(format!("{label}: geometry {gi} has indices out of range; skipped"));
                continue;
            }
            model.meshes.push(mesh);
        }
    }
}

// ---- GTA IV --------------------------------------------------------------

/// Diffuse texture parameter ("texture") name hash.
const IV_DIFFUSE: u32 = 0x2B51_70FD;

fn half(b: u16) -> f32 {
    half::f16::from_bits(b).to_f32()
}

/// Byte size of a declaration element type.
fn elem_size(t: u64) -> usize {
    match t {
        0 => 2,
        1 => 4,
        2 => 6,
        3 => 8,
        4 => 4,
        5 => 8,
        6 => 12,
        7 => 16,
        8..=11 => 4,
        _ => 0,
    }
}

/// Read element `t` at `b` as up to 4 floats.
fn read_elem(t: u64, b: &[u8]) -> [f32; 4] {
    let f = |i: usize| f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
    let h = |i: usize| half(u16::from_le_bytes([b[i * 2], b[i * 2 + 1]]));
    match t {
        0 => [h(0), 0.0, 0.0, 0.0],
        1 => [h(0), h(1), 0.0, 0.0],
        2 => [h(0), h(1), h(2), 0.0],
        3 => [h(0), h(1), h(2), h(3)],
        4 => [f(0), 0.0, 0.0, 0.0],
        5 => [f(0), f(1), 0.0, 0.0],
        6 => [f(0), f(1), f(2), 0.0],
        7 => [f(0), f(1), f(2), f(3)],
        10 => {
            // Dec3N: 10:10:10 signed normalised.
            let v = u32::from_le_bytes(b[..4].try_into().unwrap());
            let s = |x: u32| ((x << 22) as i32 >> 22) as f32 / 511.0;
            [s(v), s(v >> 10), s(v >> 20), 0.0]
        }
        _ => [b[0] as f32 / 255.0, b[1] as f32 / 255.0, b[2] as f32 / 255.0, b[3] as f32 / 255.0],
    }
}

fn load_iv(ext: &str, data: &[u8]) -> Result<Model> {
    let r = Rsc5::parse(data)?;
    let mut model = Model { game: "GTA IV", ..Default::default() };
    match ext {
        "wdr" => iv_drawable(&r, 0, "drawable", &mut model)?,
        "wdd" => {
            // pgDictionary: hashes at 16, drawables at 24.
            let items = r.ptr_collection(24)?;
            model.notes.push(format!("{} drawables in the dictionary", items.len()));
            let hashes = r.ptr(16)?;
            for (i, at) in items.into_iter().enumerate() {
                let label = match hashes {
                    Some(h) => format!("0x{:08x}", r.u32(h + i * 4).unwrap_or(0)),
                    None => format!("drawable {i}"),
                };
                if let Err(e) = iv_drawable(&r, at, &label, &mut model) {
                    model.notes.push(format!("{label}: {e}"));
                }
            }
        }
        _ => {
            // fragType: main drawable pointer at 0xB4.
            let at = r.ptr(0xB4)?.ok_or_else(|| Error::Malformed("fragment has no drawable".into()))?;
            iv_drawable(&r, at, "fragment", &mut model)?;
            model.notes.push("Fragment children (doors, wheels…) aren't placed yet; only the main body is shown".into());
        }
    }
    if model.meshes.is_empty() {
        return Err(Error::Malformed("no geometry found".into()));
    }
    Ok(model)
}

/// Shader index → diffuse texture name, and the embedded texture dictionary.
fn iv_shaders(r: &Rsc5, group: usize, model: &mut Model) -> Result<Vec<Option<String>>> {
    if let Some(td) = r.ptr(group + 4)? {
        match crate::textures::load_iv_at(Rsc5 { magic: r.magic, rtype: r.rtype, flags: r.flags, system: r.system.clone(), graphics: r.graphics.clone() }, td) {
            Ok(d) => {
                for t in d.textures {
                    if !model.textures.iter().any(|x| x.name.eq_ignore_ascii_case(&t.name)) {
                        model.textures.push(t);
                    }
                }
            }
            Err(e) => model.notes.push(format!("embedded textures: {e}")),
        }
    }
    let mut out = Vec::new();
    for sh in r.ptr_collection(group + 8)? {
        let count = (r.u32(sh + 28)? as usize).min(256);
        let (Some(offs), Some(types), Some(names)) = (r.ptr(sh + 20)?, r.ptr(sh + 36)?, r.ptr(sh + 52)?) else {
            out.push(None);
            continue;
        };
        let mut tex = None;
        for i in 0..count {
            if r.u8(types + i)? != 0 || r.u32(names + i * 4)? != IV_DIFFUSE {
                continue;
            }
            if let Some(p) = r.ptr(offs + i * 4)? {
                if let Some(n) = r.ptr(p + 20)? {
                    tex = Some(r.cstr(n)?);
                }
            }
        }
        out.push(tex);
    }
    Ok(out)
}

fn iv_drawable(r: &Rsc5, at: usize, label: &str, model: &mut Model) -> Result<()> {
    let shaders = match r.ptr(at + 8)? {
        Some(g) => iv_shaders(r, g, model).unwrap_or_else(|e| {
            model.notes.push(format!("{label}: shaders: {e}"));
            Vec::new()
        }),
        None => Vec::new(),
    };
    // Highest LOD that has models.
    let mut lod = None;
    for i in 0..4 {
        if let Some(p) = r.ptr(at + 0x40 + i * 4)? {
            lod = Some(p);
            break;
        }
    }
    let Some(lod) = lod else { return Ok(()) };
    for (mi, m) in r.ptr_collection(lod)?.into_iter().enumerate() {
        // grmModel: vtable, geometries (ptr,n,cap), bounds ptr, shader mapping ptr
        let geoms = r.ptr_collection(m + 4)?;
        let mapping = r.ptr(m + 16)?;
        for (gi, g) in geoms.into_iter().enumerate() {
            let shader = mapping.and_then(|p| r.u16(p + gi * 2).ok()).unwrap_or(0) as usize;
            match iv_geometry(r, g) {
                Ok(mut mesh) => {
                    mesh.label = format!("{label} / model {mi} / geometry {gi}");
                    mesh.texture = shaders.get(shader).cloned().flatten();
                    model.meshes.push(mesh);
                }
                Err(e) => model.notes.push(format!("{label}: model {mi} geometry {gi}: {e}")),
            }
        }
    }
    Ok(())
}

fn iv_geometry(r: &Rsc5, g: usize) -> Result<Mesh> {
    let vb = r.ptr(g + 12)?.ok_or_else(|| Error::Malformed("no vertex buffer".into()))?;
    let ib = r.ptr(g + 28)?.ok_or_else(|| Error::Malformed("no index buffer".into()))?;
    let index_count = r.u32(g + 44)? as usize;
    let vcount = r.u16(vb + 4)? as usize;
    let vdata = r.gfx_ptr(vb + 8)?.ok_or_else(|| Error::Malformed("vertex buffer has no data".into()))?;
    let stride = r.u32(vb + 12)? as usize;
    let decl = r.ptr(vb + 16)?.ok_or_else(|| Error::Malformed("no vertex declaration".into()))?;
    let usage = r.u32(decl)?;
    let types = u32::from_le_bytes(r.system.get(decl + 8..decl + 12).ok_or_else(|| Error::Malformed("declaration".into()))?.try_into().unwrap()) as u64
        | (r.u32(decl + 12)? as u64) << 32;
    if stride == 0 || stride > 256 || vcount == 0 {
        return Err(Error::Malformed(format!("bad vertex buffer (stride {stride}, {vcount} vertices)")));
    }
    let raw = r.graphics_slice(vdata, vcount * stride)?;
    // Element offsets, in bit order 0..16 for used bits.
    let mut offs = [usize::MAX; 16];
    let mut o = 0;
    for (bit, off) in offs.iter_mut().enumerate() {
        if usage & (1 << bit) != 0 {
            *off = o;
            o += elem_size((types >> (bit * 4)) & 0xF);
        }
    }
    if o > stride || offs[0] == usize::MAX {
        return Err(Error::Malformed(format!("vertex declaration ({o} bytes, flags {usage:#x}) doesn't fit the stride {stride}")));
    }
    let ty = |bit: usize| (types >> (bit * 4)) & 0xF;
    let mut mesh = Mesh::default();
    for v in raw.chunks_exact(stride) {
        let p = read_elem(ty(0), &v[offs[0]..]);
        mesh.positions.push([p[0], p[1], p[2]]);
        if offs[3] != usize::MAX {
            let n = read_elem(ty(3), &v[offs[3]..]);
            mesh.normals.push([n[0], n[1], n[2]]);
        }
        if offs[6] != usize::MAX {
            let t = read_elem(ty(6), &v[offs[6]..]);
            mesh.uvs.push([t[0], t[1]]);
        }
    }
    let icount = r.u32(ib + 4)? as usize;
    let idata = r.gfx_ptr(ib + 8)?.ok_or_else(|| Error::Malformed("index buffer has no data".into()))?;
    let n = icount.min(if index_count > 0 { index_count } else { icount });
    let ib = r.graphics_slice(idata, n * 2)?;
    mesh.indices = ib.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as u32).collect();
    mesh.indices.truncate(mesh.indices.len() / 3 * 3);
    if mesh.indices.iter().any(|&i| i as usize >= vcount) {
        return Err(Error::Malformed("indices out of range".into()));
    }
    Ok(mesh)
}
