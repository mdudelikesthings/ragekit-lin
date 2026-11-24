//! GTA V text tables (`.gxt2`) ⇄ editable text.
//!
//! Layout: "GXT2", entry count, (label hash, offset) pairs, "GXT2", end
//! offset, then null-terminated UTF-8 strings. Labels are stored as Jenkins
//! hashes; the text form writes them as `0xHASH = text` (a label name can be
//! used instead and is hashed).

use crate::{Error, Result};

const MAGIC: u32 = 0x4758_5432; // "2TXG" read little-endian

pub fn is_gxt2(data: &[u8]) -> bool {
    data.len() >= 8 && u32::from_le_bytes(data[0..4].try_into().unwrap()) == MAGIC
}

pub fn parse(data: &[u8]) -> Result<Vec<(u32, String)>> {
    if !is_gxt2(data) {
        return Err(Error::Invalid("not a GXT2 file".into()));
    }
    let rd = |o: usize| -> Result<u32> {
        data.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| Error::Malformed("GXT2: truncated".into()))
    };
    let count = rd(4)? as usize;
    if count > data.len() / 8 {
        return Err(Error::Malformed("GXT2: bad entry count".into()));
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let hash = rd(8 + i * 8)?;
        let off = rd(12 + i * 8)? as usize;
        let rest = data.get(off..).ok_or_else(|| Error::Malformed("GXT2: bad string offset".into()))?;
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        out.push((hash, String::from_utf8_lossy(&rest[..end]).into_owned()));
    }
    Ok(out)
}

pub fn write(entries: &[(u32, String)]) -> Vec<u8> {
    let mut e: Vec<&(u32, String)> = entries.iter().collect();
    e.sort_by_key(|x| x.0); // looked up by binary search
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&(e.len() as u32).to_le_bytes());
    let mut off = 16 + 8 * e.len() as u32;
    let mut strings = Vec::new();
    for (h, s) in &e {
        out.extend_from_slice(&h.to_le_bytes());
        out.extend_from_slice(&off.to_le_bytes());
        strings.extend_from_slice(s.as_bytes());
        strings.push(0);
        off += s.len() as u32 + 1;
    }
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&off.to_le_bytes());
    out.extend_from_slice(&strings);
    out
}

pub fn to_text(entries: &[(u32, String)]) -> String {
    let mut s = String::from("# GTA V text table: one entry per line, LABEL = text\n# Labels are hashes (0x…); a label name works too.\n");
    for (h, t) in entries {
        s.push_str(&format!("0x{h:08X} = {}\n", t.replace('\n', "\\n")));
    }
    s
}

pub fn from_text(text: &str) -> Result<Vec<(u32, String)>> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let l = line.trim_start_matches('\u{feff}');
        if l.trim().is_empty() || l.trim_start().starts_with('#') {
            continue;
        }
        let (k, v) = l.split_once(" = ").or_else(|| l.split_once('=')).ok_or_else(|| Error::Invalid(format!("line {}: expected LABEL = text", n + 1)))?;
        let k = k.trim();
        let hash = match k.strip_prefix("0x").or_else(|| k.strip_prefix("0X")) {
            Some(h) => u32::from_str_radix(h, 16).map_err(|_| Error::Invalid(format!("line {}: bad hash {k}", n + 1)))?,
            None => crate::crypto::joaat(k),
        };
        out.push((hash, v.replace("\\n", "\n")));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let e = vec![(0x1234, "Hello ~g~world".to_string()), (crate::crypto::joaat("ADDER"), "Adder".to_string())];
        let b = write(&e);
        let mut p = parse(&b).unwrap();
        p.sort();
        let mut e2 = e.clone();
        e2.sort();
        assert_eq!(p, e2);
        let t = to_text(&p);
        let back = from_text(&t).unwrap();
        assert_eq!(back, p);
        assert_eq!(from_text("ADDER = Fast car").unwrap()[0].0, crate::crypto::joaat("adder"));
    }
}
