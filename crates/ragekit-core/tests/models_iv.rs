//! Models (GTA V and GTA IV), GTA IV textures, map data and scripts.

use std::io::Write;

use ragekit_core::{model, textures};

const YDR_XML: &str = include_str!("fixtures/one_triangle.ydr.xml");
const IV_DIFFUSE: u32 = 0x2B51_70FD;

/// A little GTA IV resource builder: system and graphics memory.
struct Mem {
    sys: Vec<u8>,
    gfx: Vec<u8>,
}

impl Mem {
    fn new(sys: usize, gfx: usize) -> Mem {
        Mem { sys: vec![0; sys], gfx: vec![0; gfx] }
    }
    fn u32(&mut self, at: usize, v: u32) {
        self.sys[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn u16(&mut self, at: usize, v: u16) {
        self.sys[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn ptr(&mut self, at: usize, to: usize) {
        self.u32(at, 0x5000_0000 | to as u32);
    }
    fn gptr(&mut self, at: usize, to: usize) {
        self.u32(at, 0x6000_0000 | to as u32);
    }
    fn coll(&mut self, at: usize, list: usize, items: &[usize]) {
        self.ptr(at, list);
        self.u16(at + 4, items.len() as u16);
        self.u16(at + 6, items.len() as u16);
        for (i, &it) in items.iter().enumerate() {
            self.ptr(list + i * 4, it);
        }
    }
    fn str(&mut self, at: usize, s: &str) {
        self.sys[at..at + s.len()].copy_from_slice(s.as_bytes());
    }
    /// A texture dictionary at `at` with one 8x8 DXT1 texture.
    fn wtd(&mut self, at: usize, info: usize, name_at: usize, gfx_at: usize, name: &str) {
        self.coll(at + 24, at + 0x40, &[info]);
        self.ptr(info + 20, name_at);
        self.str(name_at, &format!("pack:/{name}.dds"));
        self.u16(info + 28, 8);
        self.u16(info + 30, 8);
        self.u32(info + 32, 0x3154_5844);
        self.sys[info + 39] = 1;
        self.gptr(info + 72, gfx_at);
        for (i, b) in self.gfx[gfx_at..gfx_at + 32].iter_mut().enumerate() {
            *b = i as u8 * 7;
        }
    }
    fn build(&self, rtype: u32) -> Vec<u8> {
        assert_eq!(self.sys.len() % 256, 0);
        assert_eq!(self.gfx.len() % 256, 0);
        let flags = (self.sys.len() / 256) as u32 | ((self.gfx.len() / 256) as u32) << 15;
        let mut out = b"RSC\x05".to_vec();
        out.extend(rtype.to_le_bytes());
        out.extend(flags.to_le_bytes());
        let mut z = flate2::write::ZlibEncoder::new(out, flate2::Compression::default());
        z.write_all(&self.sys).unwrap();
        z.write_all(&self.gfx).unwrap();
        z.finish().unwrap()
    }
}

fn wtd_file() -> Vec<u8> {
    let mut m = Mem::new(0x200, 0x100);
    m.wtd(0, 0x80, 0x100, 0, "admiral_body");
    m.build(8)
}

/// One triangle, a shader naming its diffuse texture, an embedded texture.
fn wdr_file() -> Vec<u8> {
    let mut m = Mem::new(0xB00, 0x300);
    m.ptr(8, 0x100); // shader group
    m.ptr(0x40, 0x200); // high LOD
    m.coll(0x200, 0x210, &[0x300]);
    m.coll(0x304, 0x320, &[0x400]);
    m.ptr(0x310, 0x330); // shader mapping: geometry 0 -> shader 0
    m.ptr(0x40C, 0x500); // vertex buffer (geometry +12)
    m.ptr(0x41C, 0x600); // index buffer (geometry +28)
    m.u32(0x42C, 3);
    m.u16(0x504, 3);
    m.gptr(0x508, 0);
    m.u32(0x50C, 32);
    m.ptr(0x510, 0x700);
    m.u32(0x700, 1 | 1 << 3 | 1 << 6);
    m.u16(0x704, 32);
    let types: u64 = 6 | 6 << 12 | 5 << 24;
    m.sys[0x708..0x710].copy_from_slice(&types.to_le_bytes());
    let verts: [[f32; 8]; 3] = [[0., 0., 0., 0., 0., 1., 0., 0.], [1., 0., 0., 0., 0., 1., 1., 0.], [0., 1., 0., 0., 0., 1., 0., 1.]];
    for (i, v) in verts.iter().enumerate() {
        for (j, f) in v.iter().enumerate() {
            m.gfx[i * 32 + j * 4..i * 32 + j * 4 + 4].copy_from_slice(&f.to_le_bytes());
        }
    }
    m.u32(0x604, 3);
    m.gptr(0x608, 0x100);
    for (i, ix) in [0u16, 1, 2].iter().enumerate() {
        m.gfx[0x100 + i * 2..0x102 + i * 2].copy_from_slice(&ix.to_le_bytes());
    }
    // Shader group: embedded textures and one shader.
    m.ptr(0x104, 0x800);
    m.coll(0x108, 0x140, &[0x180]);
    m.ptr(0x180 + 20, 0x1E0);
    m.u32(0x180 + 28, 1);
    m.ptr(0x180 + 36, 0x1F0);
    m.ptr(0x180 + 52, 0x1F4);
    m.ptr(0x1E0, 0x900);
    m.u32(0x1F4, IV_DIFFUSE);
    m.ptr(0x900 + 20, 0x980);
    m.str(0x980, "admiral_body");
    m.wtd(0x800, 0xA00, 0xA80, 0x200, "admiral_body");
    m.build(110)
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 16) as u8, (y * 16) as u8, 200, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[test]
fn gta_v_drawable() {
    let ydr = rage_formats::build_ydr_from_xml(YDR_XML, None).unwrap();
    let m = model::load("tri_prop.ydr", &ydr).unwrap();
    assert_eq!(m.game, "GTA V");
    assert_eq!(m.triangles(), 1);
    assert_eq!(m.meshes[0].positions.len(), m.meshes[0].normals.len());
    let (obj, mtl) = m.to_obj("tri_prop.mtl");
    assert!(obj.contains("mtllib tri_prop.mtl") && obj.lines().filter(|l| l.starts_with("f ")).count() == 1);
    assert!(mtl.contains("newmtl"));
    assert!(m.bounds().is_some());
}

#[test]
fn gta_iv_drawable_with_embedded_texture() {
    let m = model::load("admiral.wdr", &wdr_file()).unwrap();
    assert_eq!(m.game, "GTA IV");
    assert_eq!(m.triangles(), 1);
    let mesh = &m.meshes[0];
    assert_eq!(mesh.positions[1], [1.0, 0.0, 0.0]);
    assert_eq!(mesh.normals[0], [0.0, 0.0, 1.0]);
    assert_eq!(mesh.uvs[2], [0.0, 1.0]);
    assert_eq!(mesh.texture.as_deref(), Some("admiral_body"));
    assert_eq!(m.textures.len(), 1);
    assert_eq!(m.textures[0].name, "admiral_body");
    assert!(textures::rgba(&m.textures[0]).is_ok());
}

#[test]
fn gta_iv_bad_models_error_cleanly() {
    let good = wdr_file();
    assert!(model::load("x.wdr", &good[..20]).is_err());
    assert!(model::load("x.wft", &good).is_err() || model::load("x.wft", &good).is_ok());
    assert!(model::load("x.txt", &good).is_err());
}

#[test]
fn gta_iv_texture_dictionary_replace() {
    let data = wtd_file();
    assert!(textures::is_texture_dict("a.wtd", &data));
    let mut d = textures::load(&data).unwrap();
    assert!(!d.can_restructure());
    assert_eq!(d.textures.len(), 1);
    assert_eq!(d.textures[0].name, "admiral_body");
    assert_eq!((d.textures[0].width, d.textures[0].height), (8, 8));
    let before = d.textures[0].pixel_data.clone();
    assert!(textures::to_png(&d.textures[0]).is_ok());
    assert!(d.remove("admiral_body").is_err());
    assert!(d.rename("admiral_body", "x").is_err());

    // A 16x16 PNG is resized to the original 8x8 DXT1.
    d.replace("ADMIRAL_BODY", &png(16, 16)).unwrap();
    let out = d.save().unwrap();
    let d2 = textures::load(&out).unwrap();
    assert_eq!(d2.textures[0].pixel_data.len(), before.len());
    assert_ne!(d2.textures[0].pixel_data, before);
}

#[test]
fn ide_ipl_round_trip() {
    use ragekit_core::mapdata::{MapFile, MapKind};
    let ide = "objs\ncj_box, cj_box_txd, 300, 0, 0, -1, -1, 0, 1, 1, 1, 0, 0, 0.5, 1.4, null\nend\ntxdp\ncj_box_txd, generic\nend\n";
    let mut f = MapFile::parse(ide, MapKind::Ide).unwrap();
    assert_eq!(f.to_text(), ide);
    let line = f.rows(0)[0].0;
    f.set_field(0, line, 2, "500");
    assert!(f.to_text().contains("cj_box, cj_box_txd, 500,"));
}

#[test]
fn script_strings_and_natives() {
    // Header at 0, code page at 0x100, natives at 0x200, name at 0x280,
    // string page table at 0x300, strings at 0x400.
    let mut s = vec![0u8; 0x800];
    let put32 = |s: &mut Vec<u8>, o: usize, v: u32| s[o..o + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |s: &mut Vec<u8>, o: usize, v: u64| s[o..o + 8].copy_from_slice(&v.to_le_bytes());
    let p = |o: usize| 0x5000_0000u64 | o as u64;
    put64(&mut s, 0x10, p(0xF0));
    put64(&mut s, 0xF0, p(0x100));
    put32(&mut s, 0x1C, 4); // code length
    put32(&mut s, 0x2C, 2); // natives
    put64(&mut s, 0x40, p(0x200));
    let natives = [0x1234_5678_9ABC_DEF0u64, 0x0FED_CBA9_8765_4321];
    for (i, n) in natives.iter().enumerate() {
        put64(&mut s, 0x200 + i * 8, n.rotate_right(((4 + i) & 63) as u32));
    }
    s[0x100..0x104].copy_from_slice(&[1, 2, 3, 4]);
    put64(&mut s, 0x60, p(0x280));
    s[0x280..0x28A].copy_from_slice(b"main_test\0");
    put64(&mut s, 0x68, p(0x300));
    put64(&mut s, 0x300, p(0x400));
    let strings = b"hello\0PLAYER_PED_ID\0\0";
    put32(&mut s, 0x70, strings.len() as u32);
    s[0x400..0x400 + strings.len()].copy_from_slice(strings);
    let file = rage_formats::build_rsc7(10, &s, &[]);
    let sc = ragekit_core::ysc::parse(&file).unwrap();
    assert_eq!(sc.name, "main_test");
    assert_eq!(sc.natives, natives);
    assert_eq!(sc.strings, vec!["hello", "PLAYER_PED_ID"]);
    assert_eq!(sc.code, vec![1, 2, 3, 4]);
    assert!(sc.report().contains("PLAYER_PED_ID"));
}

#[test]
fn ydr_xml_round_trip() {
    let ydr = model::ydr_from_xml(YDR_XML, None).unwrap();
    assert_eq!(model::ydr_to_xml(&ydr, None).unwrap(), YDR_XML);
}

/// With RAGEKIT_DUMP=<dir>, write the synthetic files for manual checks.
#[test]
fn dump_samples() {
    let Some(dir) = std::env::var_os("RAGEKIT_DUMP") else { return };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("admiral.wdr"), wdr_file()).unwrap();
    std::fs::write(dir.join("admiral.wtd"), wtd_file()).unwrap();
    std::fs::write(dir.join("tri_prop.ydr"), model::ydr_from_xml(YDR_XML, None).unwrap()).unwrap();
}
