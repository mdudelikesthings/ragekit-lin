//! XML edits used by `.oiv` packages: a small XPath subset for picking an
//! element, and add / replace / remove.
//!
//! Supported XPath: absolute (`/A/B`), relative from the root (`A/B`),
//! descendant (`//Item`), `*`, and predicates `[n]`, `[@a]`, `[@a='v']`,
//! `[child='v']`, `[child/@a='v']`, `[text()='v']`, `[.='v']`,
//! `[contains(x,'v')]`, combined with `and`. Name tests fall back to
//! case-insensitive matching when nothing matches exactly (as OpenIV
//! packages written by hand often get case wrong).

use xmltree::{Element, EmitterConfig, ParserConfig, XMLNode};

use super::package::{AddMode, XmlEdit};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
enum Pred {
    Index(usize),
    Cmp { lhs: Operand, op: CmpOp, value: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
enum CmpOp {
    Eq,
    Ne,
    Contains,
    StartsWith,
    Exists,
}

#[derive(Debug, Clone, PartialEq)]
enum Operand {
    Attr(String),
    Child(String),
    ChildAttr(String, String),
    Text,
}

#[derive(Debug, Clone)]
struct Step {
    descendant: bool,
    name: String,
    preds: Vec<Pred>,
}

fn split_top(s: &str, sep: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut depth, mut quote) = (0i32, None::<char>);
    let mut cur = String::new();
    let b: Vec<char> = s.chars().collect();
    let sepc: Vec<char> = sep.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' => quote = Some(c),
                '[' | '(' => depth += 1,
                ']' | ')' => depth -= 1,
                _ => {
                    if depth == 0 && b[i..].starts_with(&sepc) {
                        out.push(std::mem::take(&mut cur));
                        i += sepc.len();
                        continue;
                    }
                }
            },
        }
        cur.push(c);
        i += 1;
    }
    out.push(cur);
    out
}

fn unquote(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() >= 2 && ((s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"'))) {
        Some(s[1..s.len() - 1].to_string())
    } else if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-') {
        Some(s.to_string())
    } else {
        None
    }
}

fn operand(s: &str) -> Result<Operand> {
    let s = s.trim();
    if s == "." || s == "text()" {
        return Ok(Operand::Text);
    }
    if let Some(a) = s.strip_prefix('@') {
        return Ok(Operand::Attr(a.to_string()));
    }
    if let Some((c, a)) = s.split_once("/@") {
        return Ok(Operand::ChildAttr(c.to_string(), a.to_string()));
    }
    if s.ends_with("/text()") {
        return Ok(Operand::Child(s.trim_end_matches("/text()").to_string()));
    }
    if s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == ':') {
        return Ok(Operand::Child(s.to_string()));
    }
    Err(Error::Invalid(format!("unsupported XPath expression: {s}")))
}

fn parse_pred(p: &str) -> Result<Vec<Pred>> {
    let mut out = Vec::new();
    for part in split_top(p, " and ") {
        let part = part.trim();
        if let Ok(n) = part.parse::<usize>() {
            out.push(Pred::Index(n));
            continue;
        }
        if part == "last()" {
            out.push(Pred::Index(usize::MAX));
            continue;
        }
        for (f, op) in [("contains(", CmpOp::Contains), ("starts-with(", CmpOp::StartsWith)] {
            if let Some(inner) = part.strip_prefix(f).and_then(|r| r.strip_suffix(')')) {
                let args = split_top(inner, ",");
                if args.len() != 2 {
                    return Err(Error::Invalid(format!("bad XPath function: {part}")));
                }
                let v = unquote(&args[1]).ok_or_else(|| Error::Invalid(format!("bad XPath literal: {}", args[1])))?;
                out.push(Pred::Cmp { lhs: operand(&args[0])?, op: op.clone(), value: Some(v) });
            }
        }
        if part.starts_with("contains(") || part.starts_with("starts-with(") {
            continue;
        }
        let ne = split_top(part, "!=");
        if ne.len() == 2 {
            let v = unquote(&ne[1]).ok_or_else(|| Error::Invalid(format!("bad XPath literal: {}", ne[1])))?;
            out.push(Pred::Cmp { lhs: operand(&ne[0])?, op: CmpOp::Ne, value: Some(v) });
            continue;
        }
        let eq = split_top(part, "=");
        if eq.len() == 2 {
            let v = unquote(&eq[1]).ok_or_else(|| Error::Invalid(format!("bad XPath literal: {}", eq[1])))?;
            out.push(Pred::Cmp { lhs: operand(&eq[0])?, op: CmpOp::Eq, value: Some(v) });
            continue;
        }
        out.push(Pred::Cmp { lhs: operand(part)?, op: CmpOp::Exists, value: None });
    }
    Ok(out)
}

/// Byte index of the `]` closing the `[` at the start of `s`.
fn closing_bracket(s: &str) -> Option<usize> {
    let (mut depth, mut quote) = (0i32, None::<char>);
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' => quote = Some(c),
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

fn parse_xpath(x: &str) -> Result<Vec<Step>> {
    let x = x.trim();
    if x.is_empty() {
        return Err(Error::Invalid("empty XPath".into()));
    }
    let mut steps = Vec::new();
    let mut descendant = false;
    // Split on '/' outside brackets/quotes, tracking '//' as an empty segment.
    let segs = split_top(x, "/");
    for (i, seg) in segs.iter().enumerate() {
        let seg = seg.trim();
        if seg.is_empty() {
            if i > 0 {
                descendant = true;
            }
            continue;
        }
        if seg == "." {
            continue;
        }
        let (name, rest) = match seg.find('[') {
            Some(p) => (&seg[..p], &seg[p..]),
            None => (seg, ""),
        };
        let mut preds = Vec::new();
        let mut r = rest;
        while !r.is_empty() {
            if !r.starts_with('[') {
                return Err(Error::Invalid(format!("bad XPath step: {seg}")));
            }
            let end = closing_bracket(r).ok_or_else(|| Error::Invalid(format!("unclosed [ in XPath: {x}")))?;
            preds.extend(parse_pred(&r[1..end])?);
            r = &r[end + 1..];
        }
        if name.starts_with('@') || name.ends_with("()") {
            return Err(Error::Invalid(format!("XPath must select an element: {x}")));
        }
        steps.push(Step { descendant, name: name.to_string(), preds });
        descendant = false;
    }
    if steps.is_empty() {
        return Err(Error::Invalid(format!("empty XPath: {x}")));
    }
    Ok(steps)
}

/// Element text: direct text children, trimmed.
pub fn own_text(e: &Element) -> String {
    let mut s = String::new();
    for c in &e.children {
        if let XMLNode::Text(t) | XMLNode::CData(t) = c {
            s.push_str(t);
        }
    }
    s.trim().to_string()
}

fn name_eq(a: &str, b: &str, ci: bool) -> bool {
    if ci {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn eval_operand(e: &Element, o: &Operand, ci: bool) -> Vec<String> {
    match o {
        Operand::Text => vec![own_text(e)],
        Operand::Attr(a) => e.attributes.iter().filter(|(k, _)| name_eq(k, a, ci)).map(|(_, v)| v.clone()).collect(),
        Operand::Child(c) => e.children.iter().filter_map(|n| n.as_element()).filter(|x| name_eq(&x.name, c, ci)).map(own_text).collect(),
        Operand::ChildAttr(c, a) => e
            .children
            .iter()
            .filter_map(|n| n.as_element())
            .filter(|x| name_eq(&x.name, c, ci))
            .flat_map(|x| x.attributes.iter().filter(|(k, _)| name_eq(k, a, ci)).map(|(_, v)| v.clone()).collect::<Vec<_>>())
            .collect(),
    }
}

fn pred_ok(e: &Element, p: &Pred, ci: bool) -> bool {
    match p {
        Pred::Index(_) => true,
        Pred::Cmp { lhs, op, value } => {
            let vals = eval_operand(e, lhs, ci);
            let v = value.as_deref().unwrap_or("");
            match op {
                CmpOp::Exists => !vals.is_empty(),
                CmpOp::Eq => vals.iter().any(|x| x == v || (ci && x.eq_ignore_ascii_case(v))),
                CmpOp::Ne => !vals.is_empty() && vals.iter().all(|x| x != v),
                CmpOp::Contains => vals.iter().any(|x| x.contains(v)),
                CmpOp::StartsWith => vals.iter().any(|x| x.starts_with(v)),
            }
        }
    }
}

/// Path to an element: indices into successive `children` vectors.
pub type NodePath = Vec<usize>;

pub fn get<'a>(root: &'a Element, p: &[usize]) -> &'a Element {
    let mut cur = root;
    for &i in p {
        cur = cur.children[i].as_element().unwrap();
    }
    cur
}

pub fn get_mut<'a>(root: &'a mut Element, p: &[usize]) -> &'a mut Element {
    let mut cur = root;
    for &i in p {
        cur = cur.children[i].as_mut_element().unwrap();
    }
    cur
}

fn descendants(root: &Element, base: &NodePath, out: &mut Vec<NodePath>) {
    let e = get(root, base);
    for (i, c) in e.children.iter().enumerate() {
        if c.as_element().is_some() {
            let mut p = base.clone();
            p.push(i);
            out.push(p.clone());
            descendants(root, &p, out);
        }
    }
}

fn select_with(root: &Element, steps: &[Step], ci: bool) -> Vec<NodePath> {
    // None = document node.
    let mut ctx: Vec<Option<NodePath>> = vec![None];
    for step in steps {
        let mut next: Vec<Option<NodePath>> = Vec::new();
        for c in &ctx {
            let cands: Vec<NodePath> = match (c, step.descendant) {
                (None, false) => vec![vec![]],
                (None, true) => {
                    let mut v = vec![vec![]];
                    descendants(root, &vec![], &mut v);
                    v
                }
                (Some(p), false) => get(root, p)
                    .children
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| n.as_element().is_some())
                    .map(|(i, _)| {
                        let mut q = p.clone();
                        q.push(i);
                        q
                    })
                    .collect(),
                (Some(p), true) => {
                    let mut v = Vec::new();
                    descendants(root, p, &mut v);
                    v
                }
            };
            let mut matched: Vec<NodePath> =
                cands.into_iter().filter(|p| step.name == "*" || name_eq(&get(root, p).name, &step.name, ci)).collect();
            for pred in &step.preds {
                matched = match pred {
                    Pred::Index(n) => {
                        let idx = if *n == usize::MAX { matched.len().wrapping_sub(1) } else { n.wrapping_sub(1) };
                        matched.get(idx).cloned().into_iter().collect()
                    }
                    p => matched.into_iter().filter(|q| pred_ok(get(root, q), p, ci)).collect(),
                };
            }
            next.extend(matched.into_iter().map(Some));
        }
        ctx = next;
        if ctx.is_empty() {
            break;
        }
    }
    ctx.into_iter().flatten().collect()
}

/// Find the first element matching `xpath`. Returns (path, matched case-insensitively).
pub fn select(root: &Element, xpath: &str) -> Result<Option<(NodePath, bool)>> {
    let steps = parse_xpath(xpath)?;
    if let Some(p) = select_with(root, &steps, false).into_iter().next() {
        return Ok(Some((p, false)));
    }
    Ok(select_with(root, &steps, true).into_iter().next().map(|p| (p, true)))
}

fn strip_ws(e: &mut Element) {
    e.children.retain(|n| !matches!(n, XMLNode::Text(t) if t.trim().is_empty()));
    for c in e.children.iter_mut() {
        if let XMLNode::Element(x) = c {
            strip_ws(x);
        }
    }
}

fn canon(n: &XMLNode) -> String {
    let mut e = match n {
        XMLNode::Element(e) => e.clone(),
        XMLNode::Text(t) => return format!("T:{}", t.trim()),
        other => return format!("{other:?}"),
    };
    strip_ws(&mut e);
    let mut v = Vec::new();
    let _ = e.write_with_config(&mut v, EmitterConfig::new().write_document_declaration(false));
    String::from_utf8_lossy(&v).into_owned()
}

/// A parsed XML file being edited.
pub struct XmlDoc {
    pub root: Element,
    indent: String,
    crlf: bool,
    declaration: bool,
}

impl XmlDoc {
    pub fn parse(data: &[u8]) -> Result<XmlDoc> {
        let text = String::from_utf8_lossy(data);
        let text = text.trim_start_matches('\u{feff}');
        let first = text.trim_start().as_bytes().first().copied();
        if first != Some(b'<') {
            return Err(Error::Invalid("not a text XML file (binary metadata editing is not supported yet)".into()));
        }
        let cfg = ParserConfig::new().trim_whitespace(true).ignore_comments(false).cdata_to_characters(true);
        let root = Element::parse_with_config(text.as_bytes(), cfg).map_err(|e| Error::Invalid(format!("XML parse error: {e}")))?;
        let indent = text
            .lines()
            .skip(1)
            .find_map(|l| {
                let ws: String = l.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                (!ws.is_empty()).then_some(ws)
            })
            .unwrap_or_else(|| "  ".into());
        Ok(XmlDoc { root, indent, crlf: text.contains("\r\n"), declaration: text.trim_start().starts_with("<?xml") })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut root = self.root.clone();
        strip_ws(&mut root);
        let mut v = Vec::new();
        root.write_with_config(
            &mut v,
            EmitterConfig::new()
                .perform_indent(true)
                .indent_string(self.indent.clone())
                .write_document_declaration(self.declaration)
                .pad_self_closing(true),
        )
        .map_err(|e| Error::Invalid(format!("XML write error: {e}")))?;
        let mut s = String::from_utf8_lossy(&v).into_owned();
        if self.declaration {
            // Keep the conventional declaration form.
            if let Some(end) = s.find("?>") {
                s.replace_range(..end + 2, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
            }
        }
        s.push('\n');
        if self.crlf {
            s = s.replace("\r\n", "\n").replace('\n', "\r\n");
        }
        Ok(s.into_bytes())
    }

    /// Apply one edit. Returns a note for the log (Ok) or why it failed.
    pub fn apply(&mut self, edit: &XmlEdit) -> std::result::Result<String, String> {
        let xpath = match edit {
            XmlEdit::Add { xpath, .. } | XmlEdit::Replace { xpath, .. } | XmlEdit::Remove { xpath } => xpath,
        };
        let (path, ci) = match select(&self.root, xpath) {
            Ok(Some(p)) => p,
            Ok(None) => return Err(format!("XPath matched nothing: {xpath}")),
            Err(e) => return Err(e.to_string()),
        };
        let ci_note = if ci { " (matched ignoring case)" } else { "" };
        match edit {
            XmlEdit::Add { mode, nodes, .. } => {
                let (container, at): (NodePath, Option<usize>) = match mode {
                    AddMode::First => (path.clone(), Some(0)),
                    AddMode::Last => (path.clone(), None),
                    AddMode::Before | AddMode::After => {
                        if path.is_empty() {
                            return Err("can't add a sibling to the root element".into());
                        }
                        let i = *path.last().unwrap();
                        (path[..path.len() - 1].to_vec(), Some(if *mode == AddMode::Before { i } else { i + 1 }))
                    }
                };
                let c = get_mut(&mut self.root, &container);
                let existing: Vec<String> = c.children.iter().map(canon).collect();
                let new: Vec<XMLNode> = nodes.iter().filter(|n| !existing.contains(&canon(n))).cloned().collect();
                if new.is_empty() {
                    return Ok(format!("already present at {xpath}, skipped"));
                }
                let n = new.len();
                match at {
                    None => c.children.extend(new),
                    Some(i) => {
                        let i = i.min(c.children.len());
                        c.children.splice(i..i, new);
                    }
                }
                Ok(format!("added {n} node(s) at {xpath}{ci_note}"))
            }
            XmlEdit::Replace { nodes, .. } => {
                if path.is_empty() {
                    let els: Vec<&Element> = nodes.iter().filter_map(|n| n.as_element()).collect();
                    if els.len() != 1 {
                        return Err("replacing the root needs exactly one element".into());
                    }
                    self.root = els[0].clone();
                } else {
                    let i = *path.last().unwrap();
                    let parent = get_mut(&mut self.root, &path[..path.len() - 1]);
                    parent.children.splice(i..=i, nodes.iter().cloned());
                }
                Ok(format!("replaced {xpath}{ci_note}"))
            }
            XmlEdit::Remove { .. } => {
                if path.is_empty() {
                    return Err("can't remove the root element".into());
                }
                let i = *path.last().unwrap();
                get_mut(&mut self.root, &path[..path.len() - 1]).children.remove(i);
                Ok(format!("removed {xpath}{ci_note}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CVehicleModelInfo__InitDataList>
  <InitDatas>
    <Item>
      <modelName>adder</modelName>
      <PovCameraOffset x="0.0" y="-0.1" z="0.6" />
    </Item>
    <Item type="x">
      <modelName>zentorno</modelName>
    </Item>
  </InitDatas>
  <txdRelationships />
</CVehicleModelInfo__InitDataList>
"#;

    fn doc() -> XmlDoc {
        XmlDoc::parse(DOC.as_bytes()).unwrap()
    }

    fn sel(d: &XmlDoc, x: &str) -> Option<String> {
        select(&d.root, x).unwrap().map(|(p, _)| {
            let e = get(&d.root, &p);
            format!("{}:{}", e.name, own_text(e))
        })
    }

    #[test]
    fn xpath_forms() {
        let d = doc();
        assert_eq!(sel(&d, "/CVehicleModelInfo__InitDataList/InitDatas/Item[modelName='adder']/PovCameraOffset").unwrap(), "PovCameraOffset:");
        assert_eq!(sel(&d, "CVehicleModelInfo__InitDataList/InitDatas").unwrap(), "InitDatas:");
        assert_eq!(sel(&d, "//Item[2]/modelName").unwrap(), "modelName:zentorno");
        assert_eq!(sel(&d, "//Item[@type=\"x\"]/modelName").unwrap(), "modelName:zentorno");
        assert_eq!(sel(&d, "//modelName[text()='adder']").unwrap(), "modelName:adder");
        assert_eq!(sel(&d, "//Item[contains(modelName,'zen')]/modelName").unwrap(), "modelName:zentorno");
        assert_eq!(sel(&d, "//Item[PovCameraOffset/@y='-0.1']/modelName").unwrap(), "modelName:adder");
        assert_eq!(sel(&d, "//Item[last()]/modelName").unwrap(), "modelName:zentorno");
        assert_eq!(sel(&d, "//item[modelname='adder']/modelname").unwrap(), "modelName:adder"); // case fallback
        assert!(sel(&d, "//Item[modelName='nope']").is_none());
        assert!(select(&d.root, "//Item[").is_err() || sel(&d, "//Item[").is_none());
    }

    #[test]
    fn edits() {
        let mut d = doc();
        let frag = |s: &str| {
            let e = Element::parse(format!("<r>{s}</r>").as_bytes()).unwrap();
            e.children
        };
        d.apply(&XmlEdit::Add { xpath: "//InitDatas".into(), mode: AddMode::Last, nodes: frag("<Item><modelName>car</modelName></Item>") }).unwrap();
        // Idempotent.
        let again = d.apply(&XmlEdit::Add { xpath: "//InitDatas".into(), mode: AddMode::Last, nodes: frag("<Item><modelName>car</modelName></Item>") }).unwrap();
        assert!(again.contains("skipped"));
        d.apply(&XmlEdit::Add { xpath: "//Item[modelName='adder']".into(), mode: AddMode::Before, nodes: frag("<Item><modelName>first</modelName></Item>") }).unwrap();
        d.apply(&XmlEdit::Replace { xpath: "//Item[modelName='adder']/PovCameraOffset".into(), nodes: frag(r#"<PovCameraOffset x="1" y="2" z="3" />"#) }).unwrap();
        d.apply(&XmlEdit::Remove { xpath: "//Item[modelName='zentorno']".into() }).unwrap();
        assert!(d.apply(&XmlEdit::Remove { xpath: "//Item[modelName='zentorno']".into() }).is_err());
        let out = String::from_utf8(d.to_bytes().unwrap()).unwrap();
        assert!(out.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        let first = out.find("first").unwrap();
        let adder = out.find("adder").unwrap();
        let car = out.find(">car<").unwrap();
        assert!(first < adder && adder < car);
        assert!(!out.contains("zentorno"));
        assert!(out.contains(r#"x="1" y="2" z="3""#));
        // Re-parse to make sure it's valid.
        XmlDoc::parse(out.as_bytes()).unwrap();
    }

    #[test]
    fn rejects_binary() {
        assert!(XmlDoc::parse(b"PSIN\0\0\0").is_err());
    }
}
