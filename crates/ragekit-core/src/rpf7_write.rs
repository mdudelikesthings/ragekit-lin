//! GTA V RPF7 writer.

use std::io::Write;
use std::sync::Arc;

use crate::archive::{Encryption, BLOCK, RPF7_MAGIC};
use crate::crypto::aes_encrypt;
use crate::resource;
use crate::tree::{Counting, FileData, RpfData, Tree};
use crate::{Error, Result};

pub(crate) fn write<W: Write + ?Sized>(tree: &Tree, w: &mut W, enc: Encryption) -> Result<u64> {
    if enc == Encryption::Ng {
        return Err(Error::Invalid("writing NG-encrypted archives is not supported; use OPEN".into()));
    }
    let flat = tree.flatten();

    // Names table (deduplicated; root first so it sits at offset 0).
    let mut names: Vec<u8> = Vec::new();
    let mut seen: std::collections::HashMap<&str, u32> = Default::default();
    let mut name_off = Vec::with_capacity(flat.len());
    for f in &flat {
        let off = *seen.entry(f.name).or_insert_with(|| {
            let o = names.len() as u32;
            names.extend_from_slice(f.name.as_bytes());
            names.push(0);
            o
        });
        if f.file.is_some() && off > 0xFFFF {
            return Err(Error::Invalid("too many file names for one archive (name table over 64 KiB)".into()));
        }
        name_off.push(off);
    }
    while names.len() % 16 != 0 {
        names.push(0);
    }

    // Build expanded nested archives first so every size is known.
    let mut built: Vec<Option<Arc<Vec<u8>>>> = vec![None; flat.len()];
    for (i, f) in flat.iter().enumerate() {
        match f.file {
            Some(FileData::Rpf(RpfData::Tree(t))) => built[i] = Some(Arc::new(t.to_bytes()?)),
            Some(FileData::Resource5 { .. }) => {
                return Err(Error::Invalid(format!("{} is a GTA IV resource and can't go in a GTA V archive", f.name)))
            }
            _ => {}
        }
    }
    let stored_len = |i: usize| -> u64 {
        match flat[i].file {
            None => 0,
            Some(FileData::Binary { stored, .. }) => stored.len(),
            Some(FileData::Resource { payload, .. }) => 16 + payload.len(),
            Some(FileData::Resource5 { data, .. }) => data.len(),
            Some(FileData::Rpf(RpfData::Raw(b))) => b.len() as u64,
            Some(FileData::Rpf(RpfData::Tree(_))) => built[i].as_ref().unwrap().len() as u64,
        }
    };

    // Layout in 512-byte blocks.
    let header_len = 16 + 16 * flat.len() as u64 + names.len() as u64;
    let mut next = header_len.div_ceil(BLOCK);
    let mut offsets = vec![0u32; flat.len()];
    for i in 0..flat.len() {
        if flat[i].file.is_none() {
            continue;
        }
        if next > 0x7F_FFFF {
            return Err(Error::Invalid("archive too large (over 4 GiB)".into()));
        }
        offsets[i] = next as u32;
        next += stored_len(i).div_ceil(BLOCK).max(1);
    }

    // Entry table.
    let mut toc = Vec::with_capacity(flat.len() * 16);
    let u24 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&[x as u8, (x >> 8) as u8, (x >> 16) as u8]);
    for (i, f) in flat.iter().enumerate() {
        let no = name_off[i];
        if let Some((_, first, count)) = f.dir {
            toc.extend_from_slice(&no.to_le_bytes());
            toc.extend_from_slice(&0x7FFF_FF00u32.to_le_bytes());
            toc.extend_from_slice(&first.to_le_bytes());
            toc.extend_from_slice(&count.to_le_bytes());
            continue;
        }
        let off = offsets[i];
        toc.extend_from_slice(&(no as u16).to_le_bytes());
        match f.file.unwrap() {
            FileData::Binary { compressed, size, .. } => {
                let len = stored_len(i);
                if *compressed && len > 0xFF_FFFF {
                    return Err(Error::Invalid(format!("{}: compressed size over 16 MiB", f.name)));
                }
                u24(&mut toc, if *compressed { len as u32 } else { 0 });
                u24(&mut toc, off);
                toc.extend_from_slice(&size.to_le_bytes());
                toc.extend_from_slice(&0u32.to_le_bytes());
            }
            FileData::Rpf(_) => {
                let len = u32::try_from(stored_len(i)).map_err(|_| Error::Invalid(format!("nested archive {} over 4 GiB", f.name)))?;
                u24(&mut toc, 0);
                u24(&mut toc, off);
                toc.extend_from_slice(&len.to_le_bytes());
                toc.extend_from_slice(&0u32.to_le_bytes());
            }
            FileData::Resource { system_flags, graphics_flags, .. } => {
                u24(&mut toc, stored_len(i).min(0xFF_FFFF) as u32);
                u24(&mut toc, off | 0x80_0000);
                toc.extend_from_slice(&system_flags.to_le_bytes());
                toc.extend_from_slice(&graphics_flags.to_le_bytes());
            }
            FileData::Resource5 { .. } => unreachable!(),
        }
    }

    let mut names_out = names;
    if enc == Encryption::Aes {
        let key = tree.keys().aes.as_ref().ok_or(Error::MissingKey("AES key"))?;
        aes_encrypt(&mut toc, key);
        aes_encrypt(&mut names_out, key);
    }

    let mut cw = Counting { inner: w, n: 0 };
    cw.put(&RPF7_MAGIC.to_le_bytes())?;
    cw.put(&(flat.len() as u32).to_le_bytes())?;
    cw.put(&(names_out.len() as u32).to_le_bytes())?;
    cw.put(&enc.to_u32().to_le_bytes())?;
    cw.put(&toc)?;
    cw.put(&names_out)?;

    for (i, f) in flat.iter().enumerate() {
        let Some(file) = f.file else { continue };
        cw.pad_to(offsets[i] as u64 * BLOCK)?;
        match file {
            FileData::Binary { stored, .. } => cw.source(stored)?,
            FileData::Resource { system_flags, graphics_flags, payload, .. } => {
                let mut h = resource::header(*system_flags, *graphics_flags);
                let len = 16 + payload.len();
                if len >= 0xFF_FFFF {
                    let l = len as u32;
                    h[7] = l as u8;
                    h[14] = (l >> 8) as u8;
                    h[5] = (l >> 16) as u8;
                    h[2] = (l >> 24) as u8;
                }
                cw.put(&h)?;
                cw.source(payload)?;
            }
            FileData::Rpf(RpfData::Raw(b)) => cw.put(b.bytes())?,
            FileData::Rpf(RpfData::Tree(_)) => cw.put(built[i].as_ref().unwrap())?,
            FileData::Resource5 { .. } => unreachable!(),
        }
        cw.pad_to((offsets[i] as u64 + stored_len(i).div_ceil(BLOCK).max(1)) * BLOCK)?;
    }
    let end = cw.n.div_ceil(BLOCK).max(1) * BLOCK;
    cw.pad_to(end)?;
    Ok(cw.n)
}
