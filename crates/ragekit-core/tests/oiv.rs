use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ragekit_core::oiv::{self, Options, Package};
use ragekit_core::{vfs, Format, Keys, Tree};

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ragekit-oiv-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn keys() -> Arc<Keys> {
    Arc::new(Keys::default())
}

const DLCLIST: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<SMandatoryPacksData>\n  <Paths>\n    <Item>platform:/dlcPacks/mpBeach/</Item>\n    <Item>platform:/dlcPacks/mpBusiness/</Item>\n  </Paths>\n</SMandatoryPacksData>\n";

/// A fake GTA V install: GTA5.exe + update/update.rpf with dlclist.xml.
fn gta5_folder(name: &str) -> PathBuf {
    let g = tmpdir(name);
    fs::write(g.join("GTA5.exe"), b"MZ").unwrap();
    let mut t = Tree::new(Format::Rpf7, keys());
    t.insert_bytes("common/data/dlclist.xml", DLCLIST.as_bytes().to_vec()).unwrap();
    t.insert_bytes("common/data/gameconfig.txt", b"pool 100\r\nend\r\n".to_vec()).unwrap();
    t.insert_bytes("common/data/old.meta", b"<old/>".to_vec()).unwrap();
    fs::create_dir_all(g.join("update")).unwrap();
    fs::write(g.join("update/update.rpf"), t.to_bytes().unwrap()).unwrap();
    g
}

fn build_oiv(path: &Path, assembly: &str, files: &[(&str, &[u8])]) {
    let f = fs::File::create(path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default();
    z.start_file("assembly.xml", opts).unwrap();
    z.write_all(assembly.as_bytes()).unwrap();
    for (name, data) in files {
        z.start_file(format!("content/{name}"), opts).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap();
}

const CAR_ASM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<package version="2.1" id="{CAR}" target="Five">
  <metadata><name>My Car</name><version><major>1</major><minor>0</minor></version>
    <author><displayName>Tester</displayName></author><description>Adds a car</description></metadata>
  <content>
    <archive path="Update\Update.rpf" createIfNotExist="True" type="RPF7">
      <archive path="x64\dlcpacks\mycar\dlc.rpf" createIfNotExist="True" type="RPF7">
        <add source="mycar\mycar.yft">x64\vehicles.rpf\mycar.yft</add>
        <add source="mycar\vehicles.meta">common\data\levels\gta5\vehicles.meta</add>
      </archive>
      <xml path="common\data\dlclist.xml">
        <add xpath="/SMandatoryPacksData/Paths" append="Last"><Item>dlcpacks:/mycar/</Item></add>
      </xml>
      <text path="common\data\gameconfig.txt">
        <replace line="pool 100" condition="Equal">pool 200</replace>
        <insert where="Before" line="end">extra 1</insert>
      </text>
      <delete>common\data\old.meta</delete>
    </archive>
    <add source="readme.txt">mycar_readme.txt</add>
  </content>
</package>"#;

fn rsc7(body: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(body).unwrap();
    let mut v = ragekit_core::resource::header(1 << 17, 1 << 17).to_vec();
    v.extend(e.finish().unwrap());
    v
}

fn car_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![("mycar/mycar.yft", rsc7(b"model data")), ("mycar/vehicles.meta", b"<CVehicleModelInfo__InitDataList/>".to_vec()), ("readme.txt", b"hi".to_vec())]
}

fn build_car(dir: &Path) -> PathBuf {
    let p = dir.join("mycar.oiv");
    let files = car_files();
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (*n, d.as_slice())).collect();
    build_oiv(&p, CAR_ASM, &refs);
    p
}

#[test]
fn gta5_install_into_mods_and_uninstall() {
    let g = gta5_folder("v");
    let game_rpf = fs::read(g.join("update/update.rpf")).unwrap();
    let pkg = Package::open(&build_car(&g)).unwrap();
    assert_eq!(pkg.meta.name, "My Car");

    let plan = oiv::plan(&pkg, &Options { game_dir: g.clone(), use_mods: true }, keys());
    for l in &plan.log {
        eprintln!("{:?} {}", l.level, l.msg);
    }
    assert_eq!(plan.errors(), 0);
    assert_eq!(plan.warnings(), 0);
    let rec = plan.commit(&pkg, false, &mut |_| {}).unwrap();
    assert_eq!(rec.files.len(), 2);

    // The game's archive is untouched; the mods copy has the changes.
    assert_eq!(fs::read(g.join("update/update.rpf")).unwrap(), game_rpf);
    let t = vfs::open(&g.join("mods/update/update.rpf"), keys()).unwrap();
    let list = String::from_utf8(t.read("common/data/dlclist.xml").unwrap()).unwrap();
    assert!(list.contains("dlcpacks:/mycar/"));
    assert!(list.contains("mpBusiness"));
    assert_eq!(t.read("common/data/gameconfig.txt").unwrap(), b"pool 200\r\nextra 1\r\nend\r\n");
    assert!(t.read("common/data/old.meta").is_err());
    assert_eq!(t.read("x64/dlcpacks/mycar/dlc.rpf/x64/vehicles.rpf/mycar.yft").unwrap(), rsc7(b"model data"));
    assert!(t.read("x64/dlcpacks/mycar/dlc.rpf/common/data/levels/gta5/vehicles.meta").is_ok());
    assert_eq!(fs::read(g.join("mycar_readme.txt")).unwrap(), b"hi");

    // Installing again is harmless (XML/text edits don't duplicate).
    let plan2 = oiv::plan(&pkg, &Options { game_dir: g.clone(), use_mods: true }, keys());
    assert_eq!(plan2.errors(), 0, "{:?}", plan2.log.iter().map(|l| &l.msg).collect::<Vec<_>>());
    let rec2 = plan2.commit(&pkg, false, &mut |_| {}).unwrap();
    let t = vfs::open(&g.join("mods/update/update.rpf"), keys()).unwrap();
    let list = String::from_utf8(t.read("common/data/dlclist.xml").unwrap()).unwrap();
    assert_eq!(list.matches("mycar").count(), 1, "{list}");

    // The first install can't be undone while the second's changes sit on top.
    assert_eq!(oiv::list(&g).len(), 2);
    assert!(oiv::uninstall(&g, &rec.id, false).is_err());
    oiv::uninstall(&g, &rec2.id, false).unwrap();
    oiv::uninstall(&g, &rec.id, false).unwrap();
    assert!(!g.join("mods/update/update.rpf").exists());
    assert!(!g.join("mycar_readme.txt").exists());
    assert_eq!(fs::read(g.join("update/update.rpf")).unwrap(), game_rpf);
    assert!(oiv::list(&g).is_empty());
    fs::remove_dir_all(&g).unwrap();
}

#[test]
fn errors_block_install() {
    let g = gta5_folder("err");
    let asm = CAR_ASM.replace("readme.txt\"", "missing.txt\"").replace("<xml path=\"common\\data\\dlclist.xml\">", "<pso path=\"x.ymt\"><remove xpath=\"/a\"/></pso><xml path=\"common\\data\\dlclist.xml\">");
    let p = g.join("bad.oiv");
    let files = car_files();
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (*n, d.as_slice())).collect();
    build_oiv(&p, &asm, &refs);
    let pkg = Package::open(&p).unwrap();
    let plan = oiv::plan(&pkg, &Options { game_dir: g.clone(), use_mods: true }, keys());
    assert_eq!(plan.errors(), 2, "{:?}", plan.log.iter().map(|l| &l.msg).collect::<Vec<_>>());
    assert!(plan.commit(&pkg, false, &mut |_| {}).is_err());
    assert!(!g.join("mods").exists() || !g.join("mods/update/update.rpf").exists());
    assert!(oiv::list(&g).is_empty());
    fs::remove_dir_all(&g).unwrap();
}

#[test]
fn wrong_game_is_refused() {
    let g = gta5_folder("wrong");
    let asm = CAR_ASM.replace("target=\"Five\"", "target=\"IV\"");
    let p = g.join("iv.oiv");
    build_oiv(&p, &asm, &[]);
    let pkg = Package::open(&p).unwrap();
    let plan = oiv::plan(&pkg, &Options { game_dir: g.clone(), use_mods: true }, keys());
    assert!(plan.errors() > 0);
    // Enhanced-only package on a Legacy game.
    let asm = CAR_ASM.replace("<content>", "<gameversion>enhanced</gameversion><content>");
    build_oiv(&p, &asm, &[]);
    let plan = oiv::plan(&Package::open(&p).unwrap(), &Options { game_dir: g.clone(), use_mods: true }, keys());
    assert!(plan.log.iter().any(|l| l.msg.contains("Enhanced")));
    fs::remove_dir_all(&g).unwrap();
}

#[test]
fn gta4_install_in_place_and_uninstall() {
    let g = tmpdir("iv");
    fs::write(g.join("GTAIV.exe"), b"MZ").unwrap();
    fs::create_dir_all(g.join("pc/models/cdimages")).unwrap();
    fs::create_dir_all(g.join("common/data")).unwrap();
    let mut img = Tree::new(Format::Img3, keys());
    img.insert_bytes("admiral.wdr", b"\x52\x53\x43\x05\x6e\0\0\0\x23\x01\0\xc0old".to_vec()).unwrap();
    fs::write(g.join("pc/models/cdimages/vehicles.img"), img.to_bytes().unwrap()).unwrap();
    fs::write(g.join("common/data/handling.dat"), b"ADMIRAL 1500\nBANSHEE 1200\n").unwrap();
    let before_img = fs::read(g.join("pc/models/cdimages/vehicles.img")).unwrap();

    let asm = r#"<package version="2.0" id="{IV}" target="IV">
      <metadata><name>IV Admiral</name></metadata>
      <content>
        <archive path="pc\models\cdimages\vehicles.img" createIfNotExist="False" type="IMG3">
          <add source="admiral.wdr">admiral.wdr</add>
          <add source="admiral.wtd">admiral.wtd</add>
        </archive>
        <text path="common\data\handling.dat">
          <replace line="ADMIRAL*" condition="Mask">ADMIRAL 1800</replace>
        </text>
      </content>
    </package>"#;
    let p = g.join("iv.oiv");
    let wdr = b"\x52\x53\x43\x05\x6e\0\0\0\x23\x01\0\xc0new".to_vec();
    let wtd = b"\x52\x53\x43\x05\x08\0\0\0\x23\x01\0\xc0tex".to_vec();
    build_oiv(&p, asm, &[("admiral.wdr", &wdr), ("admiral.wtd", &wtd)]);
    let pkg = Package::open(&p).unwrap();
    let plan = oiv::plan(&pkg, &Options { game_dir: g.clone(), use_mods: true }, keys());
    assert_eq!(plan.errors(), 0, "{:?}", plan.log.iter().map(|l| &l.msg).collect::<Vec<_>>());
    assert!(!plan.use_mods); // GTA IV has no mods folder
    let rec = plan.commit(&pkg, false, &mut |_| {}).unwrap();

    let t = vfs::open(&g.join("pc/models/cdimages/vehicles.img"), keys()).unwrap();
    assert_eq!(t.read("admiral.wdr").unwrap(), wdr);
    assert_eq!(t.read("admiral.wtd").unwrap(), wtd);
    assert_eq!(fs::read(g.join("common/data/handling.dat")).unwrap(), b"ADMIRAL 1800\nBANSHEE 1200\n");

    oiv::uninstall(&g, &rec.id, false).unwrap();
    assert_eq!(fs::read(g.join("pc/models/cdimages/vehicles.img")).unwrap(), before_img);
    assert_eq!(fs::read(g.join("common/data/handling.dat")).unwrap(), b"ADMIRAL 1500\nBANSHEE 1200\n");
    fs::remove_dir_all(&g).unwrap();
}
