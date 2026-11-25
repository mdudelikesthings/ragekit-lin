//! GTA V decryption keys.
//!
//! No key material ships with ragekit. Keys come from the user's own copy of
//! the game, in one of these ways:
//!
//! * [`Keys::search_bytes`] on `GTA5.exe` read from disk. Modern executables
//!   are packed, so this usually only yields the AES key.
//! * [`Keys::search_process`] on the running game (e.g. under Proton), which
//!   yields everything because the executable is unpacked in memory.
//! * [`Keys::load_codewalker_magic`]: decode CodeWalker's `magic.dat` bundle,
//!   which needs the AES key first.
//! * [`Keys::find_by_probe`] for Max Payne 3 and Red Dead Redemption: every
//!   32-byte window of the executable (or game memory) is tried as the AES
//!   key against one of the game's own encrypted archives, so no key or key
//!   hash needs to ship for those games either.
//! * [`Keys::load_dir`]: key files in the layout used by CodeWalker / GTAUtil
//!   (`gtav_aes_key.dat`, `gtav_ng_key.dat`, `gtav_ng_decrypt_tables.dat`,
//!   `gtav_hash_lut.dat`).

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;

use rayon::prelude::*;
use sha1::{Digest, Sha1};

use crate::crypto::{aes_decrypt, joaat_bytes};
use crate::key_hashes::*;
use crate::{Error, Result};

/// One expanded NG key: 17 round keys of 4 words.
pub type NgKey = [u32; 68];
/// NG decrypt tables: 17 rounds x 16 byte positions x 256 entries.
pub type NgTables = [[[u32; 256]; 16]; 17];

pub const AES_FILE: &str = "gtav_aes_key.dat";
pub const NG_KEYS_FILE: &str = "gtav_ng_key.dat";
pub const NG_TABLES_FILE: &str = "gtav_ng_decrypt_tables.dat";
pub const HASH_LUT_FILE: &str = "gtav_hash_lut.dat";
pub const IV_AES_FILE: &str = "gtaiv_aes_key.dat";
pub const MP3_AES_FILE: &str = "mp3_aes_key.dat";
pub const RDR_AES_FILE: &str = "rdr_aes_key.dat";

const NG_KEYS_LEN: usize = 101 * 272;
const NG_TABLES_LEN: usize = 17 * 16 * 256 * 4;

#[derive(Clone, Default)]
pub struct Keys {
    /// GTA V AES key.
    pub aes: Option<[u8; 32]>,
    /// GTA IV / EFLC AES key.
    pub iv_aes: Option<[u8; 32]>,
    /// Max Payne 3 AES key.
    pub mp3_aes: Option<[u8; 32]>,
    /// Red Dead Redemption AES key.
    pub rdr_aes: Option<[u8; 32]>,
    pub ng_keys: Option<Box<[NgKey; 101]>>,
    pub ng_tables: Option<Box<NgTables>>,
    pub hash_lut: Option<Box<[u8; 256]>>,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material.
        f.debug_struct("Keys")
            .field("aes", &self.aes.is_some())
            .field("iv_aes", &self.iv_aes.is_some())
            .field("mp3_aes", &self.mp3_aes.is_some())
            .field("rdr_aes", &self.rdr_aes.is_some())
            .field("ng_keys", &self.ng_keys.is_some())
            .field("ng_tables", &self.ng_tables.is_some())
            .field("hash_lut", &self.hash_lut.is_some())
            .finish()
    }
}

/// What a key search found.
#[derive(Debug, Default, Clone, Copy)]
pub struct SearchReport {
    pub aes: bool,
    pub iv_aes: bool,
    pub ng_keys_found: usize,
    pub ng_tables_found: usize,
    pub hash_lut: bool,
}

impl SearchReport {
    /// Everything needed for GTA V.
    pub fn complete(&self) -> bool {
        self.aes && self.ng_keys_found == 101 && self.ng_tables_found == 272 && self.hash_lut
    }
}

impl Keys {
    pub fn has_aes(&self) -> bool {
        self.aes.is_some()
    }
    pub fn has_iv(&self) -> bool {
        self.iv_aes.is_some()
    }
    pub fn has_ng(&self) -> bool {
        self.ng_keys.is_some() && self.ng_tables.is_some() && self.hash_lut.is_some()
    }

    /// Load whatever key files exist in `dir`. Missing files are skipped.
    pub fn load_dir(dir: &Path) -> Result<Keys> {
        let mut k = Keys::default();
        if let Some(b) = read_opt(&dir.join(AES_FILE))? {
            k.aes = Some(b.as_slice().try_into().map_err(|_| Error::Keys(format!("{AES_FILE} must be 32 bytes")))?);
        }
        if let Some(b) = read_opt(&dir.join(IV_AES_FILE))? {
            k.iv_aes = Some(b.as_slice().try_into().map_err(|_| Error::Keys(format!("{IV_AES_FILE} must be 32 bytes")))?);
        }
        for (file, slot) in [(MP3_AES_FILE, &mut k.mp3_aes), (RDR_AES_FILE, &mut k.rdr_aes)] {
            if let Some(b) = read_opt(&dir.join(file))? {
                *slot = Some(b.as_slice().try_into().map_err(|_| Error::Keys(format!("{file} must be 32 bytes")))?);
            }
        }
        if let Some(b) = read_opt(&dir.join(NG_KEYS_FILE))? {
            k.ng_keys = Some(parse_ng_keys(&b)?);
        }
        if let Some(b) = read_opt(&dir.join(NG_TABLES_FILE))? {
            k.ng_tables = Some(parse_ng_tables(&b)?);
        }
        if let Some(b) = read_opt(&dir.join(HASH_LUT_FILE))? {
            let lut: [u8; 256] = b.as_slice().try_into().map_err(|_| Error::Keys(format!("{HASH_LUT_FILE} must be 256 bytes")))?;
            k.hash_lut = Some(Box::new(lut));
        }
        k.verify()?;
        Ok(k)
    }

    /// Save present keys to `dir` (created if needed), owner-readable only.
    pub fn save_dir(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        if let Some(a) = &self.aes {
            write_private(&dir.join(AES_FILE), a)?;
        }
        if let Some(a) = &self.iv_aes {
            write_private(&dir.join(IV_AES_FILE), a)?;
        }
        if let Some(a) = &self.mp3_aes {
            write_private(&dir.join(MP3_AES_FILE), a)?;
        }
        if let Some(a) = &self.rdr_aes {
            write_private(&dir.join(RDR_AES_FILE), a)?;
        }
        if let Some(k) = &self.ng_keys {
            let mut b = Vec::with_capacity(NG_KEYS_LEN);
            for key in k.iter() {
                for w in key {
                    b.extend_from_slice(&w.to_le_bytes());
                }
            }
            write_private(&dir.join(NG_KEYS_FILE), &b)?;
        }
        if let Some(t) = &self.ng_tables {
            let mut b = Vec::with_capacity(NG_TABLES_LEN);
            for r in t.iter() {
                for p in r {
                    for w in p {
                        b.extend_from_slice(&w.to_le_bytes());
                    }
                }
            }
            write_private(&dir.join(NG_TABLES_FILE), &b)?;
        }
        if let Some(l) = &self.hash_lut {
            write_private(&dir.join(HASH_LUT_FILE), &l[..])?;
        }
        Ok(())
    }

    /// Check present keys against their known SHA-1 hashes.
    pub fn verify(&self) -> Result<()> {
        if let Some(a) = &self.aes {
            if sha1(a) != AES_KEY_HASH {
                return Err(Error::Keys("AES key does not match the known GTA V key hash".into()));
            }
        }
        if let Some(a) = &self.iv_aes {
            if sha1(a) != IV_AES_KEY_HASH {
                return Err(Error::Keys("GTA IV AES key does not match the known key hash".into()));
            }
        }
        if let Some(l) = &self.hash_lut {
            if sha1(&l[..]) != HASH_LUT_HASH {
                return Err(Error::Keys("hash lookup table does not match the known hash".into()));
            }
        }
        if let Some(k) = &self.ng_keys {
            for (i, key) in k.iter().enumerate() {
                let bytes: Vec<u8> = key.iter().flat_map(|w| w.to_le_bytes()).collect();
                if sha1(&bytes) != NG_KEY_HASHES[i] {
                    return Err(Error::Keys(format!("NG key {i} does not match its known hash")));
                }
            }
        }
        if let Some(t) = &self.ng_tables {
            for r in 0..17 {
                for p in 0..16 {
                    let bytes: Vec<u8> = t[r][p].iter().flat_map(|w| w.to_le_bytes()).collect();
                    if sha1(&bytes) != NG_DECRYPT_TABLE_HASHES[r * 16 + p] {
                        return Err(Error::Keys(format!("NG table {r}/{p} does not match its known hash")));
                    }
                }
            }
        }
        Ok(())
    }

    /// Fill in anything missing from `other`.
    pub fn merge(&mut self, other: Keys) {
        if self.aes.is_none() {
            self.aes = other.aes;
        }
        if self.iv_aes.is_none() {
            self.iv_aes = other.iv_aes;
        }
        if self.mp3_aes.is_none() {
            self.mp3_aes = other.mp3_aes;
        }
        if self.rdr_aes.is_none() {
            self.rdr_aes = other.rdr_aes;
        }
        if self.ng_keys.is_none() {
            self.ng_keys = other.ng_keys;
        }
        if self.ng_tables.is_none() {
            self.ng_tables = other.ng_tables;
        }
        if self.hash_lut.is_none() {
            self.hash_lut = other.hash_lut;
        }
    }

    /// Search a byte buffer (an executable or a memory dump) for key material
    /// by SHA-1 hash, at 8-byte alignment. Fills in only missing keys.
    pub fn search_bytes(&mut self, data: &[u8]) -> SearchReport {
        let mut s = Searcher::new(self);
        s.scan(data, 0);
        s.finish(self)
    }

    /// Search the memory of a running process (Linux `/proc/<pid>/mem`).
    /// Needs ptrace permission on the process: same user with
    /// `kernel.yama.ptrace_scope = 0`, or root.
    pub fn search_process(&mut self, pid: u32) -> Result<SearchReport> {
        let mut s = Searcher::new(self);
        scan_process(pid, &mut |buf, base| {
            s.scan(buf, base);
            s.done()
        })?;
        Ok(s.finish(self))
    }

    /// Try every 32-byte window of `data` as an AES key and return the first
    /// one `probe` accepts. Probes come from [`crate::gta4::rpf_key_probe`]
    /// and [`crate::rdr::key_probe`]. Keys are usually 4-byte aligned, so
    /// those offsets go first.
    pub fn find_by_probe(data: &[u8], probe: &(dyn Fn(&[u8; 32]) -> bool + Sync)) -> Option<[u8; 32]> {
        if data.len() < 32 {
            return None;
        }
        let last = data.len() - 32;
        let try_at = |o: usize| -> Option<[u8; 32]> {
            let k: &[u8; 32] = data[o..o + 32].try_into().unwrap();
            // Skip windows that can't be a random key (padding, tables of
            // small integers, text) before paying for the AES key schedule.
            let zeros = k.iter().filter(|&&b| b == 0).count();
            if zeros > 6 || k[..8] == k[8..16] {
                return None;
            }
            probe(k).then_some(*k)
        };
        (0..=last / 4).into_par_iter().find_map_first(|i| try_at(i * 4)).or_else(|| {
            (0..=last).into_par_iter().filter(|o| o % 4 != 0).find_map_first(try_at)
        })
    }

    /// [`Keys::find_by_probe`] over a running process's memory.
    pub fn find_by_probe_in_process(pid: u32, probe: &(dyn Fn(&[u8; 32]) -> bool + Sync)) -> Result<Option<[u8; 32]>> {
        let mut found = None;
        scan_process(pid, &mut |buf, _| {
            found = Keys::find_by_probe(buf, probe);
            found.is_some()
        })?;
        Ok(found)
    }

    /// Decode CodeWalker's `magic.dat` bundle (NG keys, NG tables, hash LUT).
    /// Requires the AES key.
    pub fn load_codewalker_magic(&mut self, magic: &[u8]) -> Result<()> {
        let aes = self.aes.ok_or(Error::MissingKey("AES key (needed to decode magic.dat)"))?;
        let seed = joaat_bytes(&aes) as i32;
        let mut rng = DotNetRandom::new(seed);
        let n = magic.len();
        let mut rb = vec![vec![0u8; n]; 4];
        for r in rb.iter_mut() {
            rng.next_bytes(r);
        }
        let mut db: Vec<u8> = (0..n)
            .map(|i| magic[i].wrapping_sub(rb[0][i]).wrapping_sub(rb[1][i]).wrapping_sub(rb[2][i]).wrapping_sub(rb[3][i]))
            .collect();
        aes_decrypt(&mut db, &aes);
        let mut out = Vec::new();
        flate2::read::DeflateDecoder::new(&db[..])
            .read_to_end(&mut out)
            .map_err(|e| Error::Keys(format!("magic.dat did not decode (wrong file or AES key?): {e}")))?;
        if out.len() < NG_KEYS_LEN + NG_TABLES_LEN + 256 {
            return Err(Error::Keys("magic.dat decoded to too little data".into()));
        }
        let mut k = Keys {
            ng_keys: Some(parse_ng_keys(&out[..NG_KEYS_LEN])?),
            ng_tables: Some(parse_ng_tables(&out[NG_KEYS_LEN..NG_KEYS_LEN + NG_TABLES_LEN])?),
            hash_lut: Some(Box::new(out[NG_KEYS_LEN + NG_TABLES_LEN..NG_KEYS_LEN + NG_TABLES_LEN + 256].try_into().unwrap())),
            ..Default::default()
        };
        k.verify()?;
        self.ng_keys = k.ng_keys.take();
        self.ng_tables = k.ng_tables.take();
        self.hash_lut = k.hash_lut.take();
        Ok(())
    }
}

/// Feed the readable memory of process `pid` to `f` in overlapping windows
/// until `f` returns true. Needs ptrace permission on the process: same
/// user with `kernel.yama.ptrace_scope = 0`, or root.
pub fn scan_process(pid: u32, f: &mut dyn FnMut(&[u8], u64) -> bool) -> Result<()> {
    use std::os::unix::fs::FileExt;
    let maps = fs::read_to_string(format!("/proc/{pid}/maps"))?;
    let mem = fs::File::open(format!("/proc/{pid}/mem"))?;
    let mut readable_any = false;
    for line in maps.lines() {
        let mut it = line.split_whitespace();
        let (Some(range), Some(perms)) = (it.next(), it.next()) else { continue };
        if !perms.starts_with('r') {
            continue;
        }
        let path = line.split_whitespace().nth(5).unwrap_or("");
        if path.starts_with("[v") {
            continue; // [vvar], [vdso], [vsyscall]
        }
        let Some((a, b)) = range.split_once('-') else { continue };
        let (Ok(start), Ok(end)) = (u64::from_str_radix(a, 16), u64::from_str_radix(b, 16)) else { continue };
        // Read in 64 MiB windows with overlap so keys spanning a boundary are seen.
        const WIN: u64 = 64 << 20;
        const OVERLAP: u64 = 0x400;
        let mut pos = start;
        while pos < end {
            let len = (end - pos).min(WIN + OVERLAP) as usize;
            let mut buf = vec![0u8; len];
            if let Ok(n @ 1..) = mem.read_at(&mut buf, pos) {
                readable_any = true;
                buf.truncate(n);
                if f(&buf, pos) {
                    return Ok(());
                }
            }
            pos += WIN;
        }
    }
    if !readable_any {
        return Err(Error::Keys(format!(
            "could not read memory of process {pid}; run as root or set kernel.yama.ptrace_scope=0"
        )));
    }
    Ok(())
}

fn sha1(data: &[u8]) -> [u8; 20] {
    Sha1::digest(data).into()
}

fn read_opt(p: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(p) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_private(p: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(p)?;
    f.write_all(data)?;
    Ok(())
}

fn parse_ng_keys(b: &[u8]) -> Result<Box<[NgKey; 101]>> {
    if b.len() != NG_KEYS_LEN {
        return Err(Error::Keys(format!("NG key data must be {NG_KEYS_LEN} bytes, got {}", b.len())));
    }
    let mut k = Box::new([[0u32; 68]; 101]);
    for (i, key) in k.iter_mut().enumerate() {
        for (j, w) in key.iter_mut().enumerate() {
            let o = i * 272 + j * 4;
            *w = u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        }
    }
    Ok(k)
}

fn parse_ng_tables(b: &[u8]) -> Result<Box<NgTables>> {
    if b.len() != NG_TABLES_LEN {
        return Err(Error::Keys(format!("NG table data must be {NG_TABLES_LEN} bytes, got {}", b.len())));
    }
    let mut t: Box<NgTables> = vec![[[0u32; 256]; 16]; 17].into_boxed_slice().try_into().unwrap();
    let mut o = 0;
    for r in t.iter_mut() {
        for p in r.iter_mut() {
            for w in p.iter_mut() {
                *w = u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
                o += 4;
            }
        }
    }
    Ok(t)
}

/// Hash search state. Targets are grouped by the length of data they cover.
struct Searcher {
    // length -> (hash -> slot)
    targets: Vec<((usize, usize), HashMap<[u8; 20], usize>)>, // (length, alignment)
    found: Vec<Option<Vec<u8>>>, // slot -> bytes
}

// Slots: 0 = AES, 1 = LUT, 2..103 = NG keys, 103..375 = NG tables.
const SLOT_AES: usize = 0;
const SLOT_LUT: usize = 1;
const SLOT_NGK: usize = 2;
const SLOT_NGT: usize = 103;
const SLOT_IV: usize = 375;
const SLOTS: usize = 376;

impl Searcher {
    fn new(have: &Keys) -> Self {
        let mut by_len: HashMap<(usize, usize), HashMap<[u8; 20], usize>> = HashMap::new();
        if have.aes.is_none() {
            by_len.entry((32, 8)).or_default().insert(AES_KEY_HASH, SLOT_AES);
        }
        if have.iv_aes.is_none() {
            by_len.entry((32, 4)).or_default().insert(IV_AES_KEY_HASH, SLOT_IV);
        }
        if have.hash_lut.is_none() {
            by_len.entry((256, 8)).or_default().insert(HASH_LUT_HASH, SLOT_LUT);
        }
        if have.ng_keys.is_none() {
            for (i, h) in NG_KEY_HASHES.iter().enumerate() {
                by_len.entry((272, 8)).or_default().insert(*h, SLOT_NGK + i);
            }
        }
        if have.ng_tables.is_none() {
            for (i, h) in NG_DECRYPT_TABLE_HASHES.iter().enumerate() {
                by_len.entry((1024, 8)).or_default().insert(*h, SLOT_NGT + i);
            }
        }
        Searcher { targets: by_len.into_iter().collect(), found: vec![None; SLOTS] }
    }

    fn done(&self) -> bool {
        self.targets.iter().all(|(_, m)| m.values().all(|&s| self.found[s].is_some()))
    }

    fn scan(&mut self, data: &[u8], base: u64) {
        // Keep the virtual address alignment consistent across windows.
        let hits: Vec<(usize, Vec<u8>)> = self
            .targets
            .par_iter()
            .flat_map(|((len, align), map)| {
                let (len, align) = (*len, *align as u64);
                let skip = ((align - (base % align)) % align) as usize;
                if data.len() < len + skip {
                    return Vec::new();
                }
                let last = data.len() - len;
                (0..=(last - skip) / align as usize)
                    .into_par_iter()
                    .map(move |k| skip + k * align as usize)
                    .filter_map(|o| {
                        let w = &data[o..o + len];
                        map.get(&sha1(w)).map(|&slot| (slot, w.to_vec()))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        for (slot, bytes) in hits {
            self.found[slot].get_or_insert(bytes);
        }
    }

    fn finish(self, keys: &mut Keys) -> SearchReport {
        let f = &self.found;
        if let Some(b) = &f[SLOT_AES] {
            keys.aes = Some(b.as_slice().try_into().unwrap());
        }
        if let Some(b) = &f[SLOT_IV] {
            keys.iv_aes = Some(b.as_slice().try_into().unwrap());
        }
        if let Some(b) = &f[SLOT_LUT] {
            keys.hash_lut = Some(Box::new(b.as_slice().try_into().unwrap()));
        }
        let ngk = (0..101).filter(|i| f[SLOT_NGK + i].is_some()).count();
        if ngk == 101 && keys.ng_keys.is_none() {
            let mut all = Vec::with_capacity(NG_KEYS_LEN);
            for i in 0..101 {
                all.extend_from_slice(f[SLOT_NGK + i].as_ref().unwrap());
            }
            keys.ng_keys = parse_ng_keys(&all).ok();
        }
        let ngt = (0..272).filter(|i| f[SLOT_NGT + i].is_some()).count();
        if ngt == 272 && keys.ng_tables.is_none() {
            let mut all = Vec::with_capacity(NG_TABLES_LEN);
            for i in 0..272 {
                all.extend_from_slice(f[SLOT_NGT + i].as_ref().unwrap());
            }
            keys.ng_tables = parse_ng_tables(&all).ok();
        }
        SearchReport {
            aes: keys.aes.is_some(),
            iv_aes: keys.iv_aes.is_some(),
            ng_keys_found: if keys.ng_keys.is_some() { 101 } else { ngk },
            ng_tables_found: if keys.ng_tables.is_some() { 272 } else { ngt },
            hash_lut: keys.hash_lut.is_some(),
        }
    }
}

/// .NET's seeded `System.Random` (the legacy subtractive generator), needed
/// to decode CodeWalker's magic.dat.
pub(crate) struct DotNetRandom {
    seeds: [i32; 56],
    inext: usize,
    inextp: usize,
}

impl DotNetRandom {
    const MBIG: i32 = i32::MAX;
    const MSEED: i32 = 161_803_398;

    pub fn new(seed: i32) -> Self {
        let mut sa = [0i32; 56];
        let subtraction = if seed == i32::MIN { i32::MAX } else { seed.abs() };
        let mut mj = Self::MSEED - subtraction;
        sa[55] = mj;
        let mut mk: i32 = 1;
        for i in 1..55 {
            let ii = (21 * i) % 55;
            sa[ii] = mk;
            mk = mj.wrapping_sub(mk);
            if mk < 0 {
                mk = mk.wrapping_add(Self::MBIG);
            }
            mj = sa[ii];
        }
        for _ in 1..5 {
            for i in 1..56 {
                sa[i] = sa[i].wrapping_sub(sa[1 + (i + 30) % 55]);
                if sa[i] < 0 {
                    sa[i] = sa[i].wrapping_add(Self::MBIG);
                }
            }
        }
        DotNetRandom { seeds: sa, inext: 0, inextp: 21 }
    }

    fn sample(&mut self) -> i32 {
        self.inext += 1;
        if self.inext >= 56 {
            self.inext = 1;
        }
        self.inextp += 1;
        if self.inextp >= 56 {
            self.inextp = 1;
        }
        let mut r = self.seeds[self.inext].wrapping_sub(self.seeds[self.inextp]);
        if r == Self::MBIG {
            r -= 1;
        }
        if r < 0 {
            r = r.wrapping_add(Self::MBIG);
        }
        self.seeds[self.inext] = r;
        r
    }

    #[allow(dead_code)]
    pub fn next(&mut self) -> i32 {
        self.sample()
    }

    pub fn next_bytes(&mut self, buf: &mut [u8]) {
        for b in buf {
            *b = self.sample() as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotnet_random_matches_reference() {
        // Values produced by `new System.Random(42).Next()` on .NET.
        let mut r = DotNetRandom::new(42);
        let got: Vec<i32> = (0..3).map(|_| r.next()).collect();
        assert_eq!(got, vec![1434747710, 302596119, 269548474]);
    }

    #[test]
    fn search_finds_planted_lut_shape() {
        // The searcher should report nothing in random data.
        let mut k = Keys::default();
        let data = vec![0xABu8; 1 << 16];
        let rep = k.search_bytes(&data);
        assert!(!rep.aes && rep.ng_keys_found == 0);
    }
}
