//! GTA IV resources (RSC5): the container of `.wtd`, `.wdr`, `.wdd`, `.wft`.
//!
//! A 12-byte header (magic, type, flags) is followed by one compressed
//! stream (zlib on PC) holding the system memory then the graphics memory.
//! Inside, 32-bit pointers carry their memory in the top nibble: 0x5… is
//! system memory, 0x6… graphics memory.
//!
//! Layout facts from SparkIV's documentation of the format; independent
//! implementation.

use std::io::{Read, Write};

use crate::{Error, Result};

pub struct Rsc5 {
    pub magic: [u8; 4],
    pub rtype: u32,
    pub flags: u32,
    pub system: Vec<u8>,
    pub graphics: Vec<u8>,
}

pub fn is_rsc5(data: &[u8]) -> bool {
    data.len() >= 12 && &data[..3] == b"RSC" && data[3] == 5
}

fn bad(what: impl Into<String>) -> Error {
    Error::Malformed(what.into())
}

impl Rsc5 {
    pub fn system_size(flags: u32) -> usize {
        ((flags & 0x7FF) as usize) << (((flags >> 11) & 0xF) + 8)
    }
    pub fn graphics_size(flags: u32) -> usize {
        (((flags >> 15) & 0x7FF) as usize) << (((flags >> 26) & 0xF) + 8)
    }

    pub fn parse(data: &[u8]) -> Result<Rsc5> {
        if !is_rsc5(data) {
            return Err(bad("not a GTA IV (RSC5) resource"));
        }
        let rd = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        let (rtype, flags) = (rd(4), rd(8));
        let (ss, gs) = (Self::system_size(flags), Self::graphics_size(flags));
        if ss + gs > 512 << 20 {
            return Err(bad("resource claims more than 512 MiB"));
        }
        let body = &data[12..];
        let mut out = Vec::with_capacity(ss + gs);
        let ok = flate2::read::ZlibDecoder::new(body).take((ss + gs) as u64).read_to_end(&mut out).is_ok() && !out.is_empty();
        if !ok {
            out.clear();
            if flate2::read::DeflateDecoder::new(body).take((ss + gs) as u64).read_to_end(&mut out).is_err() || out.is_empty() {
                out = body.to_vec();
            }
        }
        out.resize(ss + gs, 0);
        let graphics = out.split_off(ss);
        Ok(Rsc5 { magic: data[..4].try_into().unwrap(), rtype, flags, system: out, graphics })
    }

    /// Rebuild the resource file (same type and sizes).
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.magic);
        out.extend(self.rtype.to_le_bytes());
        out.extend(self.flags.to_le_bytes());
        let mut z = flate2::write::ZlibEncoder::new(out, flate2::Compression::best());
        z.write_all(&self.system)?;
        z.write_all(&self.graphics)?;
        Ok(z.finish()?)
    }

    pub fn u32(&self, sys_off: usize) -> Result<u32> {
        self.system.get(sys_off..sys_off + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| bad(format!("read past system memory at {sys_off:#x}")))
    }
    pub fn u16(&self, sys_off: usize) -> Result<u16> {
        self.system.get(sys_off..sys_off + 2).map(|b| u16::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| bad(format!("read past system memory at {sys_off:#x}")))
    }
    pub fn u8(&self, sys_off: usize) -> Result<u8> {
        self.system.get(sys_off).copied().ok_or_else(|| bad(format!("read past system memory at {sys_off:#x}")))
    }
    pub fn f32(&self, sys_off: usize) -> Result<f32> {
        Ok(f32::from_bits(self.u32(sys_off)?))
    }

    /// Follow a system pointer stored at `sys_off`; `None` for null.
    pub fn ptr(&self, sys_off: usize) -> Result<Option<usize>> {
        match self.u32(sys_off)? {
            0 => Ok(None),
            p if p >> 28 == 5 => Ok(Some((p & 0x0FFF_FFFF) as usize)),
            p => Err(bad(format!("expected a system pointer at {sys_off:#x}, found {p:#010x}"))),
        }
    }
    /// Follow a graphics pointer stored at `sys_off`.
    pub fn gfx_ptr(&self, sys_off: usize) -> Result<Option<usize>> {
        match self.u32(sys_off)? {
            0 => Ok(None),
            p if p >> 28 == 6 => Ok(Some((p & 0x0FFF_FFFF) as usize)),
            p => Err(bad(format!("expected a graphics pointer at {sys_off:#x}, found {p:#010x}"))),
        }
    }
    pub fn cstr(&self, sys_off: usize) -> Result<String> {
        let rest = self.system.get(sys_off..).ok_or_else(|| bad("string past end"))?;
        let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len()).min(1024);
        Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
    }
    /// A pointer collection (`ptr, count u16, size u16`) at `sys_off`:
    /// returns the system offsets of the items.
    pub fn ptr_collection(&self, sys_off: usize) -> Result<Vec<usize>> {
        let Some(list) = self.ptr(sys_off)? else { return Ok(Vec::new()) };
        let n = self.u16(sys_off + 4)? as usize;
        if n > 1 << 16 {
            return Err(bad("collection too large"));
        }
        let mut v = Vec::with_capacity(n);
        for i in 0..n {
            if let Some(p) = self.ptr(list + i * 4)? {
                v.push(p);
            }
        }
        Ok(v)
    }
    pub fn graphics_slice(&self, off: usize, len: usize) -> Result<&[u8]> {
        self.graphics.get(off..off.saturating_add(len)).ok_or_else(|| bad(format!("{len} bytes at graphics {off:#x} run past the end")))
    }
}

// ---- texture dictionaries (.wtd) ----------------------------------------

/// One texture of a GTA IV dictionary, with where its pixels live.
pub struct IvTexture {
    pub name: String,
    pub width: u16,
    pub height: u16,
    /// D3D format: FourCC for DXTn, 0x15 A8R8G8B8, 0x32 L8.
    pub format: u32,
    pub levels: u8,
    pub data_offset: usize,
    pub data_len: usize,
}

/// Size in bytes of a mip chain.
pub fn mip_chain_size(format: u32, w: u32, h: u32, levels: u8) -> Option<usize> {
    let (block, bpb) = match format {
        0x3154_5844 => (4, 8),            // DXT1
        0x3354_5844 | 0x3554_5844 => (4, 16), // DXT3, DXT5
        0x15 => (1, 4),
        0x32 => (1, 1),
        _ => return None,
    };
    let (mut w, mut h, mut total) = (w as usize, h as usize, 0usize);
    for _ in 0..levels.max(1) {
        let bw = w.div_ceil(block).max(1);
        let bh = h.div_ceil(block).max(1);
        total += bw * bh * bpb;
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    Some(total)
}

/// Read a texture dictionary at system offset `at` (0 for a `.wtd`).
pub fn texture_dict_at(r: &Rsc5, at: usize) -> Result<Vec<IvTexture>> {
    // pgDictionary: vtable, blockmap, parent, usage, hashes (ptr,n,cap), textures (ptr,n,cap)
    let mut out = Vec::new();
    for info in r.ptr_collection(at + 24)? {
        // grcTexturePC
        let name_ptr = r.ptr(info + 20)?;
        let mut name = match name_ptr {
            Some(p) => r.cstr(p)?,
            None => String::new(),
        };
        // Names are stored as "pack:/name.dds".
        if let Some(i) = name.rfind([':', '/']) {
            name = name[i + 1..].to_string();
        }
        if let Some(stem) = name.strip_suffix(".dds") {
            name = stem.to_string();
        }
        let width = r.u16(info + 28)?;
        let height = r.u16(info + 30)?;
        let format = r.u32(info + 32)?;
        let levels = r.u8(info + 39)?;
        let data_offset = r.gfx_ptr(info + 72)?.ok_or_else(|| bad(format!("texture {name} has no pixel data")))?;
        let data_len = mip_chain_size(format, width as u32, height as u32, levels)
            .ok_or_else(|| bad(format!("texture {name}: unsupported format {format:#x}")))?;
        r.graphics_slice(data_offset, data_len)?;
        out.push(IvTexture { name, width, height, format, levels, data_offset, data_len });
    }
    Ok(out)
}
