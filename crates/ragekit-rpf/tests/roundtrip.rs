use std::io::Write;
use std::sync::Arc;

use rand::{Rng, RngCore, SeedableRng};
use ragekit_rpf::archive::EntryKind;
use ragekit_rpf::tree::RpfData;
use ragekit_rpf::{Archive, Blob, Encryption, FileData, Keys, Node, Tree};

fn deflate(d: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

/// A fake RSC7 resource: header + deflated body. sys flags: 8 KiB system page.
fn fake_resource(body: &[u8]) -> Vec<u8> {
    let sys = (1u32 << 17) | (2 << 28);
    let gfx = 0u32;
    let mut v = ragekit_rpf::resource::header(sys, gfx).to_vec();
    v.extend(deflate(body));
    v
}

fn sample_tree(keys: &Arc<Keys>) -> (Tree, Vec<u8>, Vec<u8>) {
    let mut rng = rand::rngs::StdRng::seed_from_u64(1);
    let mut big = vec![0u8; 3 << 20];
    rng.fill_bytes(&mut big);
    let res_body: Vec<u8> = (0..20000u32).flat_map(|i| (i % 251).to_le_bytes()).collect();

    let mut t = Tree::new(keys.clone());
    t.insert("common/data/handling.meta", FileData::from_bytes("handling.meta", b"<CHandlingDataMgr/>".to_vec()).unwrap()).unwrap();
    t.insert("x64/textures/adder.ytd", FileData::from_bytes("adder.ytd", fake_resource(&res_body)).unwrap()).unwrap();
    t.insert("x64/big.bin", FileData::from_bytes("big.bin", big.clone()).unwrap()).unwrap();
    t.insert("audio/sfx.awc", FileData::from_bytes("sfx.awc", vec![9u8; 1000]).unwrap()).unwrap();
    t.insert("empty.txt", FileData::from_bytes("empty.txt", Vec::new()).unwrap()).unwrap();
    // Nested archive created by path.
    t.insert("dlc.rpf/x64/vehicles.meta", FileData::from_bytes("vehicles.meta", b"<vehicles/>".to_vec()).unwrap()).unwrap();
    t.mkdir("emptydir").unwrap();
    (t, big, res_body)
}

#[test]
fn write_and_read_back() {
    let keys = Arc::new(Keys::default());
    let (t, big, res_body) = sample_tree(&keys);
    let bytes = t.to_bytes(Encryption::Open).unwrap();
    assert_eq!(bytes.len() % 512, 0);

    let a = Archive::parse(Blob::from_vec(bytes), "test.rpf", &keys).unwrap();
    assert_eq!(a.encryption, Encryption::Open);

    let i = a.find("COMMON\\data/Handling.meta").unwrap();
    assert_eq!(a.extract(i, &keys).unwrap(), b"<CHandlingDataMgr/>");

    let i = a.find("x64/big.bin").unwrap();
    assert_eq!(a.extract(i, &keys).unwrap(), big);

    let i = a.find("audio/sfx.awc").unwrap();
    assert!(matches!(a.entries[i].kind, EntryKind::Binary { size: 0, .. }), "awc stored uncompressed");
    assert_eq!(a.extract(i, &keys).unwrap(), vec![9u8; 1000]);

    let i = a.find("empty.txt").unwrap();
    assert!(a.extract(i, &keys).unwrap().is_empty());

    let i = a.find("x64/textures/adder.ytd").unwrap();
    assert!(matches!(a.entries[i].kind, EntryKind::Resource { .. }));
    assert_eq!(a.resource_decompressed(i, &keys).unwrap(), res_body);
    let exported = a.extract(i, &keys).unwrap();
    assert_eq!(&exported[..4], b"RSC7");
    assert_eq!(ragekit_rpf::resource::parse_header(&exported).unwrap().0, 0x20);

    // Nested archive through a path.
    let (inner, j) = a.resolve("dlc.rpf/x64/vehicles.meta", &keys).unwrap();
    assert_eq!(inner.extract(j, &keys).unwrap(), b"<vehicles/>");

    assert!(a.find("emptydir").is_some());

    // Children of each folder are contiguous and sorted (game requirement).
    for (d, e) in a.entries.iter().enumerate() {
        if e.is_dir() {
            let names: Vec<&str> = a.children(d).iter().map(|&c| a.entries[c].name.as_str()).collect();
            let mut sorted = names.clone();
            sorted.sort();
            assert_eq!(names, sorted);
        }
    }
}

#[test]
fn edit_existing_archive() {
    let keys = Arc::new(Keys::default());
    let (t, big, _) = sample_tree(&keys);
    let v1 = t.to_bytes(Encryption::Open).unwrap();
    let a1 = Archive::parse(Blob::from_vec(v1), "test.rpf", &keys).unwrap();

    let mut t2 = Tree::from_archive(a1.clone(), keys.clone()).unwrap();
    // Untouched nested archive stays raw.
    let raw_before = match &t2.root.children["dlc.rpf"].1 {
        Node::File(FileData::Rpf(RpfData::Raw(b))) => b.bytes().to_vec(),
        other => panic!("expected raw nested rpf, got {other:?}"),
    };
    t2.insert("common/data/handling.meta", FileData::from_bytes("handling.meta", b"<new/>".to_vec()).unwrap()).unwrap();
    t2.remove("audio/sfx.awc").unwrap();
    let v2 = t2.to_bytes(Encryption::Open).unwrap();
    let a2 = Archive::parse(Blob::from_vec(v2), "test.rpf", &keys).unwrap();
    assert_eq!(a2.extract(a2.find("common/data/handling.meta").unwrap(), &keys).unwrap(), b"<new/>");
    assert!(a2.find("audio/sfx.awc").is_none());
    assert_eq!(a2.extract(a2.find("x64/big.bin").unwrap(), &keys).unwrap(), big);
    let nested = a2.find("dlc.rpf").unwrap();
    assert_eq!(a2.nested_blob(nested).unwrap().bytes(), &raw_before[..]);

    // Edit inside the nested archive.
    let mut t3 = Tree::from_archive(a2, keys.clone()).unwrap();
    t3.insert("dlc.rpf/x64/carcols.meta", FileData::from_bytes("carcols.meta", b"<carcols/>".to_vec()).unwrap()).unwrap();
    let a3 = Archive::parse(Blob::from_vec(t3.to_bytes(Encryption::Open).unwrap()), "test.rpf", &keys).unwrap();
    let (inner, j) = a3.resolve("dlc.rpf/x64/carcols.meta", &keys).unwrap();
    assert_eq!(inner.extract(j, &keys).unwrap(), b"<carcols/>");
    let (inner, j) = a3.resolve("dlc.rpf/x64/vehicles.meta", &keys).unwrap();
    assert_eq!(inner.extract(j, &keys).unwrap(), b"<vehicles/>");
}

#[test]
fn aes_toc() {
    let mut k = Keys::default();
    k.aes = Some([0x42; 32]);
    let keys = Arc::new(k);
    let (t, _, _) = sample_tree(&keys);
    let bytes = t.to_bytes(Encryption::Aes).unwrap();
    let a = Archive::parse(Blob::from_vec(bytes.clone()), "t.rpf", &keys).unwrap();
    assert_eq!(a.encryption, Encryption::Aes);
    assert!(a.find("x64/textures/adder.ytd").is_some());
    let err = Archive::parse(Blob::from_vec(bytes), "t.rpf", &Keys::default()).unwrap_err();
    assert!(err.to_string().contains("missing key"), "{err}");
}

#[test]
fn ng_needs_keys() {
    let keys = Arc::new(Keys::default());
    let (t, _, _) = sample_tree(&keys);
    let mut bytes = t.to_bytes(Encryption::Open).unwrap();
    bytes[12..16].copy_from_slice(&Encryption::Ng.to_u32().to_le_bytes());
    let err = Archive::parse(Blob::from_vec(bytes), "t.rpf", &keys).unwrap_err();
    assert!(err.to_string().contains("missing key"), "{err}");
}

#[test]
fn ng_cipher_round_structure() {
    // With identity-like tables (byte value in the right lane) and zero keys,
    // round A maps each 4-byte group through tables; check determinism and
    // that a trailing partial block is untouched.
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    let mut tables: Box<ragekit_rpf::keys::NgTables> =
        vec![[[0u32; 256]; 16]; 17].into_boxed_slice().try_into().unwrap();
    for r in tables.iter_mut() {
        for p in r.iter_mut() {
            for w in p.iter_mut() {
                *w = rng.gen();
            }
        }
    }
    let key: [u32; 68] = core::array::from_fn(|_| rng.gen());
    let mut a = vec![3u8; 37];
    let mut b = a.clone();
    ragekit_rpf::crypto::ng_decrypt(&mut a, &key, &tables);
    ragekit_rpf::crypto::ng_decrypt(&mut b, &key, &tables);
    assert_eq!(a, b);
    assert_eq!(&a[32..], &[3u8; 5]);
    assert_ne!(&a[..16], &[3u8; 16]);
}

#[test]
fn hostile_input_does_not_panic() {
    let keys = Arc::new(Keys::default());
    let (t, _, _) = sample_tree(&keys);
    let good = t.to_bytes(Encryption::Open).unwrap();
    let mut rng = rand::rngs::StdRng::seed_from_u64(99);
    for n in [0usize, 4, 16, 17, 100, 600, 2000] {
        let _ = Archive::parse(Blob::from_vec(good[..n.min(good.len())].to_vec()), "t.rpf", &keys);
    }
    for _ in 0..2000 {
        let mut b = good[..4096.min(good.len())].to_vec();
        for _ in 0..rng.gen_range(1..8) {
            let i = rng.gen_range(4..b.len());
            b[i] = rng.gen();
        }
        if let Ok(a) = Archive::parse(Blob::from_vec(b), "t.rpf", &keys) {
            for i in a.walk() {
                let _ = a.path_of(i);
                if !a.entries[i].is_dir() {
                    let _ = a.extract(i, &keys);
                    if a.entries[i].is_rpf() {
                        let _ = a.open_nested(i, &keys);
                    }
                }
            }
        }
    }
}

#[test]
fn folder_round_trip() {
    let keys = Arc::new(Keys::default());
    let dir = std::env::temp_dir().join(format!("ragekit-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("x64/mymod.rpf/data")).unwrap();
    std::fs::write(dir.join("x64/readme.txt"), b"hello").unwrap();
    std::fs::write(dir.join("x64/mymod.rpf/data/a.meta"), b"<a/>").unwrap();
    std::fs::write(dir.join("x64/tex.ytd"), fake_resource(b"pixels")).unwrap();
    let t = Tree::from_dir(&dir, keys.clone()).unwrap();
    let a = Archive::parse(Blob::from_vec(t.to_bytes(Encryption::Open).unwrap()), "out.rpf", &keys).unwrap();
    let (inner, j) = a.resolve("x64/mymod.rpf/data/a.meta", &keys).unwrap();
    assert_eq!(inner.extract(j, &keys).unwrap(), b"<a/>");
    assert_eq!(a.resource_decompressed(a.find("x64/tex.ytd").unwrap(), &keys).unwrap(), b"pixels");
    std::fs::remove_dir_all(&dir).unwrap();
}
