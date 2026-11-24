//! Real-file texture checks (set RAGEKIT_SAMPLES to a folder with .ytd files).

#[test]
fn real_ytd_if_present() {
    let Ok(dir) = std::env::var("RAGEKIT_SAMPLES") else { return };
    let mut n = 0;
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let data = std::fs::read(e.path()).unwrap();
        if !ragekit_core::textures::is_ytd(&data) {
            continue;
        }
        let d = ragekit_core::textures::load(&data).unwrap();
        assert!(!d.textures.is_empty());
        let mut bad = 0;
        for t in &d.textures {
            match ragekit_core::textures::rgba(t) {
                Ok(px) => assert_eq!(px.len(), t.width as usize * t.height as usize * 4),
                Err(err) => {
                    eprintln!("  can't decode {}: {err}", t.name);
                    bad += 1;
                }
            }
        }
        assert!(bad * 4 < d.textures.len(), "too many undecodable textures");
        if !d.enhanced {
            // Rebuild without changes: same textures, same pixels.
            let again = ragekit_core::textures::load(&d.save().unwrap()).unwrap();
            assert_eq!(again.textures.len(), d.textures.len());
            for t in &d.textures {
                let u = &again.textures[again.find(&t.name).unwrap()];
                assert_eq!((u.width, u.height, u.levels, u.format), (t.width, t.height, t.levels, t.format));
                assert_eq!(u.pixel_data, t.pixel_data, "{}", t.name);
            }
        }
        eprintln!("{}: {} textures ok", e.path().display(), d.textures.len());
        n += 1;
    }
    assert!(n > 0);
}
