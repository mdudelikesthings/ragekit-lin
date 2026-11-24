//! Add-on vehicle packs: turn a folder of vehicle files into a GTA V DLC
//! (`dlc.rpf` with `setup2.xml`, `content.xml` and `x64/vehicles.rpf`) and an
//! `.oiv` package that installs it and registers it in `dlclist.xml`.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use crate::keys::Keys;
use crate::tree::{FileData, Format, RpfData, Tree};
use crate::{Error, Result};

/// A data file a vehicle pack can carry, by its XML root element.
fn meta_type(root: &str) -> Option<&'static str> {
    Some(match root {
        "CHandlingDataMgr" => "HANDLING_FILE",
        "CVehicleModelInfo__InitDataList" => "VEHICLE_METADATA_FILE",
        "CVehicleModelInfoVarGlobal" => "CARCOLS_FILE",
        "CVehicleModelInfoVariation" => "VEHICLE_VARIATION_FILE",
        "CVehicleMetadataMgr" => "VEHICLE_LAYOUTS_FILE",
        "CDataFileMgr__ContentsOfDataFileXml" => return None,
        _ => return None,
    })
}

fn root_element(data: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(data);
    let mut rest = s.as_ref();
    loop {
        let i = rest.find('<')?;
        rest = &rest[i + 1..];
        if rest.starts_with('?') || rest.starts_with('!') {
            continue;
        }
        let end = rest.find(|c: char| c.is_whitespace() || c == '>' || c == '/')?;
        return Some(rest[..end].to_string());
    }
}

/// What the wizard found in the input folder.
#[derive(Debug, Default)]
pub struct VehiclePack {
    pub name: String,
    /// (file name, data) streamed from x64/vehicles.rpf
    pub stream: Vec<(String, Vec<u8>)>,
    /// (file name, data, data file type)
    pub data: Vec<(String, Vec<u8>, &'static str)>,
    pub skipped: Vec<String>,
}

/// A valid DLC name: lowercase letters, digits and underscores.
pub fn clean_name(s: &str) -> String {
    let n: String = s.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    n.trim_matches('_').chars().take(32).collect()
}

/// Collect vehicle files from a folder (searched recursively).
pub fn scan(dir: &Path, name: &str) -> Result<VehiclePack> {
    let name = clean_name(name);
    if name.is_empty() {
        return Err(Error::Invalid("pack name is empty".into()));
    }
    let mut p = VehiclePack { name, ..Default::default() };
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d)?.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let fname = e.file_name().to_string_lossy().to_ascii_lowercase();
            let ext = fname.rsplit('.').next().unwrap_or("");
            match ext {
                "yft" | "ytd" | "ydr" | "ydd" | "ycd" | "ybn" => p.stream.push((fname, fs::read(&path)?)),
                "meta" | "xml" => {
                    let data = fs::read(&path)?;
                    match root_element(&data).as_deref().and_then(meta_type) {
                        Some(t) => p.data.push((fname, data, t)),
                        None => p.skipped.push(format!("{fname} (not a vehicle data file)")),
                    }
                }
                _ => p.skipped.push(fname),
            }
        }
    }
    p.stream.sort_by(|a, b| a.0.cmp(&b.0));
    p.data.sort_by(|a, b| a.0.cmp(&b.0));
    if !p.stream.iter().any(|(n, _)| n.ends_with(".yft")) {
        return Err(Error::Invalid("no .yft model files found".into()));
    }
    if !p.data.iter().any(|d| d.2 == "VEHICLE_METADATA_FILE") {
        return Err(Error::Invalid("no vehicles.meta found (needed to register the vehicle)".into()));
    }
    Ok(p)
}

fn setup2(name: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<SSetupData>
  <deviceName>dlc_{name}</deviceName>
  <datFile>content.xml</datFile>
  <timeStamp>01/01/2026 00:00:00</timeStamp>
  <nameHash>{name}</nameHash>
  <contentChangeSets />
  <contentChangeSetGroups>
    <Item>
      <NameHash>GROUP_STARTUP</NameHash>
      <ContentChangeSets>
        <Item>{upper}_AUTOGEN</Item>
      </ContentChangeSets>
    </Item>
  </contentChangeSetGroups>
  <startupScript />
  <scriptCallstackSize value="0" />
  <type>EXTRACONTENT_COMPAT_PACK</type>
  <order value="1" />
  <minorOrder value="0" />
  <isLevelPack value="false" />
  <dependencyPackHash />
  <requiredVersion />
  <subPackCount value="0" />
</SSetupData>
"#,
        upper = name.to_ascii_uppercase()
    )
}

fn content(p: &VehiclePack) -> String {
    let n = &p.name;
    let mut items = String::new();
    let mut enable = String::new();
    for (f, _, t) in &p.data {
        items.push_str(&format!(
            "    <Item>\n      <filename>dlc_{n}:/common/data/{f}</filename>\n      <fileType>{t}</fileType>\n      <overlay value=\"false\" />\n      <disabled value=\"true\" />\n      <persistent value=\"false\" />\n    </Item>\n"
        ));
        enable.push_str(&format!("        <Item>dlc_{n}:/common/data/{f}</Item>\n"));
    }
    items.push_str(&format!(
        "    <Item>\n      <filename>dlc_{n}:/%PLATFORM%/vehicles.rpf</filename>\n      <fileType>RPF_FILE</fileType>\n      <overlay value=\"false\" />\n      <disabled value=\"true\" />\n      <persistent value=\"true\" />\n    </Item>\n"
    ));
    enable.push_str(&format!("        <Item>dlc_{n}:/%PLATFORM%/vehicles.rpf</Item>\n"));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<CDataFileMgr__ContentsOfDataFileXml>
  <disabledFiles />
  <includedXmlFiles />
  <includedDataFiles />
  <dataFiles>
{items}  </dataFiles>
  <contentChangeSets>
    <Item>
      <changeSetName>{upper}_AUTOGEN</changeSetName>
      <filesToDisable />
      <filesToEnable>
{enable}      </filesToEnable>
      <txdToLoad />
      <txdToUnload />
      <residentResources />
      <unregisterResources />
    </Item>
  </contentChangeSets>
  <patchFiles />
</CDataFileMgr__ContentsOfDataFileXml>
"#,
        upper = n.to_ascii_uppercase()
    )
}

/// Build the pack's `dlc.rpf`.
pub fn build_dlc(p: &VehiclePack, keys: Arc<Keys>) -> Result<Vec<u8>> {
    let mut dlc = Tree::new(Format::Rpf7, keys.clone());
    dlc.insert_bytes("setup2.xml", setup2(&p.name).into_bytes())?;
    dlc.insert_bytes("content.xml", content(p).into_bytes())?;
    for (f, d, _) in &p.data {
        dlc.insert_bytes(&format!("common/data/{f}"), d.clone())?;
    }
    let mut veh = Tree::new(Format::Rpf7, keys);
    for (f, d) in &p.stream {
        veh.insert_bytes(f, d.clone())?;
    }
    dlc.insert("x64/vehicles.rpf", FileData::Rpf(RpfData::Tree(Box::new(veh))))?;
    dlc.to_bytes()
}

/// `assembly.xml` for a package that installs the pack and registers it.
pub fn assembly(p: &VehiclePack) -> String {
    let n = &p.name;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package version="2.1" id="{{ragekit-{n}}}" target="Five">
  <metadata>
    <name>{n} (add-on vehicle)</name>
    <version><major>1</major><minor>0</minor></version>
    <author><displayName>ragekit</displayName></author>
    <description><![CDATA[Add-on vehicle pack "{n}": {cars} model file(s), built by ragekit.]]></description>
  </metadata>
  <colors>
    <headerBackground useBlackTextColor="False">$FF2D5A8C</headerBackground>
    <iconBackground>$FF1E1E1E</iconBackground>
  </colors>
  <content>
    <add source="dlc.rpf">update\x64\dlcpacks\{n}\dlc.rpf</add>
    <archive path="update\update.rpf" createIfNotExist="False" type="RPF7">
      <xml path="common\data\dlclist.xml">
        <add xpath="/SMandatoryPacksData/Paths" append="Last"><Item>dlcpacks:/{n}/</Item></add>
      </xml>
    </archive>
  </content>
</package>
"#,
        cars = p.stream.iter().filter(|(f, _)| f.ends_with(".yft") && !f.ends_with("_hi.yft")).count()
    )
}

/// Write an unpacked package folder (assembly.xml + content/dlc.rpf).
pub fn write_package_dir(p: &VehiclePack, dir: &Path, keys: Arc<Keys>) -> Result<()> {
    fs::create_dir_all(dir.join("content"))?;
    fs::write(dir.join("assembly.xml"), assembly(p))?;
    fs::write(dir.join("content/dlc.rpf"), build_dlc(p, keys)?)?;
    Ok(())
}

/// Write a shareable `.oiv`.
pub fn write_oiv(p: &VehiclePack, out: &Path, keys: Arc<Keys>) -> Result<()> {
    let dlc = build_dlc(p, keys)?;
    crate::fsutil::write_atomic(out, |w| {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("assembly.xml", o).map_err(|e| Error::Invalid(e.to_string()))?;
            z.write_all(assembly(p).as_bytes())?;
            // Archives are already compressed per file; store them.
            let stored = o.compression_method(zip::CompressionMethod::Stored);
            z.start_file("content/dlc.rpf", stored).map_err(|e| Error::Invalid(e.to_string()))?;
            z.write_all(&dlc)?;
            z.finish().map_err(|e| Error::Invalid(e.to_string()))?;
        }
        let b = buf.into_inner();
        w.write_all(&b)?;
        Ok(b.len() as u64)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_roots() {
        assert_eq!(clean_name("My Car-2!"), "my_car_2");
        assert_eq!(root_element(b"<?xml version=\"1.0\"?>\n<!-- x -->\n<CHandlingDataMgr>").as_deref(), Some("CHandlingDataMgr"));
    }
}
