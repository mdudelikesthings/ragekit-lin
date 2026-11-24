//! Reading `.oiv` packages (OpenIV package format 2.x).
//!
//! A package is a ZIP file (or an unpacked folder) holding `assembly.xml`,
//! an optional 128x128 `icon.png`, and a `content` folder with the files
//! that `assembly.xml` refers to.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use xmltree::{Element, ParserConfig, XMLNode};

use crate::tree::Format;
use crate::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub format_version: String,
    pub id: String,
    /// "Five" for GTA V; GTA IV packages use other targets.
    pub target: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub author_links: Vec<(String, String)>,
    pub description: String,
    pub large_description: String,
    pub license: String,
    /// `legacy` / `enhanced` (non-standard tag used by some installers).
    pub game_version: Option<String>,
    /// ARGB colors from `<colors>`.
    pub header_color: Option<u32>,
    pub header_black_text: bool,
    pub icon_color: Option<u32>,
}

impl Metadata {
    /// Which game the package is for, from its `target`.
    pub fn is_gta5(&self) -> bool {
        let t = self.target.to_ascii_lowercase();
        t.is_empty() || t == "five" || t == "v" || t == "gta5" || t == "gtav"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    Equal,
    /// Wildcards `*` and `?`.
    Mask,
    StartWith,
    EndWith,
    Contains,
}

impl Condition {
    fn parse(s: Option<&str>, default: Condition) -> Condition {
        match s.map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("equal") | Some("equals") => Condition::Equal,
            Some("mask") => Condition::Mask,
            Some("startwith") | Some("startswith") => Condition::StartWith,
            Some("endwith") | Some("endswith") => Condition::EndWith,
            Some("contains") => Condition::Contains,
            _ => default,
        }
    }
}

#[derive(Debug, Clone)]
pub enum TextEdit {
    Insert { after: bool, line: String, condition: Condition, content: String },
    Replace { line: String, condition: Condition, content: String },
    /// Append at the end of the file.
    Add { content: String },
    Delete { condition: Condition, line: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddMode {
    First,
    Last,
    Before,
    After,
}

#[derive(Debug, Clone)]
pub enum XmlEdit {
    Add { xpath: String, mode: AddMode, nodes: Vec<XMLNode> },
    Replace { xpath: String, nodes: Vec<XMLNode> },
    Remove { xpath: String },
}

#[derive(Debug, Clone)]
pub enum Op {
    /// Work inside an archive (top level: path from the game folder;
    /// nested: path inside the parent archive).
    Archive { path: String, create: bool, format: Format, children: Vec<Op> },
    /// Copy `source` (relative to `content/`) to `dest`.
    Add { source: String, dest: String },
    Delete { target: String },
    Text { path: String, create: bool, edits: Vec<TextEdit> },
    /// XML edit; `pso` marks binary metadata edits.
    Xml { path: String, pso: bool, edits: Vec<XmlEdit> },
    Defragmentation,
}

impl Op {
    pub fn count(ops: &[Op]) -> usize {
        ops.iter()
            .map(|o| match o {
                Op::Archive { children, .. } => Op::count(children),
                _ => 1,
            })
            .sum()
    }
}

enum Content {
    Zip { zip: Mutex<zip::ZipArchive<File>>, index: HashMap<String, usize>, base: String },
    Dir(PathBuf),
}

pub struct Package {
    pub path: PathBuf,
    pub meta: Metadata,
    pub icon: Option<Vec<u8>>,
    pub ops: Vec<Op>,
    content: Content,
}

impl std::fmt::Debug for Package {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Package").field("path", &self.path).field("meta", &self.meta).field("ops", &self.ops.len()).finish()
    }
}

/// Normalise a package-relative path: forward slashes, lowercase, no
/// leading slash or `./`.
pub fn norm(p: &str) -> String {
    crate::normalize_path(p)
}

fn local(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_ascii_lowercase()
}

fn child<'a>(e: &'a Element, name: &str) -> Option<&'a Element> {
    let name = name.to_ascii_lowercase();
    e.children.iter().filter_map(|c| c.as_element()).find(|c| local(&c.name) == name)
}

fn text_of(e: &Element) -> String {
    let mut s = String::new();
    for c in &e.children {
        match c {
            XMLNode::Text(t) | XMLNode::CData(t) => s.push_str(t),
            XMLNode::Element(sub) => s.push_str(&text_of(sub)),
            _ => {}
        }
    }
    s
}

fn child_text(e: &Element, name: &str) -> String {
    child(e, name).map(|c| text_of(c).trim().to_string()).unwrap_or_default()
}

fn attr<'a>(e: &'a Element, name: &str) -> Option<&'a str> {
    e.attributes.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

fn attr_bool(e: &Element, name: &str) -> bool {
    attr(e, name).is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1")
}

/// Parse "$AARRGGBB" / "#AARRGGBB" / "#RRGGBB".
fn parse_color(s: &str) -> Option<u32> {
    let h = s.trim().trim_start_matches(['$', '#']);
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(if h.len() <= 6 { 0xFF00_0000 | v } else { v })
}

/// Text content of a text-edit element: drop the blank lines around it but
/// keep indentation and inner line breaks.
fn block_text(e: &Element) -> String {
    let t = text_of(e);
    let lines: Vec<&str> = t.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let start = lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(lines.len());
    let end = lines.iter().rposition(|l| !l.trim().is_empty()).map(|i| i + 1).unwrap_or(start);
    if start >= end {
        return String::new();
    }
    // Remove indentation common to all lines (from the assembly.xml layout).
    let common = lines[start..end]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    if end - start == 1 {
        return lines[start].trim().to_string();
    }
    lines[start..end].iter().map(|l| if l.len() >= common { &l[common..] } else { l.trim_start() }).collect::<Vec<_>>().join("\n")
}

/// Child nodes of an XML-edit element, minus whitespace-only text.
fn fragment(e: &Element) -> Vec<XMLNode> {
    e.children
        .iter()
        .filter(|n| !matches!(n, XMLNode::Text(t) if t.trim().is_empty()))
        .filter(|n| !matches!(n, XMLNode::Comment(_)))
        .cloned()
        .collect()
}

fn format_of(t: Option<&str>) -> Format {
    match t.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("rpf2") | Some("rpf3") | Some("rpf") => Format::Rpf2,
        Some("img") | Some("img3") => Format::Img3,
        _ => Format::Rpf7,
    }
}

fn parse_ops(e: &Element) -> Result<Vec<Op>> {
    let mut ops = Vec::new();
    for c in e.children.iter().filter_map(|c| c.as_element()) {
        let op = match local(&c.name).as_str() {
            "archive" => Op::Archive {
                path: attr(c, "path").unwrap_or("").to_string(),
                create: attr_bool(c, "createIfNotExist"),
                format: format_of(attr(c, "type")),
                children: parse_ops(c)?,
            },
            "add" => Op::Add { source: attr(c, "source").unwrap_or("").to_string(), dest: text_of(c).trim().to_string() },
            "delete" => Op::Delete { target: text_of(c).trim().to_string() },
            "defragmentation" => Op::Defragmentation,
            "text" => {
                let mut edits = Vec::new();
                for t in c.children.iter().filter_map(|n| n.as_element()) {
                    let line = attr(t, "line").unwrap_or("").to_string();
                    let edit = match local(&t.name).as_str() {
                        "insert" => TextEdit::Insert {
                            after: attr(t, "where").is_some_and(|w| w.eq_ignore_ascii_case("after")),
                            condition: Condition::parse(attr(t, "condition"), Condition::Equal),
                            line,
                            content: block_text(t),
                        },
                        "replace" => TextEdit::Replace {
                            condition: Condition::parse(attr(t, "condition"), Condition::Equal),
                            line,
                            content: block_text(t),
                        },
                        "add" => TextEdit::Add { content: block_text(t) },
                        "delete" => TextEdit::Delete {
                            condition: Condition::parse(attr(t, "condition"), Condition::Equal),
                            line: if line.is_empty() { block_text(t) } else { line },
                        },
                        _ => continue,
                    };
                    edits.push(edit);
                }
                Op::Text { path: attr(c, "path").unwrap_or("").to_string(), create: attr_bool(c, "createIfNotExist"), edits }
            }
            name @ ("xml" | "pso") => {
                let mut edits = Vec::new();
                for x in c.children.iter().filter_map(|n| n.as_element()) {
                    let xpath = attr(x, "xpath").unwrap_or("").to_string();
                    let edit = match local(&x.name).as_str() {
                        "add" => XmlEdit::Add {
                            xpath,
                            mode: match attr(x, "append").map(|s| s.to_ascii_lowercase()).as_deref() {
                                Some("first") => AddMode::First,
                                Some("before") => AddMode::Before,
                                Some("after") => AddMode::After,
                                _ => AddMode::Last,
                            },
                            nodes: fragment(x),
                        },
                        "replace" => XmlEdit::Replace { xpath, nodes: fragment(x) },
                        "remove" => XmlEdit::Remove { xpath },
                        _ => continue,
                    };
                    edits.push(edit);
                }
                Op::Xml { path: attr(c, "path").unwrap_or("").to_string(), pso: name == "pso", edits }
            }
            _ => continue,
        };
        ops.push(op);
    }
    Ok(ops)
}

fn parse_assembly(bytes: &[u8]) -> Result<(Metadata, Vec<Op>)> {
    let cfg = ParserConfig::new().ignore_comments(true).whitespace_to_characters(true).cdata_to_characters(true);
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}');
    let root = Element::parse_with_config(text.as_bytes(), cfg).map_err(|e| Error::Invalid(format!("assembly.xml: {e}")))?;
    if local(&root.name) != "package" {
        return Err(Error::Invalid("assembly.xml: root element must be <package>".into()));
    }
    let mut m = Metadata {
        format_version: attr(&root, "version").unwrap_or("").to_string(),
        id: attr(&root, "id").unwrap_or("").to_string(),
        target: attr(&root, "target").unwrap_or("").to_string(),
        ..Default::default()
    };
    if let Some(md) = child(&root, "metadata") {
        m.name = child_text(md, "name");
        if let Some(v) = child(md, "version") {
            let (maj, min, tag) = (child_text(v, "major"), child_text(v, "minor"), child_text(v, "tag"));
            m.version = if maj.is_empty() { text_of(v).trim().to_string() } else { format!("{maj}.{}", if min.is_empty() { "0" } else { &min }) };
            if !tag.is_empty() {
                m.version.push_str(&format!(" {tag}"));
            }
        }
        if let Some(a) = child(md, "author") {
            m.author = child_text(a, "displayName");
            for k in ["actionLink", "web", "youtube", "facebook", "twitter"] {
                let v = child_text(a, &k.to_ascii_lowercase());
                if !v.is_empty() {
                    m.author_links.push((k.to_string(), v));
                }
            }
        }
        m.description = child(md, "description").map(|d| text_of(d).trim().to_string()).unwrap_or_default();
        m.large_description = child(md, "largedescription").map(|d| text_of(d).trim().to_string()).unwrap_or_default();
        m.license = child(md, "licence").or_else(|| child(md, "license")).map(|d| text_of(d).trim().to_string()).unwrap_or_default();
    }
    if let Some(c) = child(&root, "colors") {
        if let Some(h) = child(c, "headerbackground") {
            m.header_color = parse_color(&text_of(h));
            m.header_black_text = attr_bool(h, "useBlackTextColor");
        }
        m.icon_color = child(c, "iconbackground").and_then(|i| parse_color(&text_of(i)));
    }
    let gv = child_text(&root, "gameversion");
    if !gv.is_empty() {
        m.game_version = Some(gv.to_ascii_lowercase());
    }
    let ops = match child(&root, "content") {
        Some(c) => parse_ops(c)?,
        None => Vec::new(),
    };
    Ok((m, ops))
}

impl Package {
    /// Open an `.oiv` file or an unpacked package folder.
    pub fn open(path: &Path) -> Result<Package> {
        if path.is_dir() {
            return Self::open_dir(path);
        }
        let f = File::open(path)?;
        let mut zip = zip::ZipArchive::new(f).map_err(|e| Error::Invalid(format!("{}: not a valid .oiv (zip) file: {e}", path.display())))?;
        let mut index = HashMap::new();
        for i in 0..zip.len() {
            if let Ok(e) = zip.by_index_raw(i) {
                if !e.is_dir() {
                    if let Ok(name) = e.name() {
                        index.insert(norm(&name), i);
                    }
                }
            }
        }
        // assembly.xml may sit in a sub-folder of the zip; use the shallowest.
        let asm = index
            .keys()
            .filter(|k| k.rsplit('/').next() == Some("assembly.xml"))
            .min_by_key(|k| k.matches('/').count())
            .cloned()
            .ok_or_else(|| Error::Invalid("package has no assembly.xml".into()))?;
        let base = asm.rsplit_once('/').map(|x| format!("{}/", x.0)).unwrap_or_default();
        let read = |zip: &mut zip::ZipArchive<File>, idx: usize| -> Result<Vec<u8>> {
            let mut v = Vec::new();
            zip.by_index(idx).map_err(|e| Error::Invalid(e.to_string()))?.read_to_end(&mut v)?;
            Ok(v)
        };
        let asm_bytes = read(&mut zip, index[&asm])?;
        let (meta, ops) = parse_assembly(&asm_bytes)?;
        let icon = match index.get(&format!("{base}icon.png")) {
            Some(&i) => read(&mut zip, i).ok(),
            None => None,
        };
        Ok(Package { path: path.to_path_buf(), meta, icon, ops, content: Content::Zip { zip: Mutex::new(zip), index, base } })
    }

    fn open_dir(dir: &Path) -> Result<Package> {
        let asm = std::fs::read(dir.join("assembly.xml")).map_err(|e| Error::Invalid(format!("{}: no assembly.xml ({e})", dir.display())))?;
        let (meta, ops) = parse_assembly(&asm)?;
        let icon = std::fs::read(dir.join("icon.png")).ok();
        Ok(Package { path: dir.to_path_buf(), meta, icon, ops, content: Content::Dir(dir.join("content")) })
    }

    /// Size of a content file, if present.
    pub fn content_size(&self, source: &str) -> Option<u64> {
        match &self.content {
            Content::Zip { zip, index, base } => {
                let i = *index.get(&format!("{base}content/{}", norm(source)))?;
                zip.lock().unwrap().by_index_raw(i).ok().map(|e| e.size())
            }
            Content::Dir(d) => find_ci(d, source).and_then(|p| p.metadata().ok()).map(|m| m.len()),
        }
    }

    /// Read a content file (path relative to `content/`, any case).
    pub fn read(&self, source: &str) -> Result<Vec<u8>> {
        let mut v = Vec::new();
        self.copy_to(source, &mut v)?;
        Ok(v)
    }

    /// Stream a content file into `w`.
    pub fn copy_to(&self, source: &str, w: &mut dyn std::io::Write) -> Result<u64> {
        match &self.content {
            Content::Zip { zip, index, base } => {
                let key = format!("{base}content/{}", norm(source));
                let &i = index.get(&key).ok_or_else(|| Error::NotFound(format!("package content/{source}")))?;
                let mut z = zip.lock().unwrap();
                let mut e = z.by_index(i).map_err(|e| Error::Invalid(e.to_string()))?;
                Ok(std::io::copy(&mut e, w)?)
            }
            Content::Dir(d) => {
                let p = find_ci(d, source).ok_or_else(|| Error::NotFound(format!("package content/{source}")))?;
                let mut f = File::open(p)?;
                Ok(std::io::copy(&mut f, w)?)
            }
        }
    }

    pub fn has(&self, source: &str) -> bool {
        self.content_size(source).is_some()
    }
}

/// Case-insensitive path lookup under `base` (packages are made on Windows).
fn find_ci(base: &Path, rel: &str) -> Option<PathBuf> {
    let mut cur = base.to_path_buf();
    for part in norm(rel).split('/').filter(|s| !s.is_empty()) {
        let direct = cur.join(part);
        if direct.exists() {
            cur = direct;
            continue;
        }
        let found = std::fs::read_dir(&cur).ok()?.flatten().find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))?;
        cur = found.path();
    }
    cur.is_file().then_some(cur)
}

#[allow(dead_code)]
fn _assert_send<R: Read + Seek>() {}

#[cfg(test)]
mod tests {
    use super::*;

    const ASM: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<package version="2.1" id="{1234}" target="Five">
  <metadata>
    <name>Test Car</name>
    <version><major>1</major><minor>2</minor><tag>Beta</tag></version>
    <author><displayName>Someone</displayName><web>https://example.com</web></author>
    <description footerLink="https://x"><![CDATA[A car.]]></description>
  </metadata>
  <colors>
    <headerBackground useBlackTextColor="True">$FF272727</headerBackground>
    <iconBackground>$FF2E2E2E</iconBackground>
  </colors>
  <content>
    <archive path="update\update.rpf" createIfNotExist="True" type="RPF7">
      <archive path="x64\dlcpacks\car\dlc.rpf" createIfNotExist="True" type="RPF7">
        <add source="car/car.yft">x64\vehicles.rpf\car.yft</add>
      </archive>
      <xml path="common\data\dlclist.xml">
        <add xpath="/SMandatoryPacksData/Paths" append="Last"><Item>dlcpacks:/car/</Item></add>
        <remove xpath="/SMandatoryPacksData/Paths/Item[3]" />
      </xml>
      <text path="common\data\x.dat" createIfNotExist="True">
        <insert where="After" line="# start" condition="Equal">
          LINE A
          LINE B
        </insert>
        <delete condition="StartWith">OLD</delete>
      </text>
    </archive>
    <add source="readme.txt">readme_car.txt</add>
    <defragmentation archive="update\update.rpf" />
  </content>
</package>"##;

    #[test]
    fn parses_assembly() {
        let (m, ops) = parse_assembly(ASM.as_bytes()).unwrap();
        assert_eq!(m.name, "Test Car");
        assert_eq!(m.version, "1.2 Beta");
        assert_eq!(m.author, "Someone");
        assert_eq!(m.description, "A car.");
        assert_eq!(m.header_color, Some(0xFF27_2727));
        assert!(m.header_black_text);
        assert!(m.is_gta5());
        assert_eq!(ops.len(), 3);
        assert_eq!(Op::count(&ops), 5);
        let Op::Archive { path, create, children, .. } = &ops[0] else { panic!() };
        assert_eq!(path, "update\\update.rpf");
        assert!(*create);
        let Op::Text { edits, .. } = &children[2] else { panic!() };
        let TextEdit::Insert { after, content, .. } = &edits[0] else { panic!() };
        assert!(*after);
        assert_eq!(content, "LINE A\nLINE B");
        let Op::Xml { edits, .. } = &children[1] else { panic!() };
        let XmlEdit::Add { nodes, mode, .. } = &edits[0] else { panic!() };
        assert_eq!(*mode, AddMode::Last);
        assert_eq!(nodes.len(), 1);
    }
}
