//! RPF7 archive reader.

use std::io::Read;
use std::path::Path;

use crate::blob::Blob;
use crate::crypto::{aes_decrypt, ng_decrypt_named};
use crate::keys::Keys;
use crate::resource::{self, size_from_flags};
use crate::{normalize_path, Error, Result};

pub const RPF7_MAGIC: u32 = 0x5250_4637; // "RPF7" read little-endian
pub const BLOCK: u64 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    None,
    /// OpenIV-style archive: table of contents in the clear.
    Open,
    Aes,
    Ng,
}

impl Encryption {
    pub fn from_u32(v: u32) -> Encryption {
        match v {
            0 => Encryption::None,
            0x4E45_504F => Encryption::Open,
            0x0FFF_FFF9 => Encryption::Aes,
            _ => Encryption::Ng, // 0x0FEFFFFF; unknown values behave as NG in the game
        }
    }
    pub fn to_u32(self) -> u32 {
        match self {
            Encryption::None => 0,
            Encryption::Open => 0x4E45_504F,
            Encryption::Aes => 0x0FFF_FFF9,
            Encryption::Ng => 0x0FEF_FFFF,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Encryption::None => "none",
            Encryption::Open => "OPEN",
            Encryption::Aes => "AES",
            Encryption::Ng => "NG",
        }
    }
}

#[derive(Debug, Clone)]
pub enum EntryKind {
    Directory {
        first: u32,
        count: u32,
    },
    Binary {
        /// Offset in 512-byte blocks from the archive start.
        offset: u32,
        /// Stored (compressed) size; 0 means stored uncompressed.
        size: u32,
        uncompressed_size: u32,
        encrypted: bool,
    },
    Resource {
        offset: u32,
        /// Stored size including the 16-byte RSC7 header.
        size: u32,
        system_flags: u32,
        graphics_flags: u32,
        encrypted: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub parent: Option<usize>,
    pub kind: EntryKind,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Directory { .. })
    }
    pub fn is_rpf(&self) -> bool {
        matches!(self.kind, EntryKind::Binary { .. }) && self.name.to_ascii_lowercase().ends_with(".rpf")
    }
    /// Size of the file once extracted.
    pub fn extracted_size(&self) -> u64 {
        match self.kind {
            EntryKind::Directory { .. } => 0,
            EntryKind::Binary { uncompressed_size, .. } => uncompressed_size as u64,
            EntryKind::Resource { size, system_flags, graphics_flags, .. } => {
                if size == 0 {
                    16 + size_from_flags(system_flags) + size_from_flags(graphics_flags)
                } else {
                    size as u64
                }
            }
        }
    }
}

/// A parsed RPF7 archive. Cloning is cheap-ish (entry table is copied, data is shared).
#[derive(Debug, Clone)]
pub struct Archive {
    /// File name used for NG key selection (e.g. "x64a.rpf").
    pub name: String,
    pub data: Blob,
    pub encryption: Encryption,
    pub entries: Vec<Entry>,
}

fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn rd_u24(b: &[u8], o: usize) -> u32 {
    b[o] as u32 | (b[o + 1] as u32) << 8 | (b[o + 2] as u32) << 16
}

impl Archive {
    /// Open an archive file from disk (memory-mapped).
    pub fn open(path: &Path, keys: &Keys) -> Result<Archive> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Archive::parse(Blob::map_file(path)?, &name, keys)
    }

    /// Quick check for the RPF7 magic.
    pub fn is_rpf7(data: &[u8]) -> bool {
        data.len() >= 16 && rd_u32(data, 0) == RPF7_MAGIC
    }

    /// Parse an archive held in `data`. `name` is its file name, which (with
    /// its size) selects the NG key.
    pub fn parse(data: Blob, name: &str, keys: &Keys) -> Result<Archive> {
        let b = data.bytes();
        if !Self::is_rpf7(b) {
            return Err(Error::NotRpf7);
        }
        let count = rd_u32(b, 4) as usize;
        let names_len = rd_u32(b, 8) as usize;
        let encryption = Encryption::from_u32(rd_u32(b, 12));
        if count == 0 || count > 0x100_0000 {
            return Err(Error::Malformed(format!("entry count {count}")));
        }
        let toc_end = 16usize
            .checked_add(count * 16)
            .and_then(|v| v.checked_add(names_len))
            .filter(|&e| e <= b.len())
            .ok_or_else(|| Error::Malformed("table of contents runs past end of file".into()))?;
        let mut entries_raw = b[16..16 + count * 16].to_vec();
        let mut names_raw = b[16 + count * 16..toc_end].to_vec();

        match encryption {
            Encryption::None | Encryption::Open => {}
            Encryption::Aes => {
                let key = keys.aes.as_ref().ok_or(Error::MissingKey("AES key"))?;
                aes_decrypt(&mut entries_raw, key);
                aes_decrypt(&mut names_raw, key);
            }
            Encryption::Ng => {
                let len = b.len() as u32;
                ng_decrypt_named(keys, &mut entries_raw, name, len)?;
                ng_decrypt_named(keys, &mut names_raw, name, len)?;
            }
        }

        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let e = &entries_raw[i * 16..i * 16 + 16];
            let h2 = rd_u32(e, 4);
            let (name_off, kind) = if h2 == 0x7FFF_FF00 {
                (rd_u32(e, 0) as usize, EntryKind::Directory { first: rd_u32(e, 8), count: rd_u32(e, 12) })
            } else if h2 & 0x8000_0000 == 0 {
                let name_off = (e[0] as usize) | (e[1] as usize) << 8;
                let enc = rd_u32(e, 12);
                if enc > 1 {
                    return Err(Error::Malformed(format!(
                        "entry {i}: bad binary encryption flag {enc:#x} (wrong key?)"
                    )));
                }
                (
                    name_off,
                    EntryKind::Binary {
                        size: rd_u24(e, 2),
                        offset: rd_u24(e, 5),
                        uncompressed_size: rd_u32(e, 8),
                        encrypted: enc == 1,
                    },
                )
            } else {
                let name_off = (e[0] as usize) | (e[1] as usize) << 8;
                let offset = rd_u24(e, 5) & 0x7F_FFFF;
                let mut size = rd_u24(e, 2);
                if size == 0xFF_FFFF {
                    // Huge resource: real size is scattered through its RSC7 header.
                    let p = offset as usize * BLOCK as usize;
                    if p + 16 <= b.len() {
                        let h = &b[p..p + 16];
                        size = h[7] as u32 | (h[14] as u32) << 8 | (h[5] as u32) << 16 | (h[2] as u32) << 24;
                    }
                }
                (
                    name_off,
                    EntryKind::Resource {
                        offset,
                        size,
                        system_flags: rd_u32(e, 8),
                        graphics_flags: rd_u32(e, 12),
                        encrypted: false,
                    },
                )
            };
            let name = read_cstr(&names_raw, name_off)
                .ok_or_else(|| Error::Malformed(format!("entry {i}: name offset out of range (wrong key?)")))?;
            entries.push(Entry { name, parent: None, kind });
        }

        // .ysc resources are always encrypted; nothing in the entry says so.
        for e in entries.iter_mut() {
            if let EntryKind::Resource { encrypted, .. } = &mut e.kind {
                *encrypted = e.name.to_ascii_lowercase().ends_with(".ysc");
            }
        }

        if !entries[0].is_dir() {
            return Err(Error::Malformed("first entry is not the root directory (wrong key?)".into()));
        }

        // Link parents, guarding against cycles and bad ranges.
        let mut seen = vec![false; count];
        seen[0] = true;
        let mut stack = vec![0usize];
        while let Some(d) = stack.pop() {
            let EntryKind::Directory { first, count: n } = entries[d].kind else { continue };
            let (first, n) = (first as usize, n as usize);
            if first.checked_add(n).map_or(true, |end| end > count) {
                return Err(Error::Malformed(format!("directory {d} has out-of-range children")));
            }
            for c in first..first + n {
                if seen[c] {
                    return Err(Error::Malformed(format!("entry {c} appears twice in the tree")));
                }
                seen[c] = true;
                entries[c].parent = Some(d);
                if entries[c].is_dir() {
                    stack.push(c);
                }
            }
        }

        Ok(Archive { name: name.to_string(), data, encryption, entries })
    }

    /// Full path of an entry within this archive ("" for the root).
    pub fn path_of(&self, idx: usize) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(idx);
        while let Some(i) = cur {
            if i != 0 {
                parts.push(self.entries[i].name.as_str());
            }
            cur = self.entries[i].parent;
        }
        parts.reverse();
        parts.join("/")
    }

    /// Child indices of a directory entry.
    pub fn children(&self, dir: usize) -> Vec<usize> {
        match self.entries[dir].kind {
            EntryKind::Directory { first, count } => (first as usize..(first + count) as usize).collect(),
            _ => Vec::new(),
        }
    }

    /// Find an entry by path within this archive (case-insensitive).
    pub fn find(&self, path: &str) -> Option<usize> {
        let path = normalize_path(path);
        if path.is_empty() {
            return Some(0);
        }
        let mut cur = 0usize;
        for part in path.split('/') {
            cur = self.children(cur).into_iter().find(|&c| self.entries[c].name.eq_ignore_ascii_case(part))?;
        }
        Some(cur)
    }

    /// Indices of all entries, depth-first in path order.
    pub fn walk(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.entries.len());
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            out.push(i);
            let mut ch = self.children(i);
            ch.sort_by(|a, b| self.entries[*b].name.cmp(&self.entries[*a].name));
            stack.extend(ch);
        }
        out
    }

    fn stored_slice(&self, offset: u32, len: u64) -> Result<&[u8]> {
        let start = offset as u64 * BLOCK;
        let b = self.data.bytes();
        let end = start.checked_add(len).filter(|&e| e <= b.len() as u64).ok_or_else(|| {
            Error::Malformed(format!("file data at block {offset} runs past end of archive"))
        })?;
        Ok(&b[start as usize..end as usize])
    }

    fn decrypt_entry(&self, keys: &Keys, buf: &mut [u8], name: &str, ng_len: u32) -> Result<()> {
        match self.encryption {
            Encryption::Aes => {
                let key = keys.aes.as_ref().ok_or(Error::MissingKey("AES key"))?;
                aes_decrypt(buf, key);
                Ok(())
            }
            // OpenIV archives can carry NG-encrypted files: treat any non-AES archive as NG here.
            _ => ng_decrypt_named(keys, buf, name, ng_len),
        }
    }

    /// Extract a file as it would be saved on disk:
    /// * binary files: decrypted and decompressed;
    /// * resources: RSC7 header + decrypted, still-compressed payload (the
    ///   same form OpenIV and CodeWalker export, and what they import).
    pub fn extract(&self, idx: usize, keys: &Keys) -> Result<Vec<u8>> {
        let e = &self.entries[idx];
        match e.kind {
            EntryKind::Directory { .. } => Err(Error::Invalid(format!("{} is a directory", self.path_of(idx)))),
            EntryKind::Binary { offset, size, uncompressed_size, encrypted } => {
                let stored_len = if size == 0 { uncompressed_size } else { size } as u64;
                let mut buf = self.stored_slice(offset, stored_len)?.to_vec();
                if encrypted {
                    self.decrypt_entry(keys, &mut buf, &e.name, uncompressed_size)?;
                }
                if size == 0 {
                    return Ok(buf);
                }
                let mut out = Vec::with_capacity(uncompressed_size as usize);
                flate2::read::DeflateDecoder::new(&buf[..])
                    .read_to_end(&mut out)
                    .map_err(|err| Error::Malformed(format!("{}: decompression failed: {err}", self.path_of(idx))))?;
                Ok(out)
            }
            EntryKind::Resource { .. } => {
                let (sys, gfx, payload) = self.resource_payload(idx, keys)?;
                let mut out = Vec::with_capacity(16 + payload.len());
                out.extend_from_slice(&resource::header(sys, gfx));
                out.extend_from_slice(&payload);
                Ok(out)
            }
        }
    }

    /// For a binary file: the stored bytes, decrypted but still compressed.
    pub fn binary_stored(&self, idx: usize, keys: &Keys) -> Result<Vec<u8>> {
        let e = &self.entries[idx];
        let EntryKind::Binary { offset, size, uncompressed_size, encrypted } = e.kind else {
            return Err(Error::Invalid(format!("{} is not a binary file", self.path_of(idx))));
        };
        let stored_len = if size == 0 { uncompressed_size } else { size } as u64;
        let mut buf = self.stored_slice(offset, stored_len)?.to_vec();
        if encrypted {
            self.decrypt_entry(keys, &mut buf, &e.name, uncompressed_size)?;
        }
        Ok(buf)
    }

    /// For a resource: the payload exactly as stored (possibly encrypted), without header.
    pub fn resource_stored_raw(&self, idx: usize) -> Result<Vec<u8>> {
        let EntryKind::Resource { offset, size, .. } = self.entries[idx].kind else {
            return Err(Error::Invalid(format!("{} is not a resource", self.path_of(idx))));
        };
        if size < 16 {
            return Err(Error::Malformed(format!("{}: resource too small", self.path_of(idx))));
        }
        Ok(self.stored_slice(offset, size as u64)?[16..].to_vec())
    }

    /// For a resource: (system flags, graphics flags, decrypted compressed payload without header).
    pub fn resource_payload(&self, idx: usize, keys: &Keys) -> Result<(u32, u32, Vec<u8>)> {
        let e = &self.entries[idx];
        let EntryKind::Resource { offset, size, system_flags, graphics_flags, encrypted } = e.kind else {
            return Err(Error::Invalid(format!("{} is not a resource", self.path_of(idx))));
        };
        if size < 16 {
            return Err(Error::Malformed(format!("{}: resource too small", self.path_of(idx))));
        }
        let all = self.stored_slice(offset, size as u64)?;
        let mut payload = all[16..].to_vec();
        if encrypted {
            self.decrypt_entry(keys, &mut payload, &e.name, size)?;
        }
        Ok((system_flags, graphics_flags, payload))
    }

    /// For a resource: the fully decompressed (system + graphics) data.
    pub fn resource_decompressed(&self, idx: usize, keys: &Keys) -> Result<Vec<u8>> {
        let (_, _, payload) = self.resource_payload(idx, keys)?;
        let mut out = Vec::new();
        flate2::read::DeflateDecoder::new(&payload[..])
            .read_to_end(&mut out)
            .map_err(|err| Error::Malformed(format!("{}: decompression failed: {err}", self.path_of(idx))))?;
        Ok(out)
    }

    /// Raw stored bytes of a nested .rpf entry (zero-copy).
    pub fn nested_blob(&self, idx: usize) -> Result<Blob> {
        let e = &self.entries[idx];
        let EntryKind::Binary { offset, size, uncompressed_size, encrypted } = e.kind else {
            return Err(Error::Invalid(format!("{} is not a nested archive", self.path_of(idx))));
        };
        if size != 0 || encrypted {
            // Unusual: compressed or encrypted nested archive. Fall back to extraction.
            return Ok(Blob::from_vec(self.extract(idx, &Keys::default())?));
        }
        let start = offset as usize * BLOCK as usize;
        self.data
            .slice(start, uncompressed_size as usize)
            .ok_or_else(|| Error::Malformed(format!("{}: nested archive out of range", self.path_of(idx))))
    }

    /// Open a nested .rpf entry as an archive.
    pub fn open_nested(&self, idx: usize, keys: &Keys) -> Result<Archive> {
        let blob = self.nested_blob(idx)?;
        Archive::parse(blob, &self.entries[idx].name, keys)
    }

    /// Resolve a path that may pass through nested archives, e.g.
    /// `update/x64/dlcpacks/patchday1ng/dlc.rpf/x64/levels/foo.ytd`.
    /// Returns the innermost archive and the entry index within it.
    pub fn resolve(&self, path: &str, keys: &Keys) -> Result<(Archive, usize)> {
        let norm = normalize_path(path);
        let parts: Vec<&str> = if norm.is_empty() { vec![] } else { norm.split('/').collect() };
        let mut arc = self.clone();
        let mut cur = 0usize;
        for (pi, part) in parts.iter().enumerate() {
            if arc.entries[cur].is_rpf() {
                arc = arc.open_nested(cur, keys)?;
                cur = 0;
            }
            cur = arc
                .children(cur)
                .into_iter()
                .find(|&c| arc.entries[c].name.eq_ignore_ascii_case(part))
                .ok_or_else(|| Error::NotFound(parts[..=pi].join("/")))?;
        }
        Ok((arc, cur))
    }

    pub fn file_count(&self) -> usize {
        self.entries.iter().filter(|e| !e.is_dir()).count()
    }
}

fn read_cstr(b: &[u8], off: usize) -> Option<String> {
    let rest = b.get(off..)?;
    let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}
