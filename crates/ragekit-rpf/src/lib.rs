//! ragekit-rpf: read, extract and build GTA V RPF7 archives on any platform.
//!
//! * [`Archive`] parses an archive (from a file or from bytes, including
//!   archives nested inside other archives) and extracts entries.
//! * [`Tree`] is an editable model of an archive. Load one from an
//!   [`Archive`] or a folder, add/replace/remove files, then write it out
//!   as an OpenIV-style "OPEN" archive that GTA V mod loaders accept.
//! * [`Keys`] holds the decryption keys taken from the user's own game.

pub mod archive;
pub mod blob;
pub mod crypto;
mod key_hashes;
pub mod keys;
pub mod resource;
pub mod tree;

pub use archive::{Archive, Encryption, Entry, EntryKind};
pub use blob::Blob;
pub use keys::Keys;
pub use tree::{FileData, Node, Tree};

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
