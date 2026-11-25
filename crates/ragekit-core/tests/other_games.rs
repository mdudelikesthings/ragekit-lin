//! Max Payne 3 (RPF4) and Red Dead Redemption (RPF6) archives, and finding
//! their keys by probing.

use std::sync::Arc;

use ragekit_core::crypto::iv_encrypt;
use ragekit_core::vfs::{self, KeyFor};
use ragekit_core::{Blob, Format, Keys, RowKind, Tree};

fn rsc6(rtype: u32, body: &[u8]) -> Vec<u8> {
    let mut v = b"RSC\x06".to_vec();
    v.extend(rtype.to_le_bytes());
    v.extend(0xC000_0123u32.to_le_bytes());
    v.extend_from_slice(body);
    v
}

fn mp3_tree(keys: &Arc<Keys>) -> Tree {
    let mut t = Tree::new(Format::Rpf4, keys.clone());
    t.insert_bytes("data/weapons.xml", b"<weapons/>".repeat(400)).unwrap();
    t.insert_bytes("models/max.wft", rsc6(0x6A, &[3u8; 7000])).unwrap();
    t.insert_bytes("readme.txt", b"hi".to_vec()).unwrap();
    t
}

/// A fake executable with `key` buried at an odd offset among noise.
fn fake_exe(key: &[u8; 32], at: usize) -> Vec<u8> {
    let mut x = 0x1234_5678u32;
    let mut exe: Vec<u8> = (0..200_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect();
    exe[at..at + 32].copy_from_slice(key);
    exe
}

#[test]
fn rpf4_round_trip() {
    let keys = Arc::new(Keys::default());
    let bytes = mp3_tree(&keys).to_bytes().unwrap();
    assert_eq!(&bytes[..4], b"RPF4");
    let t = vfs::open_blob(Blob::from_vec(bytes), "data.rpf", keys.clone()).unwrap();
    assert_eq!(t.format, Format::Rpf4);
    assert_eq!(t.format.game(), "Max Payne 3");
    assert_eq!(t.read("data/weapons.xml").unwrap(), b"<weapons/>".repeat(400));
    assert_eq!(t.read("MODELS/MAX.WFT").unwrap(), rsc6(0x6A, &[3u8; 7000]));
    assert_eq!(t.kind("models").unwrap(), RowKind::Folder);
    assert!(t.list("models").unwrap().iter().any(|r| r.type_label == "resource"));

    let mut t2 = t.clone();
    t2.rename("readme.txt", "readme2.txt").unwrap();
    t2.insert_bytes("data/new.dat", vec![9; 3000]).unwrap();
    let t3 = vfs::open_blob(Blob::from_vec(t2.to_bytes().unwrap()), "data.rpf", keys).unwrap();
    assert_eq!(t3.read("readme2.txt").unwrap(), b"hi");
    assert_eq!(t3.read("data/new.dat").unwrap(), vec![9; 3000]);
}

#[test]
fn rpf4_encrypted_toc_and_key_probe() {
    let key = [0x3Cu8; 32].map(|b| b ^ 0x5A).iter().enumerate().map(|(i, b)| b.wrapping_add(i as u8 * 37)).collect::<Vec<_>>();
    let key: [u8; 32] = key.try_into().unwrap();
    let mut bytes = mp3_tree(&Arc::new(Keys::default())).to_bytes().unwrap();
    let toc_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    iv_encrypt(&mut bytes[0x800..0x800 + toc_size], &key);
    bytes[16..20].copy_from_slice(&u32::MAX.to_le_bytes());

    // Without the key: a clear error naming the game.
    let err = vfs::open_blob(Blob::from_vec(bytes.clone()), "data.rpf", Arc::new(Keys::default())).unwrap_err();
    assert!(err.to_string().contains("Max Payne 3"), "{err}");

    // Find the key in a fake executable using the archive as the probe.
    let (which, probe) = vfs::key_probe(&bytes[..0x810]).unwrap();
    assert_eq!(which, KeyFor::MaxPayne3);
    assert!(!probe(&[0u8; 32]));
    let found = Keys::find_by_probe(&fake_exe(&key, 123_457), &*probe).expect("key found");
    assert_eq!(found, key);

    let mut k = Keys::default();
    *which.slot(&mut k) = Some(found);
    let t = vfs::open_blob(Blob::from_vec(bytes), "data.rpf", Arc::new(k)).unwrap();
    assert_eq!(t.read("data/weapons.xml").unwrap(), b"<weapons/>".repeat(400));
}

#[test]
fn key_files_round_trip() {
    let dir = std::env::temp_dir().join(format!("ragekit-keys-{}", std::process::id()));
    let mut k = Keys::default();
    k.mp3_aes = Some([1; 32]);
    k.rdr_aes = Some([2; 32]);
    k.save_dir(&dir).unwrap();
    let back = Keys::load_dir(&dir).unwrap();
    assert_eq!(back.mp3_aes, Some([1; 32]));
    assert_eq!(back.rdr_aes, Some([2; 32]));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Build an RPF6 archive by hand: root -> [a.bin, sub/ -> [b.rsc]].
fn rpf6(key: Option<&[u8; 32]>, with_names: bool) -> Vec<u8> {
    let be = |v: u32| v.to_be_bytes();
    let a = b"hello red dead".to_vec();
    let b = vec![0xABu8; 300];
    let count = 4u32;
    let mut out = vec![0u8; 0x800];
    out[..4].copy_from_slice(b"RPF6");
    out[4..8].copy_from_slice(&be(count));
    out[12..16].copy_from_slice(&be(key.is_some() as u32));
    let a_off = 0x800u32;
    let b_off = 0x1000u32;
    let mut toc = Vec::new();
    for e in [
        [0u32, 0, 0x8000_0001, 2, 0],                   // root: children 1..3
        [0x1111_1111, a.len() as u32, a_off / 8, 0, 0], // a.bin
        [0x2222_2222, 0, 0x8000_0003, 1, 0],            // sub/: child 3
        [0x3333_3333, b.len() as u32, (b_off / 8) | 0x02, 0x8000_0042, 0], // b.rsc
    ] {
        for w in e {
            toc.extend(be(w));
        }
    }
    toc.resize(toc.len().div_ceil(16) * 16, 0);
    if let Some(k) = key {
        iv_encrypt(&mut toc, k);
    }
    out[16..16 + toc.len()].copy_from_slice(&toc);
    out.extend(&a);
    out.resize(b_off as usize, 0);
    out.extend(&b);
    if with_names {
        out.resize(out.len().div_ceil(8) * 8, 0);
        let debug = out.len() as u32;
        out[8..12].copy_from_slice(&be(debug / 8));
        let names = ["", "a.bin", "sub", "b.rsc"];
        let mut strs = Vec::new();
        for n in names {
            out.extend(be(strs.len() as u32));
            out.extend(be(0));
            strs.extend(n.as_bytes());
            strs.push(0);
        }
        out.extend(strs);
    }
    out
}

#[test]
fn rpf6_reads_hashed_and_named() {
    let keys = Arc::new(Keys::default());
    let t = vfs::open_blob(Blob::from_vec(rpf6(None, false)), "x.rpf", keys.clone()).unwrap();
    assert_eq!(t.format, Format::Rpf6);
    assert_eq!(t.read("0x11111111").unwrap(), b"hello red dead");
    assert_eq!(t.read("0x22222222/0x33333333").unwrap(), vec![0xAB; 300]);
    assert!(t.origin.contains("hashed"));
    assert!(t.to_bytes().is_err(), "RPF6 is read-only");

    let t = vfs::open_blob(Blob::from_vec(rpf6(None, true)), "x.rpf", keys).unwrap();
    assert_eq!(t.read("a.bin").unwrap(), b"hello red dead");
    assert_eq!(t.read("sub/b.rsc").unwrap(), vec![0xAB; 300]);
}

#[test]
fn rpf6_encrypted_and_probe() {
    let key: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(71).wrapping_add(13));
    let bytes = rpf6(Some(&key), true);
    assert!(vfs::open_blob(Blob::from_vec(bytes.clone()), "x.rpf", Arc::new(Keys::default())).is_err());
    let (which, probe) = vfs::key_probe(&bytes[..0x810]).unwrap();
    assert_eq!(which, KeyFor::Rdr);
    assert_eq!(Keys::find_by_probe(&fake_exe(&key, 4096), &*probe), Some(key));
    let mut k = Keys::default();
    k.rdr_aes = Some(key);
    let t = vfs::open_blob(Blob::from_vec(bytes), "x.rpf", Arc::new(k)).unwrap();
    assert_eq!(t.read("sub/b.rsc").unwrap(), vec![0xAB; 300]);
}

#[test]
fn hostile_rpf4_and_rpf6() {
    let keys = Arc::new(Keys::default());
    let good = mp3_tree(&keys).to_bytes().unwrap();
    let six = rpf6(None, true);
    for base in [good, six] {
        for i in (0..base.len().min(0x900)).step_by(3) {
            let mut b = base.clone();
            b[i] ^= 0xFF;
            // Must not panic; errors are fine.
            if let Ok(t) = vfs::open_blob(Blob::from_vec(b), "x.rpf", keys.clone()) {
                let _ = t.counts();
            }
        }
        for len in [4, 16, 100, 0x800, 0x810] {
            let _ = vfs::open_blob(Blob::from_vec(base[..len.min(base.len())].to_vec()), "x.rpf", keys.clone());
        }
    }
}

#[test]
fn max_payne_3_game_folder() {
    let dir = std::env::temp_dir().join(format!("ragekit-mp3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("MaxPayne3.exe"), b"MZ").unwrap();
    let (g, _) = vfs::detect_game(&dir).unwrap();
    assert_eq!(g, vfs::Game::MaxPayne3);
    assert_eq!(g.format(), Format::Rpf4);
    assert!(!g.takes_oiv());
    std::fs::remove_dir_all(&dir).unwrap();
}
