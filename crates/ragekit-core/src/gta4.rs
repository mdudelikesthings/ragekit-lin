//! GTA IV / Episodes from Liberty City archives: RPF2, RPF3 (audio) and IMG3.
//!
//! Layout facts from the GTAMods wiki and SparkIV's documentation of the
//! formats; this is an independent implementation.
//!
//! * RPF2/RPF3: 20-byte header, table of contents at 0x800 (AES-256 applied
//!   16 times when encrypted), file data at byte offsets aligned to 0x800.
//!   RPF3 stores Jenkins hashes instead of names.
//! * IMG3: 20-byte header followed directly by the table of contents (both
//!   encrypted the same way when the magic doesn't match), flat list of
//!   files in 0x800-byte blocks.

use std::io::Write;
use std::sync::Arc;

use crate::blob::Blob;
use crate::crypto::iv_decrypt;
use crate::keys::Keys;
use crate::tree::{Counting, Dir, FileData, Format, Node, Source, Tree};
use crate::{Error, Result};

pub const RPF2_MAGIC: u32 = 0x3246_5052; // "RPF2"
pub const RPF3_MAGIC: u32 = 0x3346_5052; // "RPF3"
pub const IMG3_MAGIC: u32 = 0xA94E_2A52;
const SECTOR: u64 = 0x800;

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn cstr(b: &[u8], off: usize) -> Option<String> {
    let rest = b.get(off..)?;
    let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}

fn iv_key(keys: &Keys) -> Result<&[u8; 32]> {
    keys.iv_aes.as_ref().ok_or(Error::MissingKey("GTA IV AES key"))
}

/// Is this an IMG3 archive (plain or encrypted)? Needs the key to tell for
/// encrypted ones.
pub fn is_img3(data: &[u8], keys: &Keys) -> bool {
    if data.len() < 0x14 {
        return false;
    }
    if u32_at(data, 0) == IMG3_MAGIC {
        return true;
    }
    if let Some(k) = &keys.iv_aes {
        let mut h = data[..16].to_vec();
        iv_decrypt(&mut h, k);
        return u32_at(&h, 0) == IMG3_MAGIC;
    }
    false
}

/// Parse an RPF2 or RPF3 archive.
pub fn parse_rpf(data: Blob, keys: Arc<Keys>) -> Result<Tree> {
    let b = data.bytes();
    if b.len() < 20 {
        return Err(Error::Malformed("file too small".into()));
    }
    let magic = u32_at(b, 0);
    let format = match magic {
        RPF2_MAGIC => Format::Rpf2,
        RPF3_MAGIC => Format::Rpf3,
        _ => return Err(Error::Malformed("not an RPF2/RPF3 archive".into())),
    };
    let toc_size = u32_at(b, 4) as usize;
    let count = u32_at(b, 8) as usize;
    let unknown = u32_at(b, 12);
    let encrypted = u32_at(b, 16) != 0;
    if count == 0 || count.checked_mul(16).map_or(true, |n| n > toc_size) {
        return Err(Error::Malformed(format!("bad entry count {count}")));
    }
    let toc_end = (SECTOR as usize).checked_add(toc_size).filter(|&e| e <= b.len());
    let toc_end = toc_end.ok_or_else(|| Error::Malformed("table of contents runs past end of file".into()))?;
    let mut toc = b[SECTOR as usize..toc_end].to_vec();
    if encrypted {
        iv_decrypt(&mut toc, iv_key(&keys)?);
    }
    let names = &toc[count * 16..];

    struct E {
        name: String,
        w1: u32,
        w2: u32,
        w3: u32,
        dir: bool,
    }
    let mut ents = Vec::with_capacity(count);
    for i in 0..count {
        let e = &toc[i * 16..i * 16 + 16];
        let (w0, w1, w2, w3) = (u32_at(e, 0), u32_at(e, 4), u32_at(e, 8), u32_at(e, 12));
        let dir = (w2 as i32) < 0;
        let name = if format == Format::Rpf2 {
            cstr(names, w0 as usize).ok_or_else(|| Error::Malformed(format!("entry {i}: bad name offset (wrong key?)")))?
        } else if i == 0 {
            String::new()
        } else if dir {
            format!("0x{w0:08x}")
        } else {
            format!("0x{w0:08x}.ivaud")
        };
        ents.push(E { name, w1, w2, w3, dir });
    }
    if !ents[0].dir {
        return Err(Error::Malformed("first entry is not the root folder (wrong key?)".into()));
    }

    let mut seen = vec![false; count];
    fn build(ents: &[E], idx: usize, seen: &mut [bool], data: &Blob, format: Format) -> Result<Dir> {
        let e = &ents[idx];
        let first = (e.w2 & 0x7FFF_FFFF) as usize;
        let n = (e.w3 & 0x0FFF_FFFF) as usize;
        if first.checked_add(n).map_or(true, |end| end > ents.len()) {
            return Err(Error::Malformed(format!("folder {idx} has out-of-range children")));
        }
        let mut dir = Dir { flags: e.w1, ..Default::default() };
        for c in first..first + n {
            if seen[c] {
                return Err(Error::Malformed(format!("entry {c} appears twice")));
            }
            seen[c] = true;
            let ce = &ents[c];
            let node = if ce.dir {
                Node::Dir(build(ents, c, seen, data, format)?)
            } else {
                let size = ce.w1;
                let is_res = ce.w3 & 0xC000_0000 == 0xC000_0000;
                if is_res {
                    let offset = (ce.w2 & 0x7FFF_FF00) as usize;
                    let blob = data.slice(offset, size as usize).ok_or_else(|| {
                        Error::Malformed(format!("{}: data runs past end of archive", ce.name))
                    })?;
                    Node::File(FileData::Resource5 { rtype: ce.w2 & 0xFF, flags: ce.w3, data: Source::Blob(blob) })
                } else {
                    let stored = (ce.w3 & 0x3FFF_FFFF) as usize;
                    let compressed = ce.w3 & 0x4000_0000 != 0;
                    let offset = ce.w2 as usize;
                    let blob = data.slice(offset, stored).ok_or_else(|| {
                        Error::Malformed(format!("{}: data runs past end of archive", ce.name))
                    })?;
                    Node::File(FileData::Binary { stored: Source::Blob(blob), compressed, size })
                }
            };
            dir.children.insert(ce.name.to_ascii_lowercase(), (ce.name.clone(), node));
        }
        Ok(dir)
    }
    seen[0] = true;
    let root = build(&ents, 0, &mut seen, &data, format)?;
    let mut tree = Tree::new(format, keys);
    tree.root = root;
    tree.header_extra = unknown;
    tree.root_name = if format == Format::Rpf2 { ents[0].name.clone() } else { String::new() };
    tree.origin = format!("{} · {}", format.label(), if encrypted { "AES" } else { "unencrypted" });
    Ok(tree)
}

/// Parse an IMG3 archive.
pub fn parse_img(data: Blob, keys: Arc<Keys>) -> Result<Tree> {
    let b = data.bytes();
    if b.len() < 0x14 {
        return Err(Error::Malformed("file too small".into()));
    }
    let mut hdr = b[..0x14].to_vec();
    let encrypted = u32_at(&hdr, 0) != IMG3_MAGIC;
    if encrypted {
        iv_decrypt(&mut hdr, iv_key(&keys)?);
        if u32_at(&hdr, 0) != IMG3_MAGIC {
            return Err(Error::Malformed("not an IMG3 archive".into()));
        }
    }
    let version = u32_at(&hdr, 4);
    if version != 3 {
        return Err(Error::Malformed(format!("unsupported IMG version {version}")));
    }
    let count = u32_at(&hdr, 8) as usize;
    let toc_size = u32_at(&hdr, 12) as usize;
    let extra = u32_at(&hdr, 16);
    if count.checked_mul(16).map_or(true, |n| n > toc_size) {
        return Err(Error::Malformed(format!("bad entry count {count}")));
    }
    let toc_end = 0x14usize.checked_add(toc_size).filter(|&e| e <= b.len());
    let toc_end = toc_end.ok_or_else(|| Error::Malformed("table of contents runs past end of file".into()))?;
    let mut toc = b[0x14..toc_end].to_vec();
    if encrypted {
        iv_decrypt(&mut toc, iv_key(&keys)?);
    }
    let mut names = toc[count * 16..].split(|&c| c == 0).map(|s| String::from_utf8_lossy(s).into_owned());

    let mut root = Dir::default();
    for i in 0..count {
        let e = &toc[i * 16..i * 16 + 16];
        let w0 = u32_at(e, 0);
        let rtype = u32_at(e, 4);
        let block = u32_at(e, 8) as u64;
        let used = u16::from_le_bytes([e[12], e[13]]) as u64;
        let flags = u16::from_le_bytes([e[14], e[15]]) as u64;
        let is_res = w0 & 0xC000_0000 != 0;
        let size = if is_res { (used * SECTOR).saturating_sub(flags & 0x7FF) } else { w0 as u64 };
        let name = names.next().unwrap_or_default();
        let name = if name.is_empty() { format!("unnamed_{i}") } else { name };
        let blob = data
            .slice((block * SECTOR) as usize, size as usize)
            .ok_or_else(|| Error::Malformed(format!("{name}: data runs past end of archive")))?;
        let node = if is_res {
            FileData::Resource5 { rtype, flags: w0, data: Source::Blob(blob) }
        } else {
            FileData::Binary { stored: Source::Blob(blob), compressed: false, size: size as u32 }
        };
        root.children.insert(name.to_ascii_lowercase(), (name, Node::File(node)));
    }
    let mut tree = Tree::new(Format::Img3, keys);
    tree.root = root;
    tree.header_extra = extra;
    tree.origin = format!("IMG3 · {}", if encrypted { "AES" } else { "unencrypted" });
    Ok(tree)
}

/// Write an unencrypted RPF2 archive.
pub(crate) fn write_rpf2<W: Write + ?Sized>(tree: &Tree, w: &mut W) -> Result<u64> {
    let flat = tree.flatten();
    let mut names: Vec<u8> = Vec::new();
    let mut name_off = Vec::with_capacity(flat.len());
    let mut seen: std::collections::HashMap<&str, u32> = Default::default();
    for f in &flat {
        let off = *seen.entry(f.name).or_insert_with(|| {
            let o = names.len() as u32;
            names.extend_from_slice(f.name.as_bytes());
            names.push(0);
            o
        });
        name_off.push(off);
    }
    while names.len() % 16 != 0 {
        names.push(0);
    }
    let toc_size = flat.len() * 16 + names.len();

    let stored_len = |fd: &FileData| -> Result<u64> {
        Ok(match fd {
            FileData::Binary { stored, .. } => stored.len(),
            FileData::Resource5 { data, .. } => data.len(),
            FileData::Resource { .. } => return Err(Error::Invalid("GTA V resources can't go in a GTA IV archive".into())),
            FileData::Rpf(_) => return Err(Error::Invalid("GTA IV archives can't contain nested archives".into())),
        })
    };

    let mut next = (SECTOR + toc_size as u64).div_ceil(SECTOR) * SECTOR;
    let mut offsets = vec![0u64; flat.len()];
    for (i, f) in flat.iter().enumerate() {
        if let Some(fd) = f.file {
            offsets[i] = next;
            next += stored_len(fd)?.div_ceil(SECTOR).max(1) * SECTOR;
            if offsets[i] > 0x7FFF_FF00 {
                return Err(Error::Invalid("archive too large (over 2 GiB)".into()));
            }
        }
    }

    let mut toc = Vec::with_capacity(toc_size);
    for (i, f) in flat.iter().enumerate() {
        toc.extend_from_slice(&name_off[i].to_le_bytes());
        if let Some((d, first, count)) = f.dir {
            toc.extend_from_slice(&d.flags.to_le_bytes());
            toc.extend_from_slice(&(first | 0x8000_0000).to_le_bytes());
            toc.extend_from_slice(&count.to_le_bytes());
            continue;
        }
        let off = offsets[i] as u32;
        match f.file.unwrap() {
            FileData::Binary { stored, compressed, size } => {
                let s = stored.len() as u32;
                if s > 0x3FFF_FFFF {
                    return Err(Error::Invalid(format!("{}: file too large", f.name)));
                }
                toc.extend_from_slice(&size.to_le_bytes());
                toc.extend_from_slice(&off.to_le_bytes());
                toc.extend_from_slice(&(s | if *compressed { 0x4000_0000 } else { 0 }).to_le_bytes());
            }
            FileData::Resource5 { rtype, flags, data } => {
                toc.extend_from_slice(&(data.len() as u32).to_le_bytes());
                toc.extend_from_slice(&(off | (rtype & 0xFF)).to_le_bytes());
                toc.extend_from_slice(&(flags | 0xC000_0000).to_le_bytes());
            }
            _ => unreachable!("checked above"),
        }
    }
    toc.extend_from_slice(&names);

    let mut cw = Counting { inner: w, n: 0 };
    cw.put(&RPF2_MAGIC.to_le_bytes())?;
    cw.put(&(toc_size as u32).to_le_bytes())?;
    cw.put(&(flat.len() as u32).to_le_bytes())?;
    cw.put(&tree.header_extra.to_le_bytes())?;
    cw.put(&0u32.to_le_bytes())?; // not encrypted
    cw.pad_to(SECTOR)?;
    cw.put(&toc)?;
    for (i, f) in flat.iter().enumerate() {
        let Some(fd) = f.file else { continue };
        cw.pad_to(offsets[i])?;
        match fd {
            FileData::Binary { stored, .. } => cw.source(stored)?,
            FileData::Resource5 { data, .. } => cw.source(data)?,
            _ => unreachable!(),
        }
        cw.pad_to(offsets[i] + stored_len(fd)?.div_ceil(SECTOR).max(1) * SECTOR)?;
    }
    let end = cw.n.div_ceil(SECTOR) * SECTOR;
    cw.pad_to(end)?;
    Ok(cw.n)
}

/// Write an unencrypted IMG3 archive.
pub(crate) fn write_img<W: Write + ?Sized>(tree: &Tree, w: &mut W) -> Result<u64> {
    let mut files: Vec<(&String, &FileData)> = Vec::new();
    for (name, node) in tree.root.children.values() {
        match node {
            Node::File(fd) => files.push((name, fd)),
            Node::Dir(_) => return Err(Error::Invalid("IMG archives can't contain folders".into())),
        }
    }
    files.sort_by(|a, b| a.0.cmp(b.0));
    let mut names = Vec::new();
    for (n, _) in &files {
        names.extend_from_slice(n.as_bytes());
        names.push(0);
    }
    let toc_size = files.len() * 16 + names.len();
    let mut next_block = (0x14 + toc_size as u64).div_ceil(SECTOR);

    let mut toc = Vec::with_capacity(toc_size);
    let mut layout = Vec::with_capacity(files.len());
    for (name, fd) in &files {
        let (len, w0, rtype) = match fd {
            FileData::Binary { stored, compressed, .. } => {
                if *compressed {
                    return Err(Error::Invalid(format!("{name}: IMG archives store files uncompressed")));
                }
                (stored.len(), stored.len() as u32, 0u32)
            }
            FileData::Resource5 { rtype, flags, data } => (data.len(), *flags, *rtype),
            _ => return Err(Error::Invalid(format!("{name} can't go in a GTA IV archive"))),
        };
        let used = len.div_ceil(SECTOR).max(1);
        if used > 0xFFFF {
            return Err(Error::Invalid(format!("{name}: over 128 MiB, too large for an IMG archive")));
        }
        let padding = (used * SECTOR - len) as u16 & 0x7FF;
        toc.extend_from_slice(&w0.to_le_bytes());
        toc.extend_from_slice(&rtype.to_le_bytes());
        toc.extend_from_slice(&(next_block as u32).to_le_bytes());
        toc.extend_from_slice(&(used as u16).to_le_bytes());
        toc.extend_from_slice(&padding.to_le_bytes());
        layout.push(next_block);
        next_block += used;
    }
    toc.extend_from_slice(&names);

    let mut cw = Counting { inner: w, n: 0 };
    cw.put(&IMG3_MAGIC.to_le_bytes())?;
    cw.put(&3u32.to_le_bytes())?;
    cw.put(&(files.len() as u32).to_le_bytes())?;
    cw.put(&(toc_size as u32).to_le_bytes())?;
    let extra = if tree.header_extra == 0 { 16 } else { tree.header_extra }; // entry size 16 in low half
    cw.put(&extra.to_le_bytes())?;
    cw.put(&toc)?;
    for ((_, fd), block) in files.iter().zip(layout) {
        cw.pad_to(block * SECTOR)?;
        match fd {
            FileData::Binary { stored, .. } => cw.source(stored)?,
            FileData::Resource5 { data, .. } => cw.source(data)?,
            _ => unreachable!(),
        }
    }
    let end = cw.n.div_ceil(SECTOR) * SECTOR;
    cw.pad_to(end)?;
    Ok(cw.n)
}
