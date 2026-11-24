//! Readable text forms of binary game files: binary metadata ⇄ XML,
//! `.gxt2` text tables ⇄ text. Used for previews, "export as text",
//! "replace from text", and extracting with conversion.

use crate::{gxt2, meta, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conv {
    Meta(meta::Kind),
    Gxt2,
}

impl Conv {
    /// File extension of the text form.
    pub fn ext(self) -> &'static str {
        match self {
            Conv::Meta(_) => "xml",
            Conv::Gxt2 => "txt",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Conv::Meta(k) => k.label(),
            Conv::Gxt2 => "GXT2 text table",
        }
    }
}

/// Whether `data` has a text form.
pub fn kind(data: &[u8]) -> Option<Conv> {
    if gxt2::is_gxt2(data) {
        return Some(Conv::Gxt2);
    }
    meta::detect(data).map(Conv::Meta)
}

/// The text form of a file.
pub fn to_text(data: &[u8]) -> Result<(String, Conv)> {
    match kind(data) {
        Some(Conv::Gxt2) => Ok((gxt2::to_text(&gxt2::parse(data)?), Conv::Gxt2)),
        Some(Conv::Meta(k)) => Ok((meta::to_xml(data, &[])?.0, Conv::Meta(k))),
        None => Err(crate::Error::Invalid("this file has no text form".into())),
    }
}

/// Convert edited text back to the binary form of `original`.
pub fn from_text(text: &str, original: &[u8]) -> Result<Vec<u8>> {
    match kind(original) {
        Some(Conv::Gxt2) => Ok(gxt2::write(&gxt2::from_text(text)?)),
        Some(Conv::Meta(_)) => meta::from_xml(text, original),
        None => Err(crate::Error::Invalid("the original has no text form".into())),
    }
}
