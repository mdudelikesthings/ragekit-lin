//! Editable archive model and RPF7 writer.
//!
//! Load a [`Tree`] from an existing [`Archive`] or a folder on disk, edit it,
//! then write it out. Output archives use OpenIV's "OPEN" layout (table of
//! contents in the clear, file data unencrypted), which the game accepts
//! through OpenIV.asi / OpenRPF / RageOpenV mod loaders.
//!
//! Unchanged nested archives are copied byte for byte (their own encryption
//! is kept, since it only depends on their name and size).

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use flate2::write::DeflateEncoder;
use flate2::Compression;

use crate::archive::{Archive, Encryption, EntryKind, BLOCK};
use crate::blob::Blob;
use crate::crypto::aes_encrypt;
use crate::keys::Keys;
use crate::resource;
use crate::{normalize_path, Error, Result};

/// Where a file's bytes come from.
#[derive(Clone)]
pub enum Source {
    Mem(Arc<Vec<u8>>),
    /// Binary entry in an archive: decrypted stored (compressed) bytes.
    ArchiveBinary { arc: Arc<Archive>, idx: usize, keys: Arc<Keys>, len: u64 },
    /// Resource entry in an archive: payload without header.
    /// `.ysc` payloads stay encrypted (the game expects them that way).
    ArchiveResource { arc: Arc<Archive>, idx: usize, keys: Arc<Keys>, len: u64, keep_encrypted: bool },
}

impl Source {
    pub fn len(&self) -> u64 {
        match self {
            Source::Mem(v) => v.len() as u64,
            Source::ArchiveBinary { len, .. } | Source::ArchiveResource { len, .. } => *len,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self) -> Result<Arc<Vec<u8>>> {
        match self {
            Source::Mem(v) => Ok(v.clone()),
            Source::ArchiveBinary { arc, idx, keys, .. } => Ok(Arc::new(arc.binary_stored(*idx, keys)?)),
            Source::ArchiveResource { arc, idx, keys, keep_encrypted, .. } => {
                if *keep_encrypted {
                    Ok(Arc::new(arc.resource_stored_raw(*idx)?))
                } else {
                    Ok(Arc::new(arc.resource_payload(*idx, keys)?.2))
                }
            }
        }
    }
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Source({} bytes)", self.len())
    }
}

#[derive(Debug, Clone)]
pub enum FileData {
    /// `stored` is deflate-compressed when `compressed`, else the plain file.
    Binary { stored: Source, compressed: bool, size: u32 },
    /// RSC7 resource; `payload` is the compressed data after the 16-byte header.
    Resource { system_flags: u32, graphics_flags: u32, payload: Source },
    /// A nested archive, either untouched bytes or an expanded, editable tree.
    Rpf(RpfData),
}

#[derive(Debug, Clone)]
pub enum RpfData {
    Raw(Blob),
    Tree(Box<Tree>),
}

impl FileData {
    /// Build file data from the bytes of a file on disk, the way OpenIV
    /// imports: RSC7 files become resources, RPF7 files nested archives,
    /// everything else a compressed binary file.
    pub fn from_bytes(name: &str, data: Vec<u8>) -> Result<FileData> {
        let lname = name.to_ascii_lowercase();
        if let Some((_, sys, gfx)) = resource::parse_header(&data) {
            if lname.ends_with(".ysc") {
                return Err(Error::Invalid(format!(
                    "{name}: importing .ysc scripts needs NG encryption, which ragekit can't do yet"
                )));
            }
            return Ok(FileData::Resource {
                system_flags: sys,
                graphics_flags: gfx,
                payload: Source::Mem(Arc::new(data[16..].to_vec())),
            });
        }
        if lname.ends_with(".rpf") && Archive::is_rpf7(&data) {
            return Ok(FileData::Rpf(RpfData::Raw(Blob::from_vec(data))));
        }
        let size = u32::try_from(data.len()).map_err(|_| Error::Invalid(format!("{name}: file over 4 GiB")))?;
        if lname.ends_with(".awc") {
            // Audio is stored uncompressed so the game can stream it.
            return Ok(FileData::Binary { stored: Source::Mem(Arc::new(data)), compressed: false, size });
        }
        let compressed = deflate(&data)?;
        if compressed.len() > 0xFF_FFFF {
            // Compressed size field is 24-bit; fall back to storing as is.
            return Ok(FileData::Binary { stored: Source::Mem(Arc::new(data)), compressed: false, size });
        }
        Ok(FileData::Binary { stored: Source::Mem(Arc::new(compressed)), compressed: true, size })
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            FileData::Binary { .. } => "binary",
            FileData::Resource { .. } => "resource",
            FileData::Rpf(_) => "archive",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Node {
    Dir(Dir),
    File(FileData),
}

#[derive(Debug, Clone, Default)]
pub struct Dir {
    /// Keyed by lowercase name; value keeps the display name.
    pub children: BTreeMap<String, (String, Node)>,
}

/// An editable archive.
#[derive(Debug, Clone)]
pub struct Tree {
    pub root: Dir,
    keys: Arc<Keys>,
}

fn deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut enc = DeflateEncoder::new(Vec::with_capacity(data.len() / 2), Compression::default());
    enc.write_all(data)?;
    Ok(enc.finish()?)
}

impl Tree {
    pub fn new(keys: Arc<Keys>) -> Tree {
        Tree { root: Dir::default(), keys }
    }

    /// Build an editable tree from an archive. Nested archives stay as raw
    /// bytes until a path inside them is edited.
    pub fn from_archive(arc: Archive, keys: Arc<Keys>) -> Result<Tree> {
        let arc = Arc::new(arc);
        let mut tree = Tree::new(keys.clone());
        tree.root = dir_from_archive(&arc, 0, &keys)?;
        Ok(tree)
    }

    /// Build a tree from a folder on disk. Sub-folders named `*.rpf` become
    /// nested archives, like OpenIV's folder import.
    pub fn from_dir(path: &Path, keys: Arc<Keys>) -> Result<Tree> {
        let mut tree = Tree::new(keys.clone());
        tree.root = dir_from_disk(path, &keys)?;
        Ok(tree)
    }

    pub fn keys(&self) -> &Arc<Keys> {
        &self.keys
    }

    /// Mutable directory at `path`, descending into (and expanding) nested
    /// archives. With `create`, missing directories are made.
    fn dir_mut(&mut self, path: &str, create: bool) -> Result<&mut Dir> {
        let keys = self.keys.clone();
        let norm = normalize_path(path);
        let mut cur = &mut self.root;
        if norm.is_empty() {
            return Ok(cur);
        }
        for part in norm.split('/') {
            if !cur.children.contains_key(part) {
                if !create {
                    return Err(Error::NotFound(norm.clone()));
                }
                let node = if part.ends_with(".rpf") {
                    Node::File(FileData::Rpf(RpfData::Tree(Box::new(Tree::new(keys.clone())))))
                } else {
                    Node::Dir(Dir::default())
                };
                cur.children.insert(part.to_string(), (part.to_string(), node));
            }
            let (_, node) = cur.children.get_mut(part).unwrap();
            cur = match node {
                Node::Dir(d) => d,
                Node::File(FileData::Rpf(rpf)) => {
                    if let RpfData::Raw(blob) = rpf {
                        let arc = Archive::parse(blob.clone(), part, &keys)?;
                        *rpf = RpfData::Tree(Box::new(Tree::from_archive(arc, keys.clone())?));
                    }
                    let RpfData::Tree(t) = rpf else { unreachable!() };
                    &mut t.root
                }
                Node::File(_) => return Err(Error::Invalid(format!("{part} is a file, not a folder"))),
            };
        }
        Ok(cur)
    }

    /// Add or replace a file at `path` (parent folders are created).
    pub fn insert(&mut self, path: &str, data: FileData) -> Result<()> {
        let norm = normalize_path(path);
        let (parent, name) = norm.rsplit_once('/').unwrap_or(("", norm.as_str()));
        if name.is_empty() {
            return Err(Error::Invalid("empty file name".into()));
        }
        let dir = self.dir_mut(parent, true)?;
        if let Some((_, Node::Dir(_))) = dir.children.get(name) {
            return Err(Error::Invalid(format!("{norm} is a folder")));
        }
        dir.children.insert(name.to_string(), (name.to_string(), Node::File(data)));
        Ok(())
    }

    /// Insert a whole node (file or folder) at `path`, replacing what's there.
    pub fn insert_node(&mut self, path: &str, node: Node) -> Result<()> {
        let norm = normalize_path(path);
        let (parent, name) = norm.rsplit_once('/').unwrap_or(("", norm.as_str()));
        if name.is_empty() {
            // Merge into root.
            if let Node::Dir(d) = node {
                for (k, v) in d.children {
                    self.root.children.insert(k, v);
                }
                return Ok(());
            }
            return Err(Error::Invalid("empty file name".into()));
        }
        let dir = self.dir_mut(parent, true)?;
        dir.children.insert(name.to_string(), (name.to_string(), node));
        Ok(())
    }

    /// Create a folder (and parents).
    pub fn mkdir(&mut self, path: &str) -> Result<()> {
        self.dir_mut(path, true).map(|_| ())
    }

    /// Remove a file or folder.
    pub fn remove(&mut self, path: &str) -> Result<()> {
        let norm = normalize_path(path);
        let (parent, name) = norm.rsplit_once('/').unwrap_or(("", norm.as_str()));
        let dir = self.dir_mut(parent, false)?;
        dir.children.remove(name).map(|_| ()).ok_or(Error::NotFound(norm))
    }

    /// Whether a path exists (without expanding nested archives).
    pub fn contains(&self, path: &str) -> bool {
        let norm = normalize_path(path);
        let mut cur = &self.root;
        let parts: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
        for (i, part) in parts.iter().enumerate() {
            match cur.children.get(*part) {
                None => return false,
                Some((_, Node::Dir(d))) => cur = d,
                Some((_, Node::File(FileData::Rpf(RpfData::Tree(t))))) => cur = &t.root,
                Some((_, Node::File(_))) => return i == parts.len() - 1,
            }
        }
        true
    }

    /// Serialize to a byte vector.
    pub fn to_bytes(&self, enc: Encryption) -> Result<Vec<u8>> {
        let mut v = Vec::new();
        self.write(&mut v, enc)?;
        Ok(v)
    }

    /// Write the archive. `enc` may be `Open` (recommended), `None` or `Aes`
    /// (needs the AES key). NG output is not supported.
    pub fn write<W: Write>(&self, w: &mut W, enc: Encryption) -> Result<u64> {
        if enc == Encryption::Ng {
            return Err(Error::Invalid("writing NG-encrypted archives is not supported; use OPEN".into()));
        }

        // 1. Flatten in the order the game and CodeWalker use: each folder's
        //    children are contiguous and sorted by name; folders are expanded
        //    depth-first via a stack.
        struct Flat<'a> {
            name: &'a str,
            dir: Option<(&'a Dir, u32, u32)>,
            file: Option<&'a FileData>,
        }
        let mut flat: Vec<Flat> = vec![Flat { name: "", dir: Some((&self.root, 0, 0)), file: None }];
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            let d = flat[i].dir.unwrap().0;
            let first = flat.len() as u32;
            let mut kids: Vec<(&String, &Node)> = d.children.iter().map(|(_, (n, node))| (n, node)).collect();
            kids.sort_by(|a, b| a.0.cmp(b.0));
            flat[i].dir = Some((d, first, kids.len() as u32));
            for (name, node) in kids {
                let idx = flat.len();
                match node {
                    Node::Dir(sub) => {
                        flat.push(Flat { name, dir: Some((sub, 0, 0)), file: None });
                        stack.push(idx);
                    }
                    Node::File(f) => flat.push(Flat { name, dir: None, file: Some(f) }),
                }
            }
        }

        // 2. Names table (deduplicated, root name at offset 0).
        let mut names: Vec<u8> = Vec::new();
        let mut name_offsets: std::collections::HashMap<&str, u32> = Default::default();
        let mut name_off = Vec::with_capacity(flat.len());
        for f in &flat {
            let off = *name_offsets.entry(f.name).or_insert_with(|| {
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

        // 3. Materialise data that needs building (expanded nested archives).
        let mut built: Vec<Option<Arc<Vec<u8>>>> = vec![None; flat.len()];
        for (i, f) in flat.iter().enumerate() {
            if let Some(FileData::Rpf(RpfData::Tree(t))) = f.file {
                built[i] = Some(Arc::new(t.to_bytes(Encryption::Open)?));
            }
        }
        let stored_len = |i: usize| -> u64 {
            match flat[i].file {
                None => 0,
                Some(FileData::Binary { stored, .. }) => stored.len(),
                Some(FileData::Resource { payload, .. }) => 16 + payload.len(),
                Some(FileData::Rpf(RpfData::Raw(b))) => b.len() as u64,
                Some(FileData::Rpf(RpfData::Tree(_))) => built[i].as_ref().unwrap().len() as u64,
            }
        };

        // 4. Layout.
        let header_len = 16 + 16 * flat.len() as u64 + names.len() as u64;
        let mut next_block = header_len.div_ceil(BLOCK);
        let mut offsets = vec![0u32; flat.len()];
        for i in 0..flat.len() {
            if flat[i].file.is_none() {
                continue;
            }
            let len = stored_len(i);
            if next_block > 0x7F_FFFF {
                return Err(Error::Invalid("archive too large (over 4 GiB)".into()));
            }
            offsets[i] = next_block as u32;
            next_block += len.div_ceil(BLOCK).max(1);
        }

        // 5. Entry table.
        let mut toc = Vec::with_capacity(flat.len() * 16);
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
                    let csize = if *compressed { stored_len(i) as u32 } else { 0 };
                    push_u24(&mut toc, csize);
                    push_u24(&mut toc, off);
                    toc.extend_from_slice(&size.to_le_bytes());
                    toc.extend_from_slice(&0u32.to_le_bytes());
                }
                FileData::Rpf(_) => {
                    let len = u32::try_from(stored_len(i))
                        .map_err(|_| Error::Invalid(format!("nested archive {} over 4 GiB", f.name)))?;
                    push_u24(&mut toc, 0);
                    push_u24(&mut toc, off);
                    toc.extend_from_slice(&len.to_le_bytes());
                    toc.extend_from_slice(&0u32.to_le_bytes());
                }
                FileData::Resource { system_flags, graphics_flags, .. } => {
                    let len = stored_len(i);
                    push_u24(&mut toc, len.min(0xFF_FFFF) as u32);
                    push_u24(&mut toc, off | 0x80_0000);
                    toc.extend_from_slice(&system_flags.to_le_bytes());
                    toc.extend_from_slice(&graphics_flags.to_le_bytes());
                }
            }
        }

        // 6. Header (+ optional AES on the table of contents).
        let mut names_out = names;
        if enc == Encryption::Aes {
            let key = self.keys.aes.as_ref().ok_or(Error::MissingKey("AES key"))?;
            aes_encrypt(&mut toc, key);
            aes_encrypt(&mut names_out, key);
        }
        let mut cw = Counting { inner: w, n: 0 };
        let mut hdr = Vec::with_capacity(16);
        hdr.extend_from_slice(&crate::archive::RPF7_MAGIC.to_le_bytes());
        hdr.extend_from_slice(&(flat.len() as u32).to_le_bytes());
        hdr.extend_from_slice(&(names_out.len() as u32).to_le_bytes());
        hdr.extend_from_slice(&enc.to_u32().to_le_bytes());
        cw.put(&hdr)?;
        cw.put(&toc)?;
        cw.put(&names_out)?;

        // 7. File data, each padded to a 512-byte block.
        let zeros = [0u8; BLOCK as usize];
        for (i, f) in flat.iter().enumerate() {
            let Some(file) = f.file else { continue };
            let target = offsets[i] as u64 * BLOCK;
            while cw.n < target {
                let n = ((target - cw.n) as usize).min(zeros.len());
                cw.put(&zeros[..n])?;
            }
            match file {
                FileData::Binary { stored, .. } => cw.put(&stored.get()?)?,
                FileData::Resource { system_flags, graphics_flags, payload } => {
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
                    cw.put(&payload.get()?)?;
                }
                FileData::Rpf(RpfData::Raw(b)) => cw.put(b.bytes())?,
                FileData::Rpf(RpfData::Tree(_)) => cw.put(built[i].as_ref().unwrap())?,
            }
            let end = cw.n.div_ceil(BLOCK).max(offsets[i] as u64 + 1) * BLOCK;
            while cw.n < end {
                let n = ((end - cw.n) as usize).min(zeros.len());
                cw.put(&zeros[..n])?;
            }
        }
        if cw.n == header_len {
            // Pad an empty archive's header to a whole block.
            let end = header_len.div_ceil(BLOCK) * BLOCK;
            cw.put(&zeros[..(end - cw.n) as usize])?;
        }
        Ok(cw.n)
    }
}

struct Counting<'a, W: Write> {
    inner: &'a mut W,
    n: u64,
}

impl<W: Write> Counting<'_, W> {
    fn put(&mut self, b: &[u8]) -> Result<()> {
        self.inner.write_all(b)?;
        self.n += b.len() as u64;
        Ok(())
    }
}

fn push_u24(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&[x as u8, (x >> 8) as u8, (x >> 16) as u8]);
}

fn dir_from_archive(arc: &Arc<Archive>, dir: usize, keys: &Arc<Keys>) -> Result<Dir> {
    let mut out = Dir::default();
    for c in arc.children(dir) {
        let e = &arc.entries[c];
        let node = match e.kind {
            EntryKind::Directory { .. } => Node::Dir(dir_from_archive(arc, c, keys)?),
            EntryKind::Binary { .. } if e.is_rpf() => Node::File(FileData::Rpf(RpfData::Raw(arc.nested_blob(c)?))),
            EntryKind::Binary { size, uncompressed_size, .. } => Node::File(FileData::Binary {
                stored: Source::ArchiveBinary {
                    arc: arc.clone(),
                    idx: c,
                    keys: keys.clone(),
                    len: if size == 0 { uncompressed_size } else { size } as u64,
                },
                compressed: size != 0,
                size: uncompressed_size,
            }),
            EntryKind::Resource { size, system_flags, graphics_flags, encrypted, .. } => {
                // Keep .ysc encrypted unless it came from an AES archive (it
                // would be misread as NG in an OPEN archive).
                if encrypted && arc.encryption == Encryption::Aes {
                    return Err(Error::Invalid(format!(
                        "{}: re-packing scripts from AES archives isn't supported yet",
                        arc.path_of(c)
                    )));
                }
                Node::File(FileData::Resource {
                    system_flags,
                    graphics_flags,
                    payload: Source::ArchiveResource {
                        arc: arc.clone(),
                        idx: c,
                        keys: keys.clone(),
                        len: (size as u64).saturating_sub(16),
                        keep_encrypted: encrypted,
                    },
                })
            }
        };
        out.children.insert(e.name.to_ascii_lowercase(), (e.name.clone(), node));
    }
    Ok(out)
}

fn dir_from_disk(path: &Path, keys: &Arc<Keys>) -> Result<Dir> {
    let mut out = Dir::default();
    for ent in fs::read_dir(path)? {
        let ent = ent?;
        let name = ent.file_name().to_string_lossy().to_ascii_lowercase();
        let ft = ent.file_type()?;
        let p = ent.path();
        let node = if ft.is_dir() {
            if name.ends_with(".rpf") {
                Node::File(FileData::Rpf(RpfData::Tree(Box::new(Tree { root: dir_from_disk(&p, keys)?, keys: keys.clone() }))))
            } else {
                Node::Dir(dir_from_disk(&p, keys)?)
            }
        } else if ft.is_file() || (ft.is_symlink() && p.is_file()) {
            let mut data = Vec::new();
            fs::File::open(&p)?.read_to_end(&mut data)?;
            Node::File(FileData::from_bytes(&name, data)?)
        } else {
            continue;
        };
        out.children.insert(name.clone(), (name, node));
    }
    Ok(out)
}

/// Read a file or folder from disk as a node for insertion.
pub fn node_from_disk(path: &Path, keys: &Arc<Keys>) -> Result<Node> {
    let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if path.is_dir() {
        if name.ends_with(".rpf") {
            Ok(Node::File(FileData::Rpf(RpfData::Tree(Box::new(Tree { root: dir_from_disk(path, keys)?, keys: keys.clone() })))))
        } else {
            Ok(Node::Dir(dir_from_disk(path, keys)?))
        }
    } else {
        Ok(Node::File(FileData::from_bytes(&name, fs::read(path)?)?))
    }
}
