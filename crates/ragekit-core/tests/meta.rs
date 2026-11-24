//! Binary metadata editing through .oiv packages.
//!
//! Set RAGEKIT_SAMPLES to a folder of real .ymf/.ymt/.ytyp/.ymap files to
//! also round-trip those.

use std::fs;
use std::io::Write;
use std::sync::Arc;

use ragekit_core::oiv::{self, Options, Package};
use ragekit_core::{meta, vfs, Format, Keys, Tree};

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CPackFileMetaData>
  <MapDataGroups itemType="CMapDataGroup"/>
  <HDTxdBindingArray itemType="CHDTxdAssetBinding"/>
  <imapDependencies itemType="CImapDependency"/>
  <imapDependencies_2 itemType="CImapDependencies">
    <Item>
      <imapName>my_interior</imapName>
      <manifestFlags/>
      <itypDepArray itemType="Hash">
        <Item>my_props</Item>
      </itypDepArray>
    </Item>
  </imapDependencies_2>
  <itypDependencies_2 itemType="CItypDependencies"/>
  <Interiors itemType="CInteriorBoundsFiles"/>
</CPackFileMetaData>
"#;

#[test]
fn pso_round_trip_and_names() {
    let pso = meta::pso_from_xml_builtin(MANIFEST).unwrap();
    assert_eq!(meta::detect(&pso), Some(meta::Kind::Pso));
    meta::check_round_trip(&pso).unwrap();
    let (x, _) = meta::to_xml(&pso, &["my_interior".into(), "my_props".into()]).unwrap();
    assert!(x.contains("<imapName>my_interior</imapName>"), "{x}");
    let (x2, _) = meta::to_xml(&pso, &[]).unwrap();
    assert!(x2.contains("hash_"), "{x2}");
}

#[test]
fn oiv_pso_edit() {
    let g = std::env::temp_dir().join(format!("ragekit-meta-{}", std::process::id()));
    let _ = fs::remove_dir_all(&g);
    fs::create_dir_all(g.join("update")).unwrap();
    fs::write(g.join("GTA5.exe"), b"MZ").unwrap();
    let keys = Arc::new(Keys::default());
    let mut t = Tree::new(Format::Rpf7, keys.clone());
    t.insert_bytes("common/data/_manifest.ymf", meta::pso_from_xml_builtin(MANIFEST).unwrap()).unwrap();
    fs::write(g.join("update/update.rpf"), t.to_bytes().unwrap()).unwrap();

    let asm = r#"<package version="2.2" id="{M}" target="Five"><metadata><name>Manifest edit</name></metadata><content>
      <archive path="update\update.rpf" createIfNotExist="False" type="RPF7">
        <pso path="common\data\_manifest.ymf">
          <add xpath="/CPackFileMetaData/imapDependencies_2/Item[imapName='my_interior']/itypDepArray" append="Last"><Item>extra_props</Item></add>
          <add xpath="/CPackFileMetaData/imapDependencies_2" append="Last">
            <Item><imapName>second_map</imapName><manifestFlags/><itypDepArray itemType="Hash"><Item>my_props</Item></itypDepArray></Item>
          </add>
        </pso>
      </archive></content></package>"#;
    let p = g.join("m.oiv");
    let mut z = zip::ZipWriter::new(fs::File::create(&p).unwrap());
    z.start_file("assembly.xml", zip::write::SimpleFileOptions::default()).unwrap();
    z.write_all(asm.as_bytes()).unwrap();
    z.finish().unwrap();

    let pkg = Package::open(&p).unwrap();
    let plan = oiv::plan(&pkg, &Options { game_dir: g.clone(), use_mods: true }, keys.clone());
    let msgs: Vec<&str> = plan.log.iter().map(|l| l.msg.as_str()).collect();
    assert_eq!(plan.errors(), 0, "{msgs:#?}");
    assert_eq!(plan.warnings(), 0, "{msgs:#?}");
    plan.commit(&pkg, false, &mut |_| {}).unwrap();

    let t = vfs::open(&g.join("mods/update/update.rpf"), keys).unwrap();
    let data = t.read("common/data/_manifest.ymf").unwrap();
    assert_eq!(meta::detect(&data), Some(meta::Kind::Pso), "still binary PSO");
    let names: Vec<String> = ["my_interior", "my_props", "extra_props", "second_map"].iter().map(|s| s.to_string()).collect();
    let (x, _) = meta::to_xml(&data, &names).unwrap();
    assert!(x.contains("<Item>extra_props</Item>"), "{x}");
    assert!(x.contains("<imapName>second_map</imapName>"), "{x}");
    fs::remove_dir_all(&g).unwrap();
}

#[test]
fn real_samples_if_present() {
    let Ok(dir) = std::env::var("RAGEKIT_SAMPLES") else { return };
    let mut n = 0;
    for e in fs::read_dir(dir).unwrap().flatten() {
        let data = fs::read(e.path()).unwrap();
        if meta::detect(&data).is_none() {
            continue;
        }
        meta::check_round_trip(&data).unwrap_or_else(|err| panic!("{}: {err}", e.path().display()));
        // Through the installer's XML editor (parse + re-serialise) too.
        let (x, _) = meta::to_xml(&data, &[]).unwrap();
        let doc = ragekit_core::oiv::xml::XmlDoc::parse(x.as_bytes()).unwrap();
        let rebuilt = meta::from_xml(&String::from_utf8(doc.to_bytes().unwrap()).unwrap(), &data).unwrap();
        let (x2, _) = meta::to_xml(&rebuilt, &[]).unwrap();
        assert_eq!(x, x2, "{}", e.path().display());
        n += 1;
    }
    eprintln!("round-tripped {n} real files");
    assert!(n > 0);
}
