//! Text map data: item definitions (`.ide`) and item placements (`.ipl`)
//! as used by GTA IV and EFLC (and, in the same shape, the older 3D-era
//! games). A file is a list of sections:
//!
//! ```text
//! objs
//! cj_box, cj_box_txd, 300, 0, 0, -1.0, -1.0, 0.0, 1.0, 1.0, 1.0, 0, 0, 0.5, 1.4, null
//! end
//! ```
//!
//! Editing keeps every line that wasn't touched byte for byte (comments,
//! spacing, blank lines), so a round trip without edits is lossless.

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    /// A data row: the fields, and the original text when unedited.
    Row { fields: Vec<String>, original: Option<String> },
    /// Comment, blank or anything that isn't a row.
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    /// As written (e.g. "inst").
    pub name: String,
    /// The original header and `end` lines.
    pub header: String,
    pub end: Option<String>,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapFile {
    /// Lines before the first section.
    pub preamble: Vec<String>,
    pub sections: Vec<Section>,
    /// "\r\n" or "\n", as found.
    pub newline: &'static str,
    pub kind: MapKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapKind {
    Ide,
    Ipl,
}

pub fn kind_of(name: &str) -> Option<MapKind> {
    let l = name.to_ascii_lowercase();
    if l.ends_with(".ide") {
        Some(MapKind::Ide)
    } else if l.ends_with(".ipl") {
        Some(MapKind::Ipl)
    } else {
        None
    }
}

const SECTIONS: &[&str] = &[
    // .ide
    "objs", "tobj", "tree", "path", "anim", "tanm", "mlo", "2dfx", "amat", "txdp", "hier", "cars", "peds", "weap", "lodm", "agrps",
    // .ipl
    "inst", "cull", "grge", "enex", "pick", "jump", "tcyc", "auzo", "mult", "occl", "zone", "blok", "mzon", "vnod", "link", "slow", "rtfx",
];

fn split_fields(s: &str) -> Vec<String> {
    s.split(',').map(|f| f.trim().to_string()).collect()
}

/// Column names for the common GTA IV sections; others get "Field N".
pub fn columns(kind: MapKind, section: &str, n: usize) -> Vec<String> {
    let known: &[&str] = match (kind, section.to_ascii_lowercase().as_str()) {
        (MapKind::Ide, "objs") => &[
            "Model", "Texture dict", "Draw distance", "Flags 1", "Flags 2", "Min X", "Min Y", "Min Z", "Max X", "Max Y", "Max Z", "Centre X",
            "Centre Y", "Centre Z", "Radius", "LOD / WDD",
        ],
        (MapKind::Ide, "tobj") => &[
            "Model", "Texture dict", "Draw distance", "Flags 1", "Flags 2", "Min X", "Min Y", "Min Z", "Max X", "Max Y", "Max Z", "Centre X",
            "Centre Y", "Centre Z", "Radius", "LOD / WDD", "Time on", "Time off",
        ],
        (MapKind::Ide, "anim") => &[
            "Model", "Texture dict", "Animation", "Draw distance", "Flags 1", "Flags 2", "Min X", "Min Y", "Min Z", "Max X", "Max Y", "Max Z",
            "Centre X", "Centre Y", "Centre Z", "Radius", "LOD / WDD",
        ],
        (MapKind::Ide, "txdp") => &["Texture dict", "Parent"],
        (MapKind::Ide, "cars") => &["Model", "Texture dict", "Type", "Handling", "Game name", "Anims", "Anims 2", "Frequency", "Max num", "Wheel radius front", "Wheel radius rear"],
        (MapKind::Ipl, "inst") => &["X", "Y", "Z", "Rot X", "Rot Y", "Rot Z", "Rot W", "Model", "Flags", "LOD index", "Unknown 1", "LOD distance"],
        (MapKind::Ipl, "grge") => &["X1", "Y1", "Z1", "X2", "Y2", "X3", "Y3", "Z3", "Flags", "Type", "Name"],
        (MapKind::Ipl, "cars") => &["X", "Y", "Z", "Rot X", "Rot Y", "Rot Z", "Rot W", "Model", "Colour 1", "Colour 2", "Colour 3", "Colour 4", "Flags", "Alarm", "Door lock"],
        (MapKind::Ipl, "pick") => &["X", "Y", "Z", "Rot X", "Rot Y", "Rot Z", "Rot W", "Type"],
        _ => &[],
    };
    (0..n).map(|i| known.get(i).map(|s| s.to_string()).unwrap_or_else(|| format!("Field {}", i + 1))).collect()
}

impl MapFile {
    pub fn parse(text: &str, kind: MapKind) -> Result<MapFile> {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let mut f = MapFile { preamble: Vec::new(), sections: Vec::new(), newline, kind };
        let mut cur: Option<Section> = None;
        let body = text.strip_suffix(newline).unwrap_or(text);
        for raw in body.split(newline) {
            let t = raw.trim();
            let word = t.split(|c: char| c == '#' || c.is_whitespace()).next().unwrap_or("").to_ascii_lowercase();
            match &mut cur {
                None => {
                    if SECTIONS.contains(&word.as_str()) && !t.contains(',') {
                        cur = Some(Section { name: t.split_whitespace().next().unwrap().to_string(), header: raw.to_string(), end: None, lines: Vec::new() });
                    } else {
                        f.preamble.push(raw.to_string());
                    }
                }
                Some(s) => {
                    if word == "end" {
                        s.end = Some(raw.to_string());
                        f.sections.push(cur.take().unwrap());
                    } else if t.is_empty() || t.starts_with('#') {
                        s.lines.push(Line::Other(raw.to_string()));
                    } else {
                        let data = t.split('#').next().unwrap_or("").trim();
                        if data.contains(',') || !data.is_empty() {
                            s.lines.push(Line::Row { fields: split_fields(data), original: Some(raw.to_string()) });
                        } else {
                            s.lines.push(Line::Other(raw.to_string()));
                        }
                    }
                }
            }
        }
        if let Some(s) = cur {
            // Missing final "end": keep the section anyway.
            f.sections.push(s);
        }
        if f.sections.is_empty() && !text.trim().is_empty() {
            return Err(Error::Invalid("no map sections found (objs, inst, …)".into()));
        }
        Ok(f)
    }

    pub fn to_text(&self) -> String {
        let nl = self.newline;
        let mut out = String::new();
        for l in &self.preamble {
            out.push_str(l);
            out.push_str(nl);
        }
        for s in &self.sections {
            out.push_str(&s.header);
            out.push_str(nl);
            for l in &s.lines {
                match l {
                    Line::Row { original: Some(o), .. } => out.push_str(o),
                    Line::Row { fields, original: None } => out.push_str(&fields.join(", ")),
                    Line::Other(o) => out.push_str(o),
                }
                out.push_str(nl);
            }
            if let Some(e) = &s.end {
                out.push_str(e);
                out.push_str(nl);
            }
        }
        out
    }

    /// Rows of a section, with their line indices.
    pub fn rows(&self, section: usize) -> Vec<(usize, &Vec<String>)> {
        self.sections[section]
            .lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| match l {
                Line::Row { fields, .. } => Some((i, fields)),
                _ => None,
            })
            .collect()
    }

    pub fn set_field(&mut self, section: usize, line: usize, col: usize, value: &str) {
        if let Some(Line::Row { fields, original }) = self.sections[section].lines.get_mut(line) {
            if col >= fields.len() {
                fields.resize(col + 1, String::new());
            }
            if fields[col] != value {
                fields[col] = value.trim().to_string();
                *original = None;
            }
        }
    }

    /// Add a row after `after` (a line index) or at the end; copies the
    /// given row's fields when one is given.
    pub fn insert_row(&mut self, section: usize, after: Option<usize>, fields: Vec<String>) -> usize {
        let lines = &mut self.sections[section].lines;
        let at = after.map(|a| (a + 1).min(lines.len())).unwrap_or(lines.len());
        lines.insert(at, Line::Row { fields, original: None });
        at
    }

    pub fn remove_line(&mut self, section: usize, line: usize) {
        let lines = &mut self.sections[section].lines;
        if line < lines.len() {
            lines.remove(line);
        }
    }

    pub fn add_section(&mut self, name: &str) -> usize {
        self.sections.push(Section { name: name.to_string(), header: name.to_string(), end: Some("end".into()), lines: Vec::new() });
        self.sections.len() - 1
    }

    /// Case-insensitive search over all fields; (section, line) of matches.
    pub fn find(&self, needle: &str) -> Vec<(usize, usize)> {
        let n = needle.to_ascii_lowercase();
        let mut out = Vec::new();
        for (si, s) in self.sections.iter().enumerate() {
            for (li, l) in s.lines.iter().enumerate() {
                if let Line::Row { fields, .. } = l {
                    if fields.iter().any(|f| f.to_ascii_lowercase().contains(&n)) {
                        out.push((si, li));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IPL: &str = "# test\r\ninst\r\n1.0, 2.0, 3.0, 0, 0, 0, 1, cj_box, 0, -1, 0, 0  # a box\r\n\r\n4, 5, 6, 0, 0, 0, 1, cj_bin, 0, -1, 0, 0\r\nend\r\ncars\r\nend\r\n";

    #[test]
    fn lossless_and_edit() {
        let mut f = MapFile::parse(IPL, MapKind::Ipl).unwrap();
        assert_eq!(f.to_text(), IPL);
        assert_eq!(f.sections.len(), 2);
        let rows = f.rows(0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1[7], "cj_box");
        let line = rows[1].0;
        f.set_field(0, line, 7, "cj_dumpster");
        let t = f.to_text();
        assert!(t.contains("4, 5, 6, 0, 0, 0, 1, cj_dumpster, 0, -1, 0, 0\r\n"));
        assert!(t.contains("cj_box, 0, -1, 0, 0  # a box"), "untouched row keeps its comment");
        let at = f.insert_row(0, Some(line), vec!["7".into(), "8".into()]);
        f.remove_line(0, at);
        assert_eq!(f.find("DUMPSTER"), vec![(0, line)]);
        assert_eq!(columns(MapKind::Ipl, "inst", 3), vec!["X", "Y", "Z"]);
    }
}
