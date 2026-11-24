//! Format-independent editable archive model.
//!
//! A [`Tree`] is what the browser and the CLI work with, whatever the game:
//! load one from any supported archive (see [`crate::vfs::open`]) or from a
//! folder, list and read paths (descending into nested archives), edit it,
//! then write it back in its own format.
//!
//! Output formats:
//! * GTA V: RPF7 in OpenIV's "OPEN" layout (unencrypted table of contents),
//!   which mods-folder loaders (OpenIV.asi, OpenRPF, RageOpenV) accept.
//! * GTA IV: unencrypted RPF2 and IMG3, which the game reads as is.
//! * RPF3 (GTA IV audio) is read-only.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use flate2::write::DeflateEncoder;
use flate2::Compression;

use crate::archive::{Archive, Encryption, EntryKind};
use crate::blob::Blob;
use crate::crypto::ng_decrypt_named;
use crate::keys::Keys;
use crate::resource;
use crate::{normalize_path, Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// GTA V archive.
    Rpf7,
    /// GTA IV / EFLC archive.
    Rpf2,
    /// GTA IV audio archive (hashed names). Read-only.
    Rpf3,
    /// GTA IV / EFLC image archive (flat, no folders).
    Img3,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Rpf7 => "RPF7",
            Format::Rpf2 => "RPF2",
            Format::Rpf3 => "RPF3",
            Format::Img3 => "IMG3",
        }
    }
    pub fn game(self) -> &'static str {
        match self {
            Format::Rpf7 => "GTA V",
            _ => "GTA IV",
        }
    }
    pub fn writable(self) -> bool {
        self != Format::Rpf3
    }
    pub fn has_folders(self) -> bool {
        self != Format::Img3
    }
}

/// Where a file's bytes come from.
#[derive(Clone)]
pub enum Source {
    Mem(Arc<Vec<u8>>),
    /// Zero-copy slice of an archive.
    Blob(Blob),
    /// Produced on demand (e.g. decrypted from an archive).
    Lazy { len: u64, f: Arc<dyn Fn() -> Result<Vec<u8>> + Send + Sync> },
}

impl Source {
    pub fn mem(v: Vec<u8>) -> Source {
        Source::Mem(Arc::new(v))
    }
    pub fn len(&self) -> u64 {
        match self {
            Source::Mem(v) => v.len() as u64,
            Source::Blob(b) => b.len() as u64,
            Source::Lazy { len, .. } => *len,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Run `f` on the bytes, avoiding a copy where possible.
    pub fn with<R>(&self, f: impl FnOnce(&[u8]) -> Result<R>) -> Result<R> {
        match self {
            Source::Mem(v) => f(v),
            Source::Blob(b) => f(b.bytes()),
            Source::Lazy { f: g, .. } => f(&g()?),
        }
    }
    pub fn to_vec(&self) -> Result<Vec<u8>> {
        self.with(|b| Ok(b.to_vec()))
    }
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Source({} bytes)", self.len())
    }
}

#[derive(Debug, Clone)]
pub enum FileData {
    /// Plain file. `stored` is raw-deflate compressed when `compressed`.
    Binary { stored: Source, compressed: bool, size: u32 },
    /// GTA V RSC7 resource: `payload` is the data after the 16-byte header.
    /// `ng_encrypted` marks .ysc scripts, kept encrypted as the game expects.
    Resource { system_flags: u32, graphics_flags: u32, payload: Source, ng_encrypted: bool },
    /// GTA IV RSC5 resource: `data` is the whole file including its header.
    Resource5 { rtype: u32, flags: u32, data: Source },
    /// Nested GTA V archive: untouched bytes or an expanded, editable tree.
    Rpf(RpfData),
}

#[derive(Debug, Clone)]
pub enum RpfData {
    Raw(Blob),
    Tree(Box<Tree>),
}

pub const RSC5_MAGIC: u32 = 0x0543_5352; // "RSC\x05"

fn deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut enc = DeflateEncoder::new(Vec::with_capacity(data.len() / 2), Compression::default());
    enc.write_all(data)?;
    Ok(enc.finish()?)
}

fn inflate(data: &[u8], hint: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(hint);
    flate2::read::DeflateDecoder::new(data)
        .read_to_end(&mut out)
        .map_err(|e| Error::Malformed(format!("decompression failed: {e}")))?;
    Ok(out)
}

fn parse_rsc5(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 12 || u32::from_le_bytes(data[0..4].try_into().unwrap()) != RSC5_MAGIC {
        return None;
    }
    let rtype = u32::from_le_bytes(data[4..8].try_into().unwrap());
    let flags = u32::from_le_bytes(data[8..12].try_into().unwrap());
    Some((rtype, flags))
}

impl FileData {
    /// Build file data from a file's bytes, converting the way OpenIV
    /// imports for the given archive format.
    pub fn from_bytes(name: &str, data: Vec<u8>, format: Format) -> Result<FileData> {
        let lname = name.to_ascii_lowercase();
        let size = u32::try_from(data.len()).map_err(|_| Error::Invalid(format!("{name}: file over 4 GiB")))?;
        match format {
            Format::Rpf3 => Err(Error::Invalid("GTA IV audio archives (RPF3) are read-only".into())),
            Format::Rpf7 => {
                if parse_rsc5(&data).is_some() {
                    return Err(Error::Invalid(format!("{name} is a GTA IV resource and can't go in a GTA V archive")));
                }
                if let Some((_, sys, gfx)) = resource::parse_header(&data) {
                    if lname.ends_with(".ysc") {
                        return Err(Error::Invalid(format!(
                            "{name}: importing .ysc scripts needs NG encryption, which ragekit can't do yet"
                        )));
                    }
                    return Ok(FileData::Resource {
                        system_flags: sys,
                        graphics_flags: gfx,
                        payload: Source::mem(data[16..].to_vec()),
                        ng_encrypted: false,
                    });
                }
                if lname.ends_with(".rpf") && Archive::is_rpf7(&data) {
                    return Ok(FileData::Rpf(RpfData::Raw(Blob::from_vec(data))));
                }
                if lname.ends_with(".awc") || lname.ends_with(".rpf") {
                    return Ok(FileData::Binary { stored: Source::mem(data), compressed: false, size });
                }
                Self::compressed_binary(data, size, 0xFF_FFFF)
            }
            Format::Rpf2 | Format::Img3 => {
                if resource::parse_header(&data).is_some() {
                    return Err(Error::Invalid(format!("{name} is a GTA V resource and can't go in a GTA IV archive")));
                }
                if let Some((rtype, flags)) = parse_rsc5(&data) {
                    if format == Format::Img3 || flags & 0xC000_0000 == 0xC000_0000 {
                        return Ok(FileData::Resource5 { rtype, flags, data: Source::mem(data) });
                    }
                }
                if format == Format::Img3 || lname.ends_with(".ivaud") {
                    return Ok(FileData::Binary { stored: Source::mem(data), compressed: false, size });
                }
                Self::compressed_binary(data, size, 0x3FFF_FFFF)
            }
        }
    }

    fn compressed_binary(data: Vec<u8>, size: u32, max: usize) -> Result<FileData> {
        let c = deflate(&data)?;
        if c.len() > max || c.len() >= data.len() {
            return Ok(FileData::Binary { stored: Source::mem(data), compressed: false, size });
        }
        Ok(FileData::Binary { stored: Source::mem(c), compressed: true, size })
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            FileData::Binary { .. } => "file",
            FileData::Resource { .. } | FileData::Resource5 { .. } => "resource",
            FileData::Rpf(_) => "archive",
        }
    }

    /// Size of the file once exported (0 if not known without building).
    pub fn size(&self) -> u64 {
        match self {
            FileData::Binary { size, .. } => *size as u64,
            FileData::Resource { payload, .. } => 16 + payload.len(),
            FileData::Resource5 { data, .. } => data.len(),
            FileData::Rpf(RpfData::Raw(b)) => b.len() as u64,
            FileData::Rpf(RpfData::Tree(_)) => 0,
        }
    }

    /// One-line technical detail for display.
    pub fn detail(&self) -> String {
        match self {
            FileData::Binary { stored, compressed, size } => {
                if *compressed {
                    format!("compressed {} -> {}", stored.len(), size)
                } else {
                    "stored".into()
                }
            }
            FileData::Resource { system_flags, graphics_flags, ng_encrypted, .. } => format!(
                "RSC7 v{} · system {} · graphics {}{}",
                resource::version_from_flags(*system_flags, *graphics_flags),
                resource::size_from_flags(*system_flags),
                resource::size_from_flags(*graphics_flags),
                if *ng_encrypted { " · encrypted" } else { "" }
            ),
            FileData::Resource5 { rtype, flags, .. } => format!("RSC5 type {rtype:#x} · flags {flags:#010x}"),
            FileData::Rpf(RpfData::Raw(_)) => "nested archive".into(),
            FileData::Rpf(RpfData::Tree(_)) => "nested archive (modified)".into(),
        }
    }

    /// The file as it is saved to disk: decompressed / decrypted binary,
    /// resources with their header (the form OpenIV, CodeWalker and SparkIV
    /// export and import).
    pub fn export(&self, name: &str, keys: &Keys) -> Result<Vec<u8>> {
        match self {
            FileData::Binary { stored, compressed, size } => {
                if *compressed {
                    stored.with(|b| inflate(b, *size as usize))
                } else {
                    stored.to_vec()
                }
            }
            FileData::Resource { system_flags, graphics_flags, payload, ng_encrypted } => {
                let mut out = resource::header(*system_flags, *graphics_flags).to_vec();
                let start = out.len();
                payload.with(|b| {
                    out.extend_from_slice(b);
                    Ok(())
                })?;
                if *ng_encrypted {
                    let len = out.len() as u32;
                    ng_decrypt_named(keys, &mut out[start..], name, len)?;
                }
                Ok(out)
            }
            FileData::Resource5 { data, .. } => data.to_vec(),
            FileData::Rpf(RpfData::Raw(b)) => Ok(b.bytes().to_vec()),
            FileData::Rpf(RpfData::Tree(t)) => t.to_bytes(),
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
    /// Keyed by lowercase name; the value keeps the display name.
    pub children: BTreeMap<String, (String, Node)>,
    /// RPF2 directory flags (kept as read).
    pub flags: u32,
}

/// What a path points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Folder,
    /// A nested archive you can enter.
    Archive,
    File,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub name: String,
    pub kind: RowKind,
    pub size: u64,
    /// "file", "resource", "archive" or "folder".
    pub type_label: &'static str,
    pub detail: String,
}

/// An editable archive.
#[derive(Debug, Clone)]
pub struct Tree {
    pub root: Dir,
    pub format: Format,
    keys: Arc<Keys>,
    /// How the source was stored, for display (e.g. "RPF7 · NG").
    pub origin: String,
    /// Opaque header word kept for RPF2 / IMG3.
    pub header_extra: u32,
    /// Root directory name (RPF2 keeps one).
    pub root_name: String,
}

enum NodeRef<'a> {
    Dir(&'a Dir),
    File(&'a FileData),
}

impl Tree {
    pub fn new(format: Format, keys: Arc<Keys>) -> Tree {
        Tree {
            root: Dir::default(),
            format,
            keys,
            origin: format!("{} · new", format.label()),
            header_extra: 0,
            root_name: String::new(),
        }
    }

    pub fn keys(&self) -> &Arc<Keys> {
        &self.keys
    }

    /// Build an editable tree from a GTA V archive. Nested archives stay as
    /// raw bytes until something inside them is edited.
    pub fn from_archive(arc: Archive, keys: Arc<Keys>) -> Result<Tree> {
        let origin = format!("RPF7 · {}", arc.encryption.label());
        let arc = Arc::new(arc);
        let mut tree = Tree::new(Format::Rpf7, keys.clone());
        tree.root = dir_from_archive(&arc, 0, &keys)?;
        tree.origin = origin;
        Ok(tree)
    }

    /// Build a tree from a folder on disk. For GTA V, sub-folders named
    /// `*.rpf` become nested archives (like OpenIV's folder import).
    pub fn from_dir(path: &Path, format: Format, keys: Arc<Keys>) -> Result<Tree> {
        let mut tree = Tree::new(format, keys.clone());
        tree.root = dir_from_disk(path, format, &keys)?;
        if format == Format::Img3 && tree.root.children.values().any(|(_, n)| matches!(n, Node::Dir(_))) {
            return Err(Error::Invalid("IMG archives can't contain folders".into()));
        }
        Ok(tree)
    }

    fn parts(path: &str) -> Vec<String> {
        let n = normalize_path(path);
        if n.is_empty() {
            Vec::new()
        } else {
            n.split('/').map(str::to_string).collect()
        }
    }

    /// Walk to `parts`, entering nested archives (parsing untouched ones
    /// on the fly, without modifying the tree).
    fn with_node<R>(&self, parts: &[String], f: &mut dyn FnMut(NodeRef, &Tree) -> Result<R>) -> Result<R> {
        let mut cur = NodeRef::Dir(&self.root);
        for (i, part) in parts.iter().enumerate() {
            let dir = match cur {
                NodeRef::Dir(d) => d,
                NodeRef::File(FileData::Rpf(RpfData::Tree(t))) => {
                    return t.with_node(&parts[i..], f);
                }
                NodeRef::File(FileData::Rpf(RpfData::Raw(b))) => {
                    let t = self.expand_raw(b, &parts[i - 1])?;
                    return t.with_node(&parts[i..], f);
                }
                NodeRef::File(_) => return Err(Error::NotFound(parts[..=i].join("/"))),
            };
            let (_, node) = dir.children.get(part).ok_or_else(|| Error::NotFound(parts[..=i].join("/")))?;
            cur = match node {
                Node::Dir(d) => NodeRef::Dir(d),
                Node::File(fd) => NodeRef::File(fd),
            };
        }
        f(cur, self)
    }

    fn expand_raw(&self, b: &Blob, name: &str) -> Result<Tree> {
        Tree::from_archive(Archive::parse(b.clone(), name, &self.keys)?, self.keys.clone())
    }

    /// What `path` is.
    pub fn kind(&self, path: &str) -> Result<RowKind> {
        self.with_node(&Self::parts(path), &mut |n, _| {
            Ok(match n {
                NodeRef::Dir(_) => RowKind::Folder,
                NodeRef::File(FileData::Rpf(_)) => RowKind::Archive,
                NodeRef::File(_) => RowKind::File,
            })
        })
    }

    /// List a folder or nested archive, sorted folders first then by name.
    pub fn list(&self, path: &str) -> Result<Vec<Row>> {
        self.with_node(&Self::parts(path), &mut |n, owner| {
            let dir_rows = |d: &Dir| -> Vec<Row> {
                let mut rows: Vec<Row> = d
                    .children
                    .values()
                    .map(|(name, node)| match node {
                        Node::Dir(sub) => Row {
                            name: name.clone(),
                            kind: RowKind::Folder,
                            size: 0,
                            type_label: "folder",
                            detail: format!("{} items", sub.children.len()),
                        },
                        Node::File(fd) => Row {
                            name: name.clone(),
                            kind: if matches!(fd, FileData::Rpf(_)) { RowKind::Archive } else { RowKind::File },
                            size: fd.size(),
                            type_label: fd.kind_label(),
                            detail: fd.detail(),
                        },
                    })
                    .collect();
                rows.sort_by(|a, b| {
                    (a.kind == RowKind::File).cmp(&(b.kind == RowKind::File)).then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
                });
                rows
            };
            match n {
                NodeRef::Dir(d) => Ok(dir_rows(d)),
                NodeRef::File(FileData::Rpf(RpfData::Tree(t))) => Ok(dir_rows(&t.root)),
                NodeRef::File(FileData::Rpf(RpfData::Raw(b))) => {
                    let name = Self::parts(path).last().cloned().unwrap_or_default();
                    Ok(dir_rows(&owner.expand_raw(b, &name)?.root))
                }
                NodeRef::File(_) => Err(Error::Invalid(format!("{path} is a file"))),
            }
        })
    }

    /// Exported bytes of a file.
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let parts = Self::parts(path);
        let name = parts.last().cloned().unwrap_or_default();
        self.with_node(&parts, &mut |n, owner| match n {
            NodeRef::File(fd) => fd.export(&name, &owner.keys),
            NodeRef::Dir(_) => Err(Error::Invalid(format!("{path} is a folder"))),
        })
    }

    /// Detail line for one path.
    pub fn detail(&self, path: &str) -> Result<String> {
        self.with_node(&Self::parts(path), &mut |n, _| {
            Ok(match n {
                NodeRef::File(fd) => fd.detail(),
                NodeRef::Dir(d) => format!("{} items", d.children.len()),
            })
        })
    }

    /// Visit every file (not descending into nested archives); paths are
    /// relative to the root.
    pub fn for_each_file(&self, f: &mut dyn FnMut(&str, &FileData)) {
        fn rec(d: &Dir, prefix: &str, f: &mut dyn FnMut(&str, &FileData)) {
            for (name, node) in d.children.values() {
                let p = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
                match node {
                    Node::Dir(sub) => rec(sub, &p, f),
                    Node::File(fd) => f(&p, fd),
                }
            }
        }
        rec(&self.root, "", f);
    }

    /// Counts of (folders, files) not descending into nested archives.
    pub fn counts(&self) -> (usize, usize) {
        fn rec(d: &Dir) -> (usize, usize) {
            let mut c = (0, 0);
            for (_, n) in d.children.values() {
                match n {
                    Node::Dir(s) => {
                        let (a, b) = rec(s);
                        c.0 += 1 + a;
                        c.1 += b;
                    }
                    Node::File(_) => c.1 += 1,
                }
            }
            c
        }
        rec(&self.root)
    }

    // ---- editing ------------------------------------------------------

    fn check_writable(&self) -> Result<()> {
        if !self.format.writable() {
            return Err(Error::Invalid(format!("{} archives are read-only", self.format.label())));
        }
        Ok(())
    }

    /// Mutable folder at `path`, expanding nested archives on the way.
    fn dir_mut(&mut self, path: &str, create: bool) -> Result<(&mut Dir, Format)> {
        self.check_writable()?;
        let keys = self.keys.clone();
        let norm = normalize_path(path);
        let mut format = self.format;
        let mut cur = &mut self.root;
        if norm.is_empty() {
            return Ok((cur, format));
        }
        if !format.has_folders() {
            return Err(Error::Invalid("IMG archives have no folders".into()));
        }
        for part in norm.split('/') {
            if !cur.children.contains_key(part) {
                if !create {
                    return Err(Error::NotFound(norm.clone()));
                }
                let node = if format == Format::Rpf7 && part.ends_with(".rpf") {
                    Node::File(FileData::Rpf(RpfData::Tree(Box::new(Tree::new(Format::Rpf7, keys.clone())))))
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
                    format = t.format;
                    &mut t.root
                }
                Node::File(_) => return Err(Error::Invalid(format!("{part} is a file, not a folder"))),
            };
        }
        Ok((cur, format))
    }

    /// Add or replace a file from raw bytes (converted for the archive's format).
    pub fn insert_bytes(&mut self, path: &str, data: Vec<u8>) -> Result<()> {
        let norm = normalize_path(path);
        let (parent, name) = norm.rsplit_once('/').unwrap_or(("", norm.as_str()));
        let (_, format) = self.dir_mut(parent, true)?;
        let fd = FileData::from_bytes(name, data, format)?;
        self.insert(&norm, fd)
    }

    /// Add or replace a file at `path` (parent folders are created).
    pub fn insert(&mut self, path: &str, data: FileData) -> Result<()> {
        self.insert_node(path, Node::File(data))
    }

    /// Insert a file or folder at `path`, replacing what's there.
    pub fn insert_node(&mut self, path: &str, node: Node) -> Result<()> {
        let norm = normalize_path(path);
        let (parent, name) = norm.rsplit_once('/').unwrap_or(("", norm.as_str()));
        if name.is_empty() {
            return Err(Error::Invalid("empty name".into()));
        }
        let (dir, format) = self.dir_mut(parent, true)?;
        if let Node::Dir(_) = node {
            if !format.has_folders() {
                return Err(Error::Invalid("IMG archives have no folders".into()));
            }
        }
        if let (Node::File(_), Some((_, Node::Dir(_)))) = (&node, dir.children.get(name)) {
            return Err(Error::Invalid(format!("{norm} is a folder")));
        }
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
        let (dir, _) = self.dir_mut(parent, false)?;
        dir.children.remove(name).map(|_| ()).ok_or(Error::NotFound(norm))
    }

    /// Rename a file or folder within its folder.
    pub fn rename(&mut self, path: &str, new_name: &str) -> Result<()> {
        let norm = normalize_path(path);
        let new_l = new_name.to_ascii_lowercase();
        if new_l.is_empty() || new_l.contains('/') || new_l.contains('\\') {
            return Err(Error::Invalid(format!("bad name {new_name:?}")));
        }
        let (parent, name) = norm.rsplit_once('/').unwrap_or(("", norm.as_str()));
        let (dir, _) = self.dir_mut(parent, false)?;
        if dir.children.contains_key(&new_l) {
            return Err(Error::Invalid(format!("{new_name} already exists")));
        }
        let (_, node) = dir.children.remove(name).ok_or_else(|| Error::NotFound(norm.clone()))?;
        dir.children.insert(new_l.clone(), (new_l, node));
        Ok(())
    }

    /// Whether a path exists.
    pub fn contains(&self, path: &str) -> bool {
        self.kind(path).is_ok()
    }

    // ---- writing ------------------------------------------------------

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut v = Vec::new();
        self.write(&mut v)?;
        Ok(v)
    }

    /// Write the archive in its own format (GTA V: OPEN RPF7).
    pub fn write<W: Write>(&self, w: &mut W) -> Result<u64> {
        match self.format {
            Format::Rpf7 => self.write_rpf7(w, Encryption::Open),
            Format::Rpf2 => crate::gta4::write_rpf2(self, w),
            Format::Img3 => crate::gta4::write_img(self, w),
            Format::Rpf3 => Err(Error::Invalid("RPF3 archives are read-only".into())),
        }
    }

    /// Write a GTA V archive. `enc` may be `Open` (recommended), `None` or
    /// `Aes` (needs the AES key). NG output is not supported.
    pub fn write_rpf7<W: Write>(&self, w: &mut W, enc: Encryption) -> Result<u64> {
        crate::rpf7_write::write(self, w, enc)
    }

    /// Children of every folder made contiguous, folders expanded
    /// depth-first via a stack, children sorted by name (the order the
    /// RAGE engine and CodeWalker use).
    pub(crate) fn flatten(&self) -> Vec<Flat<'_>> {
        let mut flat: Vec<Flat> = vec![Flat { name: &self.root_name, dir: Some((&self.root, 0, 0)), file: None }];
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            let d = flat[i].dir.unwrap().0;
            let first = flat.len() as u32;
            let mut kids: Vec<(&String, &Node)> = d.children.values().map(|(n, node)| (n, node)).collect();
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
        flat
    }
}

pub(crate) struct Flat<'a> {
    pub name: &'a str,
    /// (dir, first child index, child count)
    pub dir: Option<(&'a Dir, u32, u32)>,
    pub file: Option<&'a FileData>,
}

/// Counts bytes written.
pub(crate) struct Counting<'a, W: Write> {
    pub inner: &'a mut W,
    pub n: u64,
}

impl<W: Write> Counting<'_, W> {
    pub fn put(&mut self, b: &[u8]) -> Result<()> {
        self.inner.write_all(b)?;
        self.n += b.len() as u64;
        Ok(())
    }
    pub fn pad_to(&mut self, target: u64) -> Result<()> {
        const Z: [u8; 4096] = [0; 4096];
        while self.n < target {
            let k = ((target - self.n) as usize).min(Z.len());
            self.put(&Z[..k])?;
        }
        Ok(())
    }
    pub fn source(&mut self, s: &Source) -> Result<()> {
        s.with(|b| self.put(b))
    }
}

fn dir_from_archive(arc: &Arc<Archive>, dir: usize, keys: &Arc<Keys>) -> Result<Dir> {
    let mut out = Dir::default();
    for c in arc.children(dir) {
        let e = &arc.entries[c];
        let node = match e.kind {
            EntryKind::Directory { .. } => Node::Dir(dir_from_archive(arc, c, keys)?),
            EntryKind::Binary { .. } if e.is_rpf() => Node::File(FileData::Rpf(RpfData::Raw(arc.nested_blob(c)?))),
            EntryKind::Binary { size, uncompressed_size, encrypted, offset } => {
                let len = if size == 0 { uncompressed_size } else { size } as u64;
                let stored = if encrypted {
                    let (arc2, keys2) = (arc.clone(), keys.clone());
                    Source::Lazy { len, f: Arc::new(move || arc2.binary_stored(c, &keys2)) }
                } else {
                    let start = offset as usize * crate::archive::BLOCK as usize;
                    Source::Blob(arc.data.slice(start, len as usize).ok_or_else(|| {
                        Error::Malformed(format!("{}: data runs past end of archive", arc.path_of(c)))
                    })?)
                };
                Node::File(FileData::Binary { stored, compressed: size != 0, size: uncompressed_size })
            }
            EntryKind::Resource { size, system_flags, graphics_flags, encrypted, offset } => {
                if encrypted && arc.encryption == Encryption::Aes {
                    // Would be misread as NG in an OPEN archive; decrypt it instead.
                    let arc2 = arc.clone();
                    let keys2 = keys.clone();
                    let payload =
                        Source::Lazy { len: (size as u64).saturating_sub(16), f: Arc::new(move || Ok(arc2.resource_payload(c, &keys2)?.2)) };
                    Node::File(FileData::Resource { system_flags, graphics_flags, payload, ng_encrypted: false })
                } else {
                    let start = offset as usize * crate::archive::BLOCK as usize + 16;
                    let len = (size as usize).saturating_sub(16);
                    let payload = Source::Blob(arc.data.slice(start, len).ok_or_else(|| {
                        Error::Malformed(format!("{}: data runs past end of archive", arc.path_of(c)))
                    })?);
                    Node::File(FileData::Resource { system_flags, graphics_flags, payload, ng_encrypted: encrypted })
                }
            }
        };
        out.children.insert(e.name.to_ascii_lowercase(), (e.name.clone(), node));
    }
    Ok(out)
}

fn dir_from_disk(path: &Path, format: Format, keys: &Arc<Keys>) -> Result<Dir> {
    let mut out = Dir::default();
    let mut ents: Vec<_> = fs::read_dir(path)?.collect::<std::io::Result<_>>()?;
    ents.sort_by_key(|e| e.file_name());
    for ent in ents {
        let name = ent.file_name().to_string_lossy().to_ascii_lowercase();
        let p = ent.path();
        let node = node_from_disk_named(&p, &name, format, keys)?;
        out.children.insert(name.clone(), (name, node));
    }
    Ok(out)
}

fn node_from_disk_named(p: &Path, name: &str, format: Format, keys: &Arc<Keys>) -> Result<Node> {
    if p.is_dir() {
        if format == Format::Rpf7 && name.ends_with(".rpf") {
            let mut t = Tree::new(Format::Rpf7, keys.clone());
            t.root = dir_from_disk(p, Format::Rpf7, keys)?;
            Ok(Node::File(FileData::Rpf(RpfData::Tree(Box::new(t)))))
        } else {
            Ok(Node::Dir(dir_from_disk(p, format, keys)?))
        }
    } else {
        let data = fs::read(p).map_err(|e| Error::Invalid(format!("{}: {e}", p.display())))?;
        Ok(Node::File(FileData::from_bytes(name, data, format)?))
    }
}

/// Read a file or folder from disk as a node for insertion into an archive
/// of `format`.
pub fn node_from_disk(path: &Path, format: Format, keys: &Arc<Keys>) -> Result<Node> {
    let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    node_from_disk_named(path, &name, format, keys)
}

impl Tree {
    /// Format of the archive that holds `path`'s parent (nested archives
    /// are always GTA V).
    pub fn format_at(&self, path: &str) -> Format {
        let parts = Self::parts(path);
        if parts.len() > 1 {
            for i in 1..parts.len() {
                if let Ok(RowKind::Archive) = self.kind(&parts[..i].join("/")) {
                    return Format::Rpf7;
                }
            }
        }
        self.format
    }
}
