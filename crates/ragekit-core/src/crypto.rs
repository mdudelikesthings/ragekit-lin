//! GTA V archive cryptography: AES-256-ECB, the "NG" table cipher, and the
//! filename hash used to pick an NG key.
//!
//! Algorithms ported from CodeWalker's GTACrypto.cs (MIT, (c) 2015 Neodymium).

use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes256;

use crate::keys::{Keys, NgKey, NgTables};
use crate::{Error, Result};

/// Decrypt in place with AES-256-ECB. A trailing partial block is left as is,
/// matching the game.
pub fn aes_decrypt(data: &mut [u8], key: &[u8; 32]) {
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let len = data.len() - data.len() % 16;
    for block in data[..len].chunks_exact_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
}

/// Encrypt in place with AES-256-ECB. A trailing partial block is left as is.
pub fn aes_encrypt(data: &mut [u8], key: &[u8; 32]) {
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let len = data.len() - data.len() % 16;
    for block in data[..len].chunks_exact_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
}

/// GTA IV table-of-contents decryption: AES-256-ECB applied 16 times.
pub fn iv_decrypt(data: &mut [u8], key: &[u8; 32]) {
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let len = data.len() - data.len() % 16;
    for block in data[..len].chunks_exact_mut(16) {
        let b = GenericArray::from_mut_slice(block);
        for _ in 0..16 {
            cipher.decrypt_block(b);
        }
    }
}

/// Inverse of [`iv_decrypt`].
pub fn iv_encrypt(data: &mut [u8], key: &[u8; 32]) {
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let len = data.len() - data.len() % 16;
    for block in data[..len].chunks_exact_mut(16) {
        let b = GenericArray::from_mut_slice(block);
        for _ in 0..16 {
            cipher.encrypt_block(b);
        }
    }
}

/// Rockstar's filename hash (uses the 256-byte lookup table from the game).
pub fn gta5_hash(lut: &[u8; 256], text: &str) -> u32 {
    let mut result: u32 = 0;
    for c in text.bytes() {
        let temp = 1025u32.wrapping_mul((lut[c as usize] as u32).wrapping_add(result));
        result = (temp >> 6) ^ temp;
    }
    let r9 = result.wrapping_mul(9);
    32769u32.wrapping_mul((r9 >> 11) ^ r9)
}

/// Index of the NG key for a file with the given name and length.
pub fn ng_key_index(lut: &[u8; 256], name: &str, length: u32) -> usize {
    let hash = gta5_hash(lut, name);
    (hash.wrapping_add(length).wrapping_add(101 - 40) % 101) as usize
}

/// Decrypt in place with the NG cipher, picking the key from name + length.
pub fn ng_decrypt_named(keys: &Keys, data: &mut [u8], name: &str, length: u32) -> Result<()> {
    let lut = keys.hash_lut.as_ref().ok_or(Error::MissingKey("NG hash lookup table"))?;
    let ng_keys = keys.ng_keys.as_ref().ok_or(Error::MissingKey("NG keys"))?;
    let tables = keys.ng_tables.as_ref().ok_or(Error::MissingKey("NG decrypt tables"))?;
    let key = &ng_keys[ng_key_index(lut, name, length)];
    ng_decrypt(data, key, tables);
    Ok(())
}

/// Decrypt in place with the NG cipher and an explicit key.
/// A trailing partial block is left as is.
pub fn ng_decrypt(data: &mut [u8], key: &NgKey, tables: &NgTables) {
    let len = data.len() - data.len() % 16;
    for block in data[..len].chunks_exact_mut(16) {
        let mut b: [u8; 16] = block.try_into().unwrap();
        b = round_a(&b, &key[0..4], &tables[0]);
        b = round_a(&b, &key[4..8], &tables[1]);
        for k in 2..=15 {
            b = round_b(&b, &key[4 * k..4 * k + 4], &tables[k]);
        }
        b = round_a(&b, &key[64..68], &tables[16]);
        block.copy_from_slice(&b);
    }
}

#[inline]
fn pack(x: [u32; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, v) in x.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

// Rounds 1, 2 and 17.
#[inline]
fn round_a(d: &[u8; 16], key: &[u32], t: &[[u32; 256]; 16]) -> [u8; 16] {
    let x1 = t[0][d[0] as usize] ^ t[1][d[1] as usize] ^ t[2][d[2] as usize] ^ t[3][d[3] as usize] ^ key[0];
    let x2 = t[4][d[4] as usize] ^ t[5][d[5] as usize] ^ t[6][d[6] as usize] ^ t[7][d[7] as usize] ^ key[1];
    let x3 = t[8][d[8] as usize] ^ t[9][d[9] as usize] ^ t[10][d[10] as usize] ^ t[11][d[11] as usize] ^ key[2];
    let x4 = t[12][d[12] as usize] ^ t[13][d[13] as usize] ^ t[14][d[14] as usize] ^ t[15][d[15] as usize] ^ key[3];
    pack([x1, x2, x3, x4])
}

// Rounds 3 to 16.
#[inline]
fn round_b(d: &[u8; 16], key: &[u32], t: &[[u32; 256]; 16]) -> [u8; 16] {
    let x1 = t[0][d[0] as usize] ^ t[7][d[7] as usize] ^ t[10][d[10] as usize] ^ t[13][d[13] as usize] ^ key[0];
    let x2 = t[1][d[1] as usize] ^ t[4][d[4] as usize] ^ t[11][d[11] as usize] ^ t[14][d[14] as usize] ^ key[1];
    let x3 = t[2][d[2] as usize] ^ t[5][d[5] as usize] ^ t[8][d[8] as usize] ^ t[15][d[15] as usize] ^ key[2];
    let x4 = t[3][d[3] as usize] ^ t[6][d[6] as usize] ^ t[9][d[9] as usize] ^ t[12][d[12] as usize] ^ key[3];
    pack([x1, x2, x3, x4])
}

/// Jenkins one-at-a-time hash over raw bytes (used to seed magic.dat decoding).
pub fn joaat_bytes(data: &[u8]) -> u32 {
    let mut h: u32 = 0;
    for &b in data {
        h = h.wrapping_add(b as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h = h.wrapping_add(h << 15);
    h
}

/// Jenkins one-at-a-time hash of a lowercased string (RAGE "joaat").
pub fn joaat(text: &str) -> u32 {
    joaat_bytes(text.to_ascii_lowercase().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes_fips197_vector() {
        // FIPS-197 C.3 AES-256 test vector.
        let key: [u8; 32] = core::array::from_fn(|i| i as u8);
        let mut block: Vec<u8> = (0..16u8).map(|i| i * 0x11).collect();
        aes_encrypt(&mut block, &key);
        assert_eq!(
            block,
            [0x8e, 0xa2, 0xb7, 0xca, 0x51, 0x67, 0x45, 0xbf, 0xea, 0xfc, 0x49, 0x90, 0x4b, 0x49, 0x60, 0x89]
        );
        aes_decrypt(&mut block, &key);
        assert_eq!(block, (0..16u8).map(|i| i * 0x11).collect::<Vec<_>>());
    }

    #[test]
    fn aes_leaves_tail() {
        let key = [7u8; 32];
        let mut d = vec![1u8; 20];
        aes_encrypt(&mut d, &key);
        assert_eq!(&d[16..], &[1, 1, 1, 1]);
    }

    #[test]
    fn joaat_known() {
        // Well-known RAGE hashes.
        assert_eq!(joaat("adder"), 0xB779A091);
        assert_eq!(joaat("WEAPON_PISTOL"), 0x1B06D571);
    }
}
