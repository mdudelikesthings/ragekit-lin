//! Red Dead Redemption archives (RPF6), read-only.
//!
//! RPF6 comes from the Xbox 360 / PS3 releases: big-endian, a 16-byte
//! header, then 20-byte entries (AES-256 applied 16 times when encrypted).
//! Offsets are in 8-byte units. Entry names are Jenkins hashes; archives
//! built with debug data carry a name table, which is used when present.
//!
//! Layout from RPFTool (twisted89) and rpf-archive (VIRUXE, Unlicense);
//! this is an independent implementation.

use std::sync::Arc;

use crate::blob::Blob;
use crate::crypto::iv_decrypt;
use crate::keys::Keys;
use crate::tree::{Dir, FileData, Format, Node, Source, Tree};
use crate::{Error, Result};

/// "RPF6" as stored on disk.
pub const RPF6_BYTES: &[u8; 4] = b"RPF6";
const HEADER: usize = 0x10;

fn be(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}

fn cstr(b: &[u8], off: usize) -> Option<String> {
    let rest = b.get(off..)?;
    let end = rest.iter().position(|&c| c == 0)?;
    let s = std::str::from_utf8(&rest[..end]).ok()?;
    (!s.is_empty()).then(|| s.to_string())
}

pub fn is_rpf6(data: &[u8]) -> bool {
    data.len() >= HEADER && &data[..4] == RPF6_BYTES
}

/// Key probe for an encrypted RPF6 archive; see [`crate::gta4::rpf_key_probe`].
pub fn key_probe(head: &[u8]) -> Option<impl Fn(&[u8; 32]) -> bool + Sync + Send + use<>> {
    if !is_rpf6(head) || head.len() < HEADER + 16 || be(head, 12) == 0 {
        return None;
    }
    let count = be(head, 4);
    let block: [u8; 16] = head[HEADER..HEADER + 16].try_into().unwrap();
    Some(move |key: &[u8; 32]| crate::gta4::root_entry_ok(&crate::crypto::iv_decrypt_block(block, key), count, true))
}

pub fn parse(data: Blob, keys: Arc<Keys>) -> Result<Tree> {
    let b = data.bytes();
    if !is_rpf6(b) {
        return Err(Error::Malformed("not an RPF6 archive".into()));
    }
    let count = be(b, 4) as usize;
    let debug_off = be(b, 8) as usize * 8;
    let encrypted = be(b, 12) != 0;
    if count == 0 || count > 1 << 24 {
        return Err(Error::Malformed(format!("bad entry count {count}")));
    }
    let toc_len = (count * 20).div_ceil(16) * 16;
    let toc = b.get(HEADER..HEADER + toc_len).ok_or_else(|| Error::Malformed("table of contents runs past end of file".into()))?;
    let mut toc = toc.to_vec();
    if encrypted {
        let key = keys.rdr_aes.as_ref().ok_or(Error::MissingKey("Red Dead Redemption AES key"))?;
        iv_decrypt(&mut toc, key);
    }

    // Optional debug name table: `count` 8-byte records (name offset first),
    // then the strings.
    let names: Option<(&[u8], &[u8])> = (debug_off != 0 && debug_off + count * 8 <= b.len())
        .then(|| (&b[debug_off..debug_off + count * 8], &b[debug_off + count * 8..]));

    struct E {
        name: String,
        w1: u32,
        w2: u32,
        w3: u32,
        dir: bool,
    }
    let mut ents = Vec::with_capacity(count);
    for i in 0..count {
        let e = &toc[i * 20..i * 20 + 20];
        let (w0, w1, w2, w3) = (be(e, 0), be(e, 4), be(e, 8), be(e, 12));
        let dir = w2 & 0x8000_0000 != 0;
        let real = names.and_then(|(recs, strs)| cstr(strs, be(recs, i * 8) as usize));
        let name = real.unwrap_or_else(|| if i == 0 { String::new() } else { format!("0x{w0:08x}") });
        ents.push(E { name, w1, w2, w3, dir });
    }
    if !ents[0].dir {
        return Err(Error::Malformed("first entry is not the root folder (wrong key?)".into()));
    }

    let mut seen = vec![false; count];
    seen[0] = true;
    fn build(ents: &[E], idx: usize, seen: &mut [bool], data: &Blob) -> Result<Dir> {
        let e = &ents[idx];
        let first = (e.w2 & 0x7FFF_FFFF) as usize;
        let n = (e.w3 & 0x0FFF_FFFF) as usize;
        if first.checked_add(n).is_none_or(|end| end > ents.len()) {
            return Err(Error::Malformed(format!("folder {idx} has out-of-range children (wrong key?)")));
        }
        let mut dir = Dir { flags: e.w1, ..Default::default() };
        for c in first..first + n {
            if std::mem::replace(&mut seen[c], true) {
                return Err(Error::Malformed(format!("entry {c} appears twice")));
            }
            let ce = &ents[c];
            let node = if ce.dir {
                Node::Dir(build(ents, c, seen, data)?)
            } else {
                let on_disk = (ce.w1 & 0x7FFF_FFFF) as usize;
                let is_res = ce.w3 & 0x8000_0000 != 0;
                let offset = if is_res { (ce.w2 & 0x7FFF_FF00) as usize } else { (ce.w2 & 0x7FFF_FFFF) as usize } * 8;
                let blob = data
                    .slice(offset, on_disk)
                    .ok_or_else(|| Error::Malformed(format!("{}: data runs past end of archive", ce.name)))?;
                Node::File(if is_res {
                    FileData::Resource5 { rtype: ce.w2 & 0xFF, flags: ce.w3, data: Source::Blob(blob) }
                } else {
                    let compressed = ce.w3 & 0x4000_0000 != 0;
                    let size = if compressed { ce.w3 & 0x3FFF_FFFF } else { on_disk as u32 };
                    FileData::Binary { stored: Source::Blob(blob), compressed, size }
                })
            };
            let key = if ce.name.is_empty() { format!("0x{c:08x}") } else { ce.name.to_ascii_lowercase() };
            dir.children.insert(key, (ce.name.clone(), node));
        }
        Ok(dir)
    }
    let root = build(&ents, 0, &mut seen, &data)?;
    let mut tree = Tree::new(Format::Rpf6, keys);
    tree.root = root;
    tree.origin = format!(
        "RPF6 · {}{}",
        if encrypted { "AES" } else { "unencrypted" },
        if names.is_some() { "" } else { " · hashed names" }
    );
    Ok(tree)
}
