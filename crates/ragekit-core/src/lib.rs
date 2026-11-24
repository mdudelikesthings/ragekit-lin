//! ragekit-core: GTA V and GTA IV archive formats.
//!
//! * [`vfs::open`] opens any supported archive (GTA V RPF7, GTA IV RPF2,
//!   RPF3 and IMG3) as an editable [`Tree`]: list and read paths (through
//!   nested archives), add/replace/remove/rename, then write it back.
//! * [`Archive`] is the low-level GTA V RPF7 reader.
//! * [`Keys`] holds decryption keys taken from the user's own games.

pub mod archive;
pub mod blob;
pub mod convert;
pub mod crypto;
pub mod dlc;
pub mod fsutil;
pub mod games;
pub mod gta4;
pub mod gxt2;
mod key_hashes;
pub mod keys;
pub mod meta;
pub mod oiv;
pub mod resource;
pub mod textures;
mod rpf7_write;
pub mod tree;
pub mod vfs;

pub use archive::{Archive, Encryption, Entry, EntryKind};
pub use blob::Blob;
pub use keys::Keys;
pub use tree::{FileData, Format, Node, Row, RowKind, Tree};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not an RPF7 archive (bad magic)")]
    NotRpf7,
    #[error("malformed archive: {0}")]
    Malformed(String),
    #[error("missing key: {0}. Run `ragekit keys extract` first")]
    MissingKey(&'static str),
    #[error("key error: {0}")]
    Keys(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Normalise an in-archive path: forward slashes, no leading/trailing slash,
/// lowercase.
pub fn normalize_path(p: &str) -> String {
    p.replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect::<Vec<_>>()
        .join("/")
        .to_ascii_lowercase()
}
