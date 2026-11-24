//! RSC7 resource helpers (the container format of .ytd, .ydr, .yft, .ymap ...).

pub const RSC7_MAGIC: u32 = 0x3743_5352; // "RSC7"

/// Size in bytes described by a system/graphics page-flags word.
pub fn size_from_flags(flags: u32) -> u64 {
    let s0 = ((flags >> 27) & 0x1) as u64;
    let s1 = (((flags >> 26) & 0x1) << 1) as u64;
    let s2 = (((flags >> 25) & 0x1) << 2) as u64;
    let s3 = (((flags >> 24) & 0x1) << 3) as u64;
    let s4 = (((flags >> 17) & 0x7F) << 4) as u64;
    let s5 = (((flags >> 11) & 0x3F) << 5) as u64;
    let s6 = (((flags >> 7) & 0xF) << 6) as u64;
    let s7 = (((flags >> 5) & 0x3) << 7) as u64;
    let s8 = (((flags >> 4) & 0x1) << 8) as u64;
    let ss = flags & 0xF;
    let base = 0x200u64 << ss;
    base * (s0 + s1 + s2 + s3 + s4 + s5 + s6 + s7 + s8)
}

/// Resource version from the two flag words (high nibbles).
pub fn version_from_flags(sys: u32, gfx: u32) -> u32 {
    (((sys >> 28) & 0xF) << 4) | ((gfx >> 28) & 0xF)
}

/// Build the 16-byte RSC7 header.
pub fn header(sys: u32, gfx: u32) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[0..4].copy_from_slice(&RSC7_MAGIC.to_le_bytes());
    h[4..8].copy_from_slice(&version_from_flags(sys, gfx).to_le_bytes());
    h[8..12].copy_from_slice(&sys.to_le_bytes());
    h[12..16].copy_from_slice(&gfx.to_le_bytes());
    h
}

/// Parse an RSC7 header: returns (version, system flags, graphics flags).
pub fn parse_header(data: &[u8]) -> Option<(u32, u32, u32)> {
    if data.len() < 16 || u32::from_le_bytes(data[0..4].try_into().unwrap()) != RSC7_MAGIC {
        return None;
    }
    let r = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    Some((r(4), r(8), r(12)))
}

/// Common resource extensions and what they hold, for display.
pub fn describe_extension(name: &str) -> Option<&'static str> {
    let ext = name.rsplit('.').next()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "ytd" => "Texture dictionary",
        "ydr" => "Drawable",
        "ydd" => "Drawable dictionary",
        "yft" => "Fragment (vehicle/breakable)",
        "ybn" => "Collision bounds",
        "ymap" => "Map placement",
        "ytyp" => "Archetype definitions",
        "ycd" => "Clip dictionary (animations)",
        "ynv" => "Nav mesh",
        "ynd" => "Path nodes",
        "ysc" => "Compiled script",
        "ypt" => "Particle effects",
        "yld" => "Cloth",
        "yed" => "Expression dictionary",
        "awc" => "Audio wave container",
        "rpf" => "Nested archive",
        "meta" | "xml" => "Metadata (XML)",
        "ymt" => "Metadata (binary)",
        "gxt2" => "Text table",
        "dat" | "rel" => "Data",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_sizes() {
        // From CodeWalker's notes: ss:0, s4:1 => 8192.
        assert_eq!(size_from_flags(1 << 17), 8192);
        // ss:1, s8:1 => 262144.
        assert_eq!(size_from_flags((1 << 4) | 1), 262144);
    }
}
