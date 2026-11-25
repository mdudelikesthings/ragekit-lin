//! Texture dictionaries: GTA V `.ytd` (list, decode, export; replace, add
//! and remove for Legacy-edition files) and GTA IV `.wtd` / textures
//! embedded in GTA IV models (list, decode, export, replace in place).
//!
//! Decoding/encoding uses the public-domain `rage-formats` crate.
//! Enhanced (gen9) dictionaries can be viewed and exported; writing them
//! back isn't supported yet, so edits are refused for them.

use rage_formats::gen9::Edition;
use rage_formats::{texture_encode, texture_utils, ytd, EncodeFormat, TextureFormat, YtdTexture};

use crate::{Error, Result};

fn err(e: impl std::fmt::Display) -> Error {
    Error::Invalid(e.to_string())
}

/// A loaded texture dictionary.
pub struct Dict {
    pub textures: Vec<YtdTexture>,
    pub enhanced: bool,
    /// Set for GTA IV dictionaries: the resource and where each texture's
    /// pixels live in it.
    pub iv: Option<IvBacking>,
}

pub struct IvBacking {
    pub res: crate::rsc5::Rsc5,
    /// (graphics offset, byte length) per texture, in `textures` order.
    pub slots: Vec<(usize, usize)>,
}

pub fn is_ytd(data: &[u8]) -> bool {
    matches!(crate::resource::parse_header(data), Some((13, _, _)) | Some((5, _, _)))
}

/// A GTA IV texture dictionary (`.wtd`)?
pub fn is_wtd(name: &str, data: &[u8]) -> bool {
    crate::rsc5::is_rsc5(data) && name.to_ascii_lowercase().ends_with(".wtd")
}

/// Is this a texture dictionary ragekit can open (by name and contents)?
pub fn is_texture_dict(name: &str, data: &[u8]) -> bool {
    is_ytd(data) || is_wtd(name, data)
}

fn row_pitch(format: TextureFormat, w: u16) -> u16 {
    let w = w as u32;
    (match format {
        TextureFormat::DXT1 => w.div_ceil(4).max(1) * 8,
        TextureFormat::DXT3 | TextureFormat::DXT5 => w.div_ceil(4).max(1) * 16,
        TextureFormat::L8 => w,
        _ => w * 4,
    }) as u16
}

/// Textures of a GTA IV dictionary at system offset `at` of `res`.
pub fn load_iv_at(res: crate::rsc5::Rsc5, at: usize) -> Result<Dict> {
    let infos = crate::rsc5::texture_dict_at(&res, at)?;
    let mut textures = Vec::new();
    let mut slots = Vec::new();
    for t in infos {
        let format = TextureFormat::from_u32(t.format);
        textures.push(YtdTexture {
            name_hash: rage_formats::rage_joaat(&t.name.to_ascii_lowercase()),
            name: t.name,
            width: t.width,
            height: t.height,
            depth: 1,
            format,
            levels: t.levels,
            stride: row_pitch(format, t.width),
            pixel_data: res.graphics_slice(t.data_offset, t.data_len)?.to_vec(),
        });
        slots.push((t.data_offset, t.data_len));
    }
    Ok(Dict { textures, enhanced: false, iv: Some(IvBacking { res, slots }) })
}

pub fn load(data: &[u8]) -> Result<Dict> {
    if crate::rsc5::is_rsc5(data) {
        return load_iv_at(crate::rsc5::Rsc5::parse(data)?, 0);
    }
    let edition = Edition::of_file("ytd", data).map_err(err)?;
    let enhanced = edition == Some(Edition::Gen9);
    let textures = if enhanced {
        let legacy = rage_formats::convert_ytd_to_legacy(data).map_err(err)?;
        ytd::parse_ytd(&legacy).map_err(err)?
    } else {
        ytd::parse_ytd(data).map_err(err)?
    };
    Ok(Dict { textures, enhanced, iv: None })
}

pub fn format_name(f: TextureFormat) -> &'static str {
    match f {
        TextureFormat::A8R8G8B8 => "A8R8G8B8",
        TextureFormat::X8R8G8B8 => "X8R8G8B8",
        TextureFormat::A1R5G5B5 => "A1R5G5B5",
        TextureFormat::A8 => "A8",
        TextureFormat::A8B8G8R8 => "A8B8G8R8",
        TextureFormat::L8 => "L8",
        TextureFormat::DXT1 => "DXT1",
        TextureFormat::DXT3 => "DXT3",
        TextureFormat::DXT5 => "DXT5",
        TextureFormat::ATI1 => "ATI1 (BC4)",
        TextureFormat::ATI2 => "ATI2 (BC5)",
        TextureFormat::BC7 => "BC7",
        TextureFormat::Unknown => "unknown",
    }
}

/// Top mip level as RGBA8.
pub fn rgba(t: &YtdTexture) -> Result<Vec<u8>> {
    texture_utils::decompress_texture(t).map_err(err)
}

pub fn to_png(t: &YtdTexture) -> Result<Vec<u8>> {
    let img = image::RgbaImage::from_raw(t.width as u32, t.height as u32, rgba(t)?)
        .ok_or_else(|| Error::Invalid(format!("{}: bad pixel data", t.name)))?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).map_err(err)?;
    Ok(out.into_inner())
}

pub fn to_dds(t: &YtdTexture) -> Vec<u8> {
    t.to_dds()
}

/// The encode format matching an existing texture, if it can be encoded.
fn encode_format_of(f: TextureFormat) -> Option<EncodeFormat> {
    Some(match f {
        TextureFormat::DXT1 => EncodeFormat::Bc1,
        TextureFormat::DXT3 | TextureFormat::DXT5 => EncodeFormat::Bc3,
        TextureFormat::ATI1 => EncodeFormat::Bc4,
        TextureFormat::ATI2 => EncodeFormat::Bc5,
        TextureFormat::BC7 => EncodeFormat::Bc7,
        TextureFormat::A8R8G8B8 | TextureFormat::X8R8G8B8 | TextureFormat::A8B8G8R8 => EncodeFormat::Rgba8,
        _ => return None,
    })
}

/// Build a texture named `name` from an image file's bytes: a `.dds` is
/// used as is; PNG/JPEG/TGA/BMP/WebP are encoded (to `format`, or to the
/// format of the texture being replaced, or picked from the image).
pub fn texture_from_file(name: &str, file: &[u8], format: Option<EncodeFormat>) -> Result<YtdTexture> {
    if file.starts_with(b"DDS ") {
        let mut t = rage_formats::parse_dds(file).map_err(err)?;
        t.name = name.to_string();
        t.name_hash = rage_formats::rage_joaat(&name.to_ascii_lowercase());
        return Ok(t);
    }
    let img = image::load_from_memory(file).map_err(|e| Error::Invalid(format!("can't read image: {e}")))?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || w > 16384 || h > 16384 {
        return Err(Error::Invalid(format!("unsupported image size {w}x{h}")));
    }
    let raw = img.into_raw();
    let fmt = format.unwrap_or_else(|| texture_encode::auto_format(name, &raw));
    if fmt != EncodeFormat::Rgba8 && (w % 4 != 0 || h % 4 != 0) {
        return Err(Error::Invalid(format!("{w}x{h}: compressed textures need sizes that are multiples of 4")));
    }
    texture_encode::encode_texture(name, &raw, w, h, fmt, None).map_err(err)
}

impl Dict {
    fn check_writable(&self) -> Result<()> {
        if self.enhanced {
            return Err(Error::Invalid("editing Enhanced (gen9) texture dictionaries isn't supported yet".into()));
        }
        Ok(())
    }

    fn check_not_iv(&self, what: &str) -> Result<()> {
        if self.iv.is_some() {
            return Err(Error::Invalid(format!("{what} isn't supported for GTA IV dictionaries; replace textures instead")));
        }
        Ok(())
    }

    /// Can textures be added, removed and renamed (not just replaced)?
    pub fn can_restructure(&self) -> bool {
        !self.enhanced && self.iv.is_none()
    }

    /// GTA IV: replace in place. The new texture is converted to the old
    /// one's size, format and mip count, so the resource layout is unchanged.
    fn replace_iv(&mut self, i: usize, file: &[u8]) -> Result<()> {
        let old = &self.textures[i];
        let fmt = encode_format_of(old.format).ok_or_else(|| Error::Invalid(format!("{}: can't encode {}", old.name, format_name(old.format))))?;
        let (w, h) = (old.width as u32, old.height as u32);
        let new = if file.starts_with(b"DDS ") {
            let t = rage_formats::parse_dds(file).map_err(err)?;
            if (t.width, t.height, t.format) != (old.width, old.height, old.format) {
                return Err(Error::Invalid(format!(
                    "the DDS is {}x{} {}, but {} must stay {}x{} {}; use a PNG to convert automatically",
                    t.width, t.height, format_name(t.format), old.name, w, h, format_name(old.format)
                )));
            }
            t
        } else {
            let img = image::load_from_memory(file).map_err(|e| Error::Invalid(format!("can't read image: {e}")))?.to_rgba8();
            let img = if img.dimensions() != (w, h) { image::imageops::resize(&img, w, h, image::imageops::FilterType::Lanczos3) } else { img };
            texture_encode::encode_texture(&old.name, &img.into_raw(), w, h, fmt, None).map_err(err)?
        };
        let (_, len) = self.iv.as_ref().unwrap().slots[i];
        if new.pixel_data.len() < len {
            return Err(Error::Invalid(format!("{}: the new texture has fewer mip levels than the original ({})", old.name, old.levels)));
        }
        let t = &mut self.textures[i];
        t.pixel_data = new.pixel_data[..len].to_vec();
        Ok(())
    }

    /// Write edited GTA IV textures back into the resource.
    fn save_iv(&self) -> Result<Vec<u8>> {
        let iv = self.iv.as_ref().unwrap();
        let mut res = crate::rsc5::Rsc5 { magic: iv.res.magic, rtype: iv.res.rtype, flags: iv.res.flags, system: iv.res.system.clone(), graphics: iv.res.graphics.clone() };
        for (t, &(off, len)) in self.textures.iter().zip(&iv.slots) {
            res.graphics[off..off + len].copy_from_slice(&t.pixel_data[..len]);
        }
        res.to_bytes()
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.textures.iter().position(|t| t.name.eq_ignore_ascii_case(name))
    }

    /// Replace a texture from an image file, keeping its name (and its
    /// format, unless the image is a DDS).
    pub fn replace(&mut self, name: &str, file: &[u8]) -> Result<()> {
        self.check_writable()?;
        let i = self.find(name).ok_or_else(|| Error::NotFound(format!("texture {name}")))?;
        if self.iv.is_some() {
            return self.replace_iv(i, file);
        }
        let old_name = self.textures[i].name.clone();
        let fmt = encode_format_of(self.textures[i].format);
        self.textures[i] = texture_from_file(&old_name, file, fmt)?;
        Ok(())
    }

    /// Add a new texture (or replace one with the same name).
    pub fn add(&mut self, name: &str, file: &[u8]) -> Result<()> {
        self.check_writable()?;
        if self.iv.is_some() && self.find(name).is_none() {
            self.check_not_iv("adding textures")?;
        }
        if self.find(name).is_some() {
            return self.replace(name, file);
        }
        let t = texture_from_file(name, file, None)?;
        self.textures.push(t);
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        self.check_writable()?;
        self.check_not_iv("removing textures")?;
        let i = self.find(name).ok_or_else(|| Error::NotFound(format!("texture {name}")))?;
        self.textures.remove(i);
        Ok(())
    }

    pub fn rename(&mut self, name: &str, new_name: &str) -> Result<()> {
        self.check_writable()?;
        self.check_not_iv("renaming textures")?;
        if self.find(new_name).is_some() {
            return Err(Error::Invalid(format!("{new_name} already exists")));
        }
        let i = self.find(name).ok_or_else(|| Error::NotFound(format!("texture {name}")))?;
        self.textures[i].name = new_name.to_string();
        self.textures[i].name_hash = rage_formats::rage_joaat(&new_name.to_ascii_lowercase());
        Ok(())
    }

    /// Build the `.ytd` file.
    pub fn save(&self) -> Result<Vec<u8>> {
        self.check_writable()?;
        if self.iv.is_some() {
            return self.save_iv();
        }
        let mut tex = self.textures.clone();
        // The game looks textures up by hash with a binary search.
        tex.sort_by_key(|t| t.name_hash);
        let out = ytd::serialize_ytd(&tex).map_err(err)?;
        // Must read back.
        load(&out)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32, alpha: bool) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 8) as u8, (y * 8) as u8, 128, if alpha { (x * 4) as u8 } else { 255 }]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn build_edit_and_read_back() {
        let mut d = Dict { textures: vec![], enhanced: false, iv: None };
        d.add("vehicle_generic_d", &png(64, 32, false)).unwrap();
        d.add("vehicle_glass", &png(32, 32, true)).unwrap();
        let bytes = d.save().unwrap();
        assert!(is_ytd(&bytes));
        let mut d2 = load(&bytes).unwrap();
        assert_eq!(d2.textures.len(), 2);
        let i = d2.find("VEHICLE_GENERIC_D").unwrap();
        assert_eq!((d2.textures[i].width, d2.textures[i].height), (64, 32));
        assert_eq!(d2.textures[i].format, TextureFormat::DXT1);
        assert!(d2.textures[i].levels > 1);
        let g = d2.find("vehicle_glass").unwrap();
        assert_eq!(d2.textures[g].format, TextureFormat::DXT5);
        // PNG and DDS export.
        let p = to_png(&d2.textures[i]).unwrap();
        assert_eq!(&p[1..4], b"PNG");
        let dds = to_dds(&d2.textures[i]);
        assert_eq!(&dds[..4], b"DDS ");
        // Replace from DDS keeps the name; replace from PNG keeps the format.
        d2.replace("vehicle_glass", &dds).unwrap();
        d2.replace("vehicle_generic_d", &png(128, 128, true)).unwrap();
        d2.rename("vehicle_glass", "vehicle_glass2").unwrap();
        let d3 = load(&d2.save().unwrap()).unwrap();
        let i = d3.find("vehicle_generic_d").unwrap();
        assert_eq!((d3.textures[i].width, d3.textures[i].format), (128, TextureFormat::DXT1));
        assert!(d3.find("vehicle_glass2").is_some());
        let rgba = rgba(&d3.textures[i]).unwrap();
        assert_eq!(rgba.len(), 128 * 128 * 4);
    }

    #[test]
    fn rejects_bad_sizes() {
        let mut d = Dict { textures: vec![], enhanced: false, iv: None };
        assert!(d.add("odd", &png(30, 30, false)).is_err());
        let mut e = Dict { textures: vec![], enhanced: true, iv: None };
        assert!(e.add("x", &png(32, 32, false)).is_err());
    }
}
