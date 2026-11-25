//! GTA V compiled scripts (`.ysc`): header facts, the string table and the
//! native calls. Scripts can be extracted and replaced like any resource;
//! this module is for looking inside them.
//!
//! Layout as documented by the open GTA V script decompiler projects.

use crate::{Error, Result};

const PAGE: usize = 0x4000;

#[derive(Debug, Clone, Default)]
pub struct Script {
    pub name: String,
    pub name_hash: u32,
    pub code_length: u32,
    pub parameters: u32,
    pub statics: u32,
    pub globals: u32,
    pub globals_signature: u32,
    /// Native hashes as the game resolves them.
    pub natives: Vec<u64>,
    /// Every string in the string table, in order.
    pub strings: Vec<String>,
    /// The bytecode, pages joined.
    pub code: Vec<u8>,
}

fn bad(s: impl Into<String>) -> Error {
    Error::Malformed(s.into())
}

pub fn is_ysc(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".ysc") || name.to_ascii_lowercase().ends_with(".ysc.full")
}

pub fn parse(data: &[u8]) -> Result<Script> {
    let (sys, _) = rage_formats::prepare_rsc7(data).map_err(|e| Error::Invalid(e.to_string()))?;
    let u32_at = |o: usize| sys.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| bad(format!("read past end at {o:#x}")));
    let u64_at = |o: usize| sys.get(o..o + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| bad(format!("read past end at {o:#x}")));
    let ptr = |o: usize| -> Result<Option<usize>> {
        match u64_at(o)? {
            0 => Ok(None),
            p if p >> 28 == 5 => Ok(Some((p & 0x0FFF_FFFF) as usize)),
            p => Err(bad(format!("unexpected pointer {p:#x} at {o:#x}"))),
        }
    };
    // Pages of `total` bytes listed by a pointer table at `table`.
    let pages = |table: Option<usize>, total: usize| -> Result<Vec<u8>> {
        let Some(t) = table else { return Ok(Vec::new()) };
        if total > 64 << 20 {
            return Err(bad("script section too large"));
        }
        let mut out = Vec::with_capacity(total);
        for i in 0..total.div_ceil(PAGE) {
            let p = ptr(t + i * 8)?.ok_or_else(|| bad("missing page"))?;
            let n = (total - i * PAGE).min(PAGE);
            out.extend_from_slice(sys.get(p..p + n).ok_or_else(|| bad("page runs past end"))?);
        }
        Ok(out)
    };

    let code_length = u32_at(0x1C)?;
    let mut s = Script {
        globals_signature: u32_at(0x18)?,
        code_length,
        parameters: u32_at(0x20)?,
        statics: u32_at(0x24)?,
        globals: u32_at(0x28)?,
        name_hash: u32_at(0x58)?,
        ..Default::default()
    };
    let natives_count = u32_at(0x2C)? as usize;
    if natives_count > 100_000 {
        return Err(bad("implausible native count"));
    }
    if let Some(n) = ptr(0x40)? {
        for i in 0..natives_count {
            let raw = u64_at(n + i * 8)?;
            s.natives.push(raw.rotate_left(((code_length as usize + i) & 63) as u32));
        }
    }
    if let Some(p) = ptr(0x60)? {
        let rest = sys.get(p..).unwrap_or(&[]);
        let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len()).min(256);
        s.name = String::from_utf8_lossy(&rest[..end]).into_owned();
    }
    let strings_size = u32_at(0x70)? as usize;
    let table = pages(ptr(0x68)?, strings_size)?;
    s.strings = table.split(|&c| c == 0).filter(|b| !b.is_empty()).map(|b| String::from_utf8_lossy(b).into_owned()).collect();
    s.code = pages(ptr(0x10)?, code_length as usize)?;
    Ok(s)
}

impl Script {
    /// A plain-text report: header, natives, strings.
    pub fn report(&self) -> String {
        use std::fmt::Write;
        let mut o = String::new();
        let _ = writeln!(o, "Script:      {}", if self.name.is_empty() { "(unnamed)" } else { &self.name });
        let _ = writeln!(o, "Name hash:   0x{:08X}", self.name_hash);
        let _ = writeln!(o, "Code:        {} bytes", self.code_length);
        let _ = writeln!(o, "Parameters:  {}", self.parameters);
        let _ = writeln!(o, "Statics:     {}", self.statics);
        let _ = writeln!(o, "Globals:     {} (signature 0x{:08X})", self.globals, self.globals_signature);
        let _ = writeln!(o, "Natives:     {}", self.natives.len());
        let _ = writeln!(o, "Strings:     {}\n", self.strings.len());
        let _ = writeln!(o, "== Natives ==");
        for (i, n) in self.natives.iter().enumerate() {
            let _ = writeln!(o, "{i:5}  0x{n:016X}");
        }
        let _ = writeln!(o, "\n== Strings ==");
        for s in &self.strings {
            let _ = writeln!(o, "{s}");
        }
        o
    }
}
