//! Binary metadata ⇄ XML, so binary files can be viewed and edited like
//! the game's text `.meta` files.
//!
//! * PSO (`.ymt`, `.ymf`, `.pso`): big-endian sections with their own schema.
//! * RSC7 "Meta" resources (`.ytyp`, `.ymap`, ped `.ymt`).
//! * RBF (binary XML, some `.meta` / `.ymt`).
//!
//! PSO and Meta use the public-domain `rage-formats` crate; RBF is handled
//! here (layout from CodeWalker's Rbf.cs, MIT, © 2015 Neodymium).
//!
//! Every conversion back to binary is checked: before editing, the original
//! file is converted to XML and back and must come out identical, otherwise
//! ragekit refuses to touch it.

use std::collections::HashMap;
use std::fmt::Write as _;

use rage_formats::names::NameTable;
use rage_formats::schema::Schema;
use rage_formats::{meta_read, meta_write, pso, pso_write, xml as rxml};
use xmltree::{Element, XMLNode};

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pso,
    Meta,
    Rbf,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Pso => "PSO",
            Kind::Meta => "RSC7 Meta",
            Kind::Rbf => "RBF",
        }
    }
}

const RBF_MAGIC: u32 = 0x3046_4252; // "RBF0"

/// What kind of binary metadata this is, if any.
pub fn detect(data: &[u8]) -> Option<Kind> {
    if data.len() < 16 {
        return None;
    }
    if pso::is_pso(data) {
        return Some(Kind::Pso);
    }
    let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if magic == RBF_MAGIC {
        return Some(Kind::Rbf);
    }
    // RSC7 Meta resources have resource version 2.
    if let Some((ver, _, _)) = crate::resource::parse_header(data) {
        if ver == 2 {
            return Some(Kind::Meta);
        }
    }
    None
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Invalid(e.to_string())
}

/// The built-in name table (tens of thousands of names), built once.
pub fn core_names() -> &'static NameTable {
    static N: std::sync::OnceLock<NameTable> = std::sync::OnceLock::new();
    N.get_or_init(NameTable::core)
}

/// Convert binary metadata to XML. `extra` adds names to resolve hashes.
pub fn to_xml(data: &[u8], extra: &[String]) -> Result<(String, Kind)> {
    let kind = detect(data).ok_or_else(|| Error::Invalid("not binary metadata".into()))?;
    match kind {
        Kind::Rbf => Ok((rbf_to_xml(&rbf_parse(data)?), kind)),
        Kind::Pso | Kind::Meta => {
            let tree = meta_read::parse_tree(data).map_err(err)?;
            let names = with_extra(extra);
            Ok((rxml::to_xml(&tree, names.as_ref().unwrap_or(core_names())), kind))
        }
    }
}

fn with_extra(extra: &[String]) -> Option<NameTable> {
    if extra.is_empty() {
        return None;
    }
    let mut n = core_names().clone();
    for s in extra {
        n.add(s);
        n.add_lowercase(s);
    }
    Some(n)
}

/// Convert edited XML back to the binary form of `original`.
pub fn from_xml(xml: &str, original: &[u8]) -> Result<Vec<u8>> {
    let kind = detect(original).ok_or_else(|| Error::Invalid("original is not binary metadata".into()))?;
    match kind {
        Kind::Rbf => {
            let types = rbf_types(&rbf_parse(original)?);
            let node = rbf_from_xml(xml, &types)?;
            Ok(rbf_write(&node))
        }
        Kind::Pso | Kind::Meta => {
            let tree = rxml::from_xml(xml).map_err(err)?;
            let mut schema = Schema::builtin().clone();
            // The file's own definitions win (Legacy and Enhanced differ).
            schema.merge(Schema::from_file(original).map_err(err)?);
            let w = if kind == Kind::Pso { pso_write::build_pso(&tree, &schema) } else { meta_write::build_meta(&tree, &schema) }
                .map_err(err)?;
            if !w.warnings.is_empty() {
                return Err(Error::Invalid(format!("conversion would lose data: {}", w.warnings.join("; "))));
            }
            // The result must read back.
            meta_read::parse_tree(&w.bytes).map_err(|e| Error::Invalid(format!("the rebuilt file doesn't read back: {e}")))?;
            Ok(w.bytes)
        }
    }
}

/// Build a new binary file from XML using only the built-in schema
/// (for tests and for files with no original to copy the layout from).
pub fn pso_from_xml_builtin(xml: &str) -> Result<Vec<u8>> {
    let tree = rxml::from_xml(xml).map_err(err)?;
    Ok(pso_write::build_pso(&tree, Schema::builtin()).map_err(err)?.bytes)
}

/// Check that this file survives binary → XML → binary unchanged, so
/// editing it can't silently damage parts nobody edited.
pub fn check_round_trip(data: &[u8]) -> Result<()> {
    let (x1, kind) = to_xml(data, &[])?;
    let back = from_xml(&x1, data)?;
    let same = match kind {
        Kind::Rbf => rbf_parse(&back)? == rbf_parse(data)?,
        _ => meta_read::parse_tree(&back).map_err(err)? == meta_read::parse_tree(data).map_err(err)?,
    };
    if !same {
        return Err(Error::Invalid(format!(
            "this {} file doesn't convert to XML and back without changes, so ragekit won't edit it",
            kind.label()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------- RBF ---

#[derive(Debug, Clone, PartialEq)]
pub enum RbfVal {
    U32(String, u32),
    Bool(String, bool),
    F32(String, f32),
    Vec3(String, [f32; 3]),
    Str(String, String),
    Bytes(Vec<u8>),
    Struct(RbfNode),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RbfNode {
    pub name: String,
    pub attrs: Vec<RbfVal>,
    pub children: Vec<RbfVal>,
}

impl RbfVal {
    fn name(&self) -> &str {
        match self {
            RbfVal::U32(n, _) | RbfVal::Bool(n, _) | RbfVal::F32(n, _) | RbfVal::Vec3(n, _) | RbfVal::Str(n, _) => n,
            RbfVal::Struct(s) => &s.name,
            RbfVal::Bytes(_) => "",
        }
    }
    fn type_code(&self) -> u8 {
        match self {
            RbfVal::Struct(_) | RbfVal::Bytes(_) => 0,
            RbfVal::U32(..) => 0x10,
            RbfVal::Bool(_, true) => 0x20,
            RbfVal::Bool(_, false) => 0x30,
            RbfVal::F32(..) => 0x40,
            RbfVal::Vec3(..) => 0x50,
            RbfVal::Str(..) => 0x60,
        }
    }
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self.p.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| Error::Malformed("RBF: unexpected end".into()))?;
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
}

pub fn rbf_parse(data: &[u8]) -> Result<RbfNode> {
    let mut r = Rd { b: data, p: 0 };
    if r.u32()? != RBF_MAGIC {
        return Err(Error::Malformed("not an RBF file".into()));
    }
    let mut names: Vec<String> = Vec::new();
    // Stack of (node, pending attribute count).
    let mut stack: Vec<(RbfNode, usize)> = Vec::new();
    let mut root: Option<RbfNode> = None;
    fn add(stack: &mut [(RbfNode, usize)], v: RbfVal) -> Result<()> {
        let (top, pending) = stack.last_mut().ok_or_else(|| Error::Malformed("RBF: value outside any element".into()))?;
        if *pending > 0 {
            *pending -= 1;
            top.attrs.push(v);
        } else {
            top.children.push(v);
        }
        Ok(())
    }
    while r.p < data.len() {
        let idx = r.u8()?;
        if idx == 0xFF {
            if r.u8()? != 0xFF {
                return Err(Error::Malformed("RBF: bad close marker".into()));
            }
            let (node, _) = stack.pop().ok_or_else(|| Error::Malformed("RBF: unbalanced close".into()))?;
            if stack.is_empty() {
                if root.is_some() || r.p != data.len() {
                    return Err(Error::Malformed("RBF: data after the root element".into()));
                }
                root = Some(node);
            } else {
                add(&mut stack, RbfVal::Struct(node))?;
            }
            continue;
        }
        if idx == 0xFD {
            if r.u8()? != 0xFF {
                return Err(Error::Malformed("RBF: bad bytes marker".into()));
            }
            let n = r.u32()? as usize;
            let b = r.take(n)?.to_vec();
            add(&mut stack, RbfVal::Bytes(b))?;
            continue;
        }
        let ty = r.u8()?;
        let idx = idx as usize;
        if idx == names.len() {
            let n = r.u16()? as usize;
            names.push(String::from_utf8_lossy(r.take(n)?).into_owned());
        } else if idx > names.len() {
            return Err(Error::Malformed("RBF: bad name index".into()));
        }
        let name = names[idx].clone();
        match ty {
            0 => {
                r.take(4)?;
                let pending = r.u16()? as usize;
                stack.push((RbfNode { name, ..Default::default() }, pending));
            }
            0x10 => add(&mut stack, RbfVal::U32(name, r.u32()?))?,
            0x20 => add(&mut stack, RbfVal::Bool(name, true))?,
            0x30 => add(&mut stack, RbfVal::Bool(name, false))?,
            0x40 => add(&mut stack, RbfVal::F32(name, r.f32()?))?,
            0x50 => add(&mut stack, RbfVal::Vec3(name, [r.f32()?, r.f32()?, r.f32()?]))?,
            0x60 => {
                let n = r.u16()? as usize;
                let s = String::from_utf8_lossy(r.take(n)?).into_owned();
                add(&mut stack, RbfVal::Str(name, s))?
            }
            t => return Err(Error::Malformed(format!("RBF: unknown value type {t:#x}"))),
        }
    }
    root.ok_or_else(|| Error::Malformed("RBF: no root element".into()))
}

pub fn rbf_write(root: &RbfNode) -> Vec<u8> {
    let mut out = RBF_MAGIC.to_le_bytes().to_vec();
    let mut names: HashMap<String, u8> = HashMap::new();
    fn rec_id(out: &mut Vec<u8>, names: &mut HashMap<String, u8>, name: &str, ty: u8) {
        let n = names.len() as u8;
        match names.get(name) {
            Some(&i) => {
                out.push(i);
                out.push(ty);
            }
            None => {
                names.insert(name.to_string(), n);
                out.push(n);
                out.push(ty);
                out.extend_from_slice(&(name.len() as u16).to_le_bytes());
                out.extend_from_slice(name.as_bytes());
            }
        }
    }
    fn val(out: &mut Vec<u8>, names: &mut HashMap<String, u8>, v: &RbfVal) {
        match v {
            RbfVal::Struct(s) => node(out, names, s),
            RbfVal::Bytes(b) => {
                out.extend_from_slice(&[0xFD, 0xFF]);
                out.extend_from_slice(&(b.len() as u32).to_le_bytes());
                out.extend_from_slice(b);
            }
            other => {
                rec_id(out, names, other.name(), other.type_code());
                match other {
                    RbfVal::U32(_, x) => out.extend_from_slice(&x.to_le_bytes()),
                    RbfVal::F32(_, x) => out.extend_from_slice(&x.to_bits().to_le_bytes()),
                    RbfVal::Vec3(_, v) => {
                        for x in v {
                            out.extend_from_slice(&x.to_bits().to_le_bytes());
                        }
                    }
                    RbfVal::Str(_, s) => {
                        out.extend_from_slice(&(s.len() as u16).to_le_bytes());
                        out.extend_from_slice(s.as_bytes());
                    }
                    _ => {}
                }
            }
        }
    }
    fn node(out: &mut Vec<u8>, names: &mut HashMap<String, u8>, n: &RbfNode) {
        rec_id(out, names, &n.name, 0);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(n.attrs.len() as u16).to_le_bytes());
        for a in &n.attrs {
            val(out, names, a);
        }
        for c in &n.children {
            val(out, names, c);
        }
        out.extend_from_slice(&[0xFF, 0xFF]);
    }
    node(&mut out, &mut names, root);
    out
}

fn fmt_f32(f: f32) -> String {
    // Shortest text that reads back to the same float.
    let s = format!("{f}");
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn rbf_to_xml(root: &RbfNode) -> String {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    fn scalar(v: &RbfVal) -> Option<String> {
        Some(match v {
            RbfVal::U32(_, x) => x.to_string(),
            RbfVal::Bool(_, b) => b.to_string(),
            RbfVal::F32(_, f) => fmt_f32(*f),
            RbfVal::Str(_, t) => t.clone(),
            _ => return None,
        })
    }
    fn node(s: &mut String, n: &RbfNode, depth: usize) {
        let pad = "  ".repeat(depth);
        let mut attrs = String::new();
        for a in &n.attrs {
            if let Some(v) = scalar(a) {
                let _ = write!(attrs, " {}=\"{}\"", a.name(), esc(&v));
            }
        }
        // Text content: a single bytes child (null-terminated string).
        if let [RbfVal::Bytes(b)] = n.children.as_slice() {
            let content = n.attrs.iter().find_map(|a| match a {
                RbfVal::Str(k, v) if k == "content" => Some(v.as_str()),
                _ => None,
            });
            let text = match content {
                Some("char_array") => b.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" "),
                Some("short_array") => b.chunks(2).map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]).to_string()).collect::<Vec<_>>().join(" "),
                _ => String::from_utf8_lossy(b.strip_suffix(&[0]).unwrap_or(b)).into_owned(),
            };
            let _ = writeln!(s, "{pad}<{}{attrs}>{}</{}>", n.name, esc(&text), n.name);
            return;
        }
        if n.children.is_empty() {
            let _ = writeln!(s, "{pad}<{}{attrs} />", n.name);
            return;
        }
        let _ = writeln!(s, "{pad}<{}{attrs}>", n.name);
        let cpad = "  ".repeat(depth + 1);
        for c in &n.children {
            match c {
                RbfVal::Struct(sub) => node(s, sub, depth + 1),
                RbfVal::Vec3(name, v) => {
                    let _ = writeln!(s, "{cpad}<{name} x=\"{}\" y=\"{}\" z=\"{}\" />", fmt_f32(v[0]), fmt_f32(v[1]), fmt_f32(v[2]));
                }
                RbfVal::Str(name, t) => {
                    let _ = writeln!(s, "{cpad}<{name}>{}</{name}>", esc(t));
                }
                RbfVal::Bytes(b) => {
                    let _ = writeln!(s, "{cpad}{}", esc(&String::from_utf8_lossy(b.strip_suffix(&[0]).unwrap_or(b))));
                }
                other => {
                    let _ = writeln!(s, "{cpad}<{} value=\"{}\" />", other.name(), esc(&scalar(other).unwrap_or_default()));
                }
            }
        }
        let _ = writeln!(s, "{pad}</{}>", n.name);
    }
    node(&mut s, root, 0);
    s
}

/// The kind of value each name had in a file (types are lost in XML).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RType {
    U32,
    Bool,
    F32,
    Vec3,
    Str,
    Struct,
}

fn rbf_types(root: &RbfNode) -> HashMap<String, RType> {
    let mut m = HashMap::new();
    fn rec(n: &RbfNode, m: &mut HashMap<String, RType>) {
        m.entry(n.name.clone()).or_insert(RType::Struct);
        for v in n.attrs.iter().chain(n.children.iter()) {
            let t = match v {
                RbfVal::U32(..) => RType::U32,
                RbfVal::Bool(..) => RType::Bool,
                RbfVal::F32(..) => RType::F32,
                RbfVal::Vec3(..) => RType::Vec3,
                RbfVal::Str(..) => RType::Str,
                RbfVal::Struct(s) => {
                    rec(s, m);
                    continue;
                }
                RbfVal::Bytes(_) => continue,
            };
            m.entry(v.name().to_string()).or_insert(t);
        }
    }
    rec(root, &mut m);
    m
}

fn guess(name: &str, v: &str, types: &HashMap<String, RType>) -> RbfVal {
    let name = name.to_string();
    match types.get(&name) {
        Some(RType::U32) => {
            let t = v.trim();
            let x = t.strip_prefix("0x").and_then(|h| u32::from_str_radix(h, 16).ok()).or_else(|| t.parse::<u32>().ok()).or_else(|| t.parse::<i32>().ok().map(|i| i as u32));
            if let Some(x) = x {
                return RbfVal::U32(name, x);
            }
        }
        Some(RType::Bool) => return RbfVal::Bool(name, v.trim().eq_ignore_ascii_case("true")),
        Some(RType::F32) => {
            if let Ok(f) = v.trim().parse::<f32>() {
                return RbfVal::F32(name, f);
            }
        }
        Some(RType::Str) => return RbfVal::Str(name, v.to_string()),
        _ => {}
    }
    let t = v.trim();
    if t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false") {
        RbfVal::Bool(name, t.eq_ignore_ascii_case("true"))
    } else if let Ok(x) = t.parse::<u32>() {
        RbfVal::U32(name, x)
    } else if let Ok(f) = t.parse::<f32>() {
        RbfVal::F32(name, f)
    } else {
        RbfVal::Str(name, v.to_string())
    }
}

fn rbf_from_xml(xml: &str, types: &HashMap<String, RType>) -> Result<RbfNode> {
    let cfg = xmltree::ParserConfig::new().trim_whitespace(true).ignore_comments(true);
    let root = Element::parse_with_config(xml.trim_start_matches('\u{feff}').as_bytes(), cfg).map_err(|e| Error::Invalid(format!("XML: {e}")))?;
    fn node(e: &Element, types: &HashMap<String, RType>) -> RbfNode {
        let mut n = RbfNode { name: e.name.clone(), ..Default::default() };
        for (k, v) in &e.attributes {
            n.attrs.push(guess(k, v, types));
        }
        let elems: Vec<&Element> = e.children.iter().filter_map(|c| c.as_element()).collect();
        let text: String = e.children.iter().filter_map(|c| match c {
            XMLNode::Text(t) | XMLNode::CData(t) => Some(t.as_str()),
            _ => None,
        }).collect();
        if elems.is_empty() && !text.is_empty() {
            let content = e.attributes.get("content").map(String::as_str);
            let bytes = match content {
                Some("char_array") => text.split_whitespace().filter_map(|x| x.parse::<u8>().ok()).collect(),
                Some("short_array") => text.split_whitespace().filter_map(|x| x.parse::<u16>().ok()).flat_map(|x| x.to_le_bytes()).collect(),
                _ => {
                    let mut b = text.as_bytes().to_vec();
                    b.push(0);
                    b
                }
            };
            n.children.push(RbfVal::Bytes(bytes));
            return n;
        }
        for c in elems {
            let only_value = c.attributes.len() == 1 && c.attributes.contains_key("value") && c.children.is_empty();
            let is_vec3 = c.attributes.len() == 3 && ["x", "y", "z"].iter().all(|k| c.attributes.contains_key(*k)) && c.children.is_empty();
            let declared = types.get(&c.name).copied();
            if only_value && declared != Some(RType::Struct) {
                n.children.push(guess(&c.name, &c.attributes["value"], types));
            } else if is_vec3 && declared != Some(RType::Struct) {
                let f = |k: &str| c.attributes[k].trim().parse::<f32>().unwrap_or(0.0);
                n.children.push(RbfVal::Vec3(c.name.clone(), [f("x"), f("y"), f("z")]));
            } else if declared == Some(RType::Str) && c.attributes.is_empty() {
                let t: String = c.children.iter().filter_map(|x| x.as_text()).collect();
                n.children.push(RbfVal::Str(c.name.clone(), t));
            } else {
                n.children.push(RbfVal::Struct(node(c, types)));
            }
        }
        n
    }
    Ok(node(&root, types))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rbf() -> RbfNode {
        RbfNode {
            name: "CDataFileMgr__ContentsOfDataFileXml".into(),
            attrs: vec![],
            children: vec![
                RbfVal::Struct(RbfNode {
                    name: "disabledFiles".into(),
                    attrs: vec![RbfVal::Str("content".into(), "char_array".into())],
                    children: vec![RbfVal::Bytes(vec![1, 2, 3])],
                }),
                RbfVal::Struct(RbfNode {
                    name: "filesToEnable".into(),
                    attrs: vec![],
                    children: vec![RbfVal::Struct(RbfNode { name: "Item".into(), attrs: vec![], children: vec![RbfVal::Bytes(b"dlc_x:/a.meta\0".to_vec())] })],
                }),
                RbfVal::U32("count".into(), 7),
                RbfVal::Bool("enabled".into(), true),
                RbfVal::F32("scale".into(), 1.5),
                RbfVal::Vec3("pos".into(), [1.0, -2.25, 3.0]),
                RbfVal::Struct(RbfNode {
                    name: "item".into(),
                    attrs: vec![RbfVal::Str("type".into(), "CFoo".into()), RbfVal::F32("w".into(), 2.0)],
                    children: vec![],
                }),
            ],
        }
    }

    #[test]
    fn rbf_binary_round_trip() {
        let n = sample_rbf();
        let b = rbf_write(&n);
        assert_eq!(rbf_parse(&b).unwrap(), n);
        assert_eq!(detect(&b), Some(Kind::Rbf));
    }

    #[test]
    fn rbf_xml_round_trip() {
        let b = rbf_write(&sample_rbf());
        check_round_trip(&b).unwrap();
        let (x, _) = to_xml(&b, &[]).unwrap();
        assert!(x.contains("<Item>dlc_x:/a.meta</Item>"), "{x}");
        assert!(x.contains(r#"<pos x="1.0" y="-2.25" z="3.0" />"#), "{x}");
        // An edit through XML.
        let edited = x.replace("</filesToEnable>", "  <Item>dlc_x:/b.meta</Item>\n  </filesToEnable>");
        let out = from_xml(&edited, &b).unwrap();
        let (x2, _) = to_xml(&out, &[]).unwrap();
        assert!(x2.contains("dlc_x:/b.meta"));
        assert_eq!(rbf_parse(&out).unwrap().children.len(), 7);
    }

    #[test]
    fn rejects_garbage() {
        assert!(rbf_parse(b"RBF0\x05\x00").is_err());
        assert_eq!(detect(b"hello world, not meta"), None);
    }
}
