//! Cheap, clonable views into shared byte storage (a memory-mapped file or
//! an owned buffer), so nested archives can be read without copying.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use memmap2::Mmap;

enum Store {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl Store {
    fn bytes(&self) -> &[u8] {
        match self {
            Store::Mapped(m) => m,
            Store::Owned(v) => v,
        }
    }
}

#[derive(Clone)]
pub struct Blob {
    store: Arc<Store>,
    start: usize,
    len: usize,
}

impl Blob {
    /// Memory-map a file read-only.
    pub fn map_file(path: &Path) -> std::io::Result<Blob> {
        let f = File::open(path)?;
        let len = f.metadata()?.len() as usize;
        if len == 0 {
            return Ok(Blob::from_vec(Vec::new()));
        }
        // SAFETY: read-only map. If another process truncates the file while
        // mapped we could fault; that is the usual mmap caveat.
        let m = unsafe { Mmap::map(&f)? };
        Ok(Blob { store: Arc::new(Store::Mapped(m)), start: 0, len })
    }

    pub fn from_vec(v: Vec<u8>) -> Blob {
        let len = v.len();
        Blob { store: Arc::new(Store::Owned(v)), start: 0, len }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.store.bytes()[self.start..self.start + self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Sub-range; `None` if out of bounds.
    pub fn slice(&self, offset: usize, len: usize) -> Option<Blob> {
        let end = offset.checked_add(len)?;
        if end > self.len {
            return None;
        }
        Some(Blob { store: self.store.clone(), start: self.start + offset, len })
    }
}

impl std::fmt::Debug for Blob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Blob({} bytes)", self.len)
    }
}
