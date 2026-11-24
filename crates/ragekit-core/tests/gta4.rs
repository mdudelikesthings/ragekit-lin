use std::sync::Arc;

use ragekit_core::crypto::iv_encrypt;
use ragekit_core::{vfs, Blob, Format, Keys, RowKind, Tree};

fn rsc5(rtype: u32, body: &[u8]) -> Vec<u8> {
    let mut v = 0x0543_5352u32.to_le_bytes().to_vec();
    v.extend(rtype.to_le_bytes());
    v.extend(0xC000_0123u32.to_le_bytes());
    v.extend_from_slice(body);
    v
}

fn iv_tree(format: Format, keys: &Arc<Keys>) -> Tree {
    let mut t = Tree::new(format, keys.clone());
    let dir = if format == Format::Img3 { "" } else { "common/data/" };
    t.insert_bytes(&format!("{dir}handling.dat"), b"; handling\nADMIRAL 1500.0".repeat(50)).unwrap();
    t.insert_bytes(&format!("{dir}admiral.wtd"), rsc5(8, &[7u8; 5000])).unwrap();
    t.insert_bytes(&format!("{dir}empty.txt"), Vec::new()).unwrap();
    t
}

fn check(tree: &Tree, format: Format) {
    let dir = if format == Format::Img3 { "" } else { "common/data/" };
    assert_eq!(tree.format, format);
    assert_eq!(tree.read(&format!("{dir}handling.dat")).unwrap(), b"; handling\nADMIRAL 1500.0".repeat(50));
    assert_eq!(tree.read(&format!("{dir}ADMIRAL.WTD")).unwrap(), rsc5(8, &[7u8; 5000]));
    assert!(tree.read(&format!("{dir}empty.txt")).unwrap().is_empty());
    let rows = tree.list(dir).unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(|r| r.name == "admiral.wtd" && r.type_label == "resource"));
}

#[test]
fn rpf2_round_trip() {
    let keys = Arc::new(Keys::default());
    let bytes = iv_tree(Format::Rpf2, &keys).to_bytes().unwrap();
    assert_eq!(&bytes[..4], b"RPF2");
    let t = vfs::open_blob(Blob::from_vec(bytes), "common.rpf", keys.clone()).unwrap();
    check(&t, Format::Rpf2);
    assert_eq!(t.kind("common").unwrap(), RowKind::Folder);

    // Edit and write again.
    let mut t2 = t.clone();
    t2.remove("common/data/empty.txt").unwrap();
    t2.rename("common/data/handling.dat", "handling2.dat").unwrap();
    let t3 = vfs::open_blob(Blob::from_vec(t2.to_bytes().unwrap()), "common.rpf", keys).unwrap();
    assert!(t3.read("common/data/handling2.dat").is_ok());
    assert!(t3.read("common/data/empty.txt").is_err());
}

#[test]
fn rpf2_encrypted_toc() {
    let mut k = Keys::default();
    k.iv_aes = Some([0x5A; 32]);
    let keys = Arc::new(k);
    let mut bytes = iv_tree(Format::Rpf2, &keys).to_bytes().unwrap();
    let toc_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    iv_encrypt(&mut bytes[0x800..0x800 + toc_size], &[0x5A; 32]);
    bytes[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    let t = vfs::open_blob(Blob::from_vec(bytes.clone()), "x.rpf", keys).unwrap();
    check(&t, Format::Rpf2);
    assert!(t.origin.contains("AES"));
    let err = vfs::open_blob(Blob::from_vec(bytes), "x.rpf", Arc::new(Keys::default())).unwrap_err();
    assert!(err.to_string().contains("missing key"));
}

#[test]
fn img3_round_trip_and_encrypted() {
    let mut k = Keys::default();
    k.iv_aes = Some([0x33; 32]);
    let keys = Arc::new(k);
    let bytes = iv_tree(Format::Img3, &keys).to_bytes().unwrap();
    let t = vfs::open_blob(Blob::from_vec(bytes.clone()), "vehicles.img", keys.clone()).unwrap();
    check(&t, Format::Img3);

    // Encrypt header + TOC like the shipped game files.
    let mut enc = bytes.clone();
    let toc_size = u32::from_le_bytes(enc[12..16].try_into().unwrap()) as usize;
    iv_encrypt(&mut enc[..0x14], &[0x33; 32]);
    iv_encrypt(&mut enc[0x14..0x14 + toc_size], &[0x33; 32]);
    let t = vfs::open_blob(Blob::from_vec(enc.clone()), "vehicles.img", keys).unwrap();
    check(&t, Format::Img3);
    let err = vfs::open_blob(Blob::from_vec(enc), "vehicles.img", Arc::new(Keys::default())).unwrap_err();
    assert!(err.to_string().contains("missing key"), "{err}");
}

#[test]
fn img_has_no_folders() {
    let keys = Arc::new(Keys::default());
    let mut t = Tree::new(Format::Img3, keys);
    assert!(t.insert_bytes("models/a.wdr", rsc5(0x6E, b"x")).is_err());
    assert!(t.mkdir("models").is_err());
}

#[test]
fn cross_game_files_rejected() {
    let keys = Arc::new(Keys::default());
    let mut t = Tree::new(Format::Rpf2, keys.clone());
    let rsc7 = ragekit_core::resource::header(1 << 17, 0).to_vec();
    assert!(t.insert_bytes("a.ytd", rsc7).is_err());
    let mut t = Tree::new(Format::Rpf7, keys);
    assert!(t.insert_bytes("a.wtd", rsc5(8, b"x")).is_err());
}

#[test]
fn rpf3_hashed_names_read_only() {
    // Build an RPF2, then relabel as RPF3 with hash-style name offsets.
    let keys = Arc::new(Keys::default());
    let mut t = Tree::new(Format::Rpf2, keys.clone());
    t.insert_bytes("sfx/ambience", vec![1u8; 3000]).unwrap();
    let mut b = t.to_bytes().unwrap();
    b[0..4].copy_from_slice(b"RPF3");
    let t = vfs::open_blob(Blob::from_vec(b), "a.rpf", keys).unwrap();
    assert_eq!(t.format, Format::Rpf3);
    let rows = t.list("").unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].name.starts_with("0x"));
    let mut t2 = t.clone();
    assert!(t2.insert_bytes("x", vec![1]).is_err());
    assert!(t.to_bytes().is_err());
}

#[test]
fn hostile_iv_input() {
    use rand::{Rng, SeedableRng};
    let keys = Arc::new(Keys::default());
    let good_rpf = iv_tree(Format::Rpf2, &keys).to_bytes().unwrap();
    let good_img = iv_tree(Format::Img3, &keys).to_bytes().unwrap();
    let mut rng = rand::rngs::StdRng::seed_from_u64(5);
    for good in [good_rpf, good_img] {
        for _ in 0..2000 {
            let mut b = good.clone();
            for _ in 0..rng.gen_range(1..6) {
                let i = rng.gen_range(4..0x900.min(b.len()));
                b[i] = rng.gen();
            }
            if let Ok(t) = vfs::open_blob(Blob::from_vec(b), "x", keys.clone()) {
                let mut paths = Vec::new();
                t.for_each_file(&mut |p, _| paths.push(p.to_string()));
                for p in paths {
                    let _ = t.read(&p);
                }
            }
        }
    }
}
