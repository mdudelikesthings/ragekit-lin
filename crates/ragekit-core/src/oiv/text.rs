//! Line-based text edits used by `.oiv` packages (for .dat, .ide, .ipl,
//! .txt, .cfg …).

use super::package::{Condition, TextEdit};

fn wildcard_full(pat: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pat.to_lowercase().chars().collect(), s.to_lowercase().chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Does `line` match `pattern` under `cond`? Case-insensitive, ignoring
/// surrounding whitespace.
pub fn line_matches(line: &str, pattern: &str, cond: Condition) -> bool {
    let l = line.trim().to_lowercase();
    let p = pattern.trim().to_lowercase();
    match cond {
        Condition::Equal => l == p,
        Condition::StartWith => l.starts_with(&p),
        Condition::EndWith => l.ends_with(&p),
        Condition::Contains => l.contains(&p),
        Condition::Mask => wildcard_full(&p, &l) || (p.contains('*') && wildcard_full(&format!("*{p}*"), &l)),
    }
}

/// A text file being edited, keeping its line-ending style.
pub struct TextDoc {
    pub lines: Vec<String>,
    crlf: bool,
    trailing_newline: bool,
}

impl TextDoc {
    pub fn parse(data: &[u8]) -> TextDoc {
        let s = String::from_utf8_lossy(data);
        let s = s.trim_start_matches('\u{feff}');
        let crlf = s.contains("\r\n");
        let trailing_newline = s.ends_with('\n') || s.is_empty();
        let mut lines: Vec<String> = s.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect();
        if trailing_newline && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        TextDoc { lines, crlf, trailing_newline }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let nl = if self.crlf { "\r\n" } else { "\n" };
        let mut s = self.lines.join(nl);
        if self.trailing_newline && !self.lines.is_empty() {
            s.push_str(nl);
        }
        s.into_bytes()
    }

    fn contains_block(&self, block: &[String]) -> bool {
        if block.is_empty() {
            return true;
        }
        self.lines.windows(block.len()).any(|w| w.iter().zip(block).all(|(a, b)| a.trim() == b.trim()))
    }

    /// Apply one edit. Ok(note) or Err(why) for the log.
    pub fn apply(&mut self, edit: &TextEdit) -> std::result::Result<String, String> {
        let block = |c: &str| -> Vec<String> { c.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect() };
        match edit {
            TextEdit::Insert { after, line, condition, content } => {
                let b = block(content);
                if self.contains_block(&b) {
                    return Ok(format!("already present, skipped: {}", b[0].trim()));
                }
                let i = self
                    .lines
                    .iter()
                    .position(|l| line_matches(l, line, *condition))
                    .ok_or_else(|| format!("no line matches \"{line}\""))?;
                let at = if *after { i + 1 } else { i };
                let n = b.len();
                self.lines.splice(at..at, b);
                Ok(format!("inserted {n} line(s) {} line {}", if *after { "after" } else { "before" }, i + 1))
            }
            TextEdit::Replace { line, condition, content } => {
                let b = block(content);
                let mut n = 0;
                let mut i = 0;
                while i < self.lines.len() {
                    if line_matches(&self.lines[i], line, *condition) {
                        self.lines.splice(i..=i, b.clone());
                        i += b.len();
                        n += 1;
                    } else {
                        i += 1;
                    }
                }
                if n == 0 {
                    return Err(format!("no line matches \"{line}\""));
                }
                Ok(format!("replaced {n} line(s) matching \"{line}\""))
            }
            TextEdit::Add { content } => {
                let b = block(content);
                if self.contains_block(&b) {
                    return Ok(format!("already present, skipped: {}", b[0].trim()));
                }
                let n = b.len();
                self.lines.extend(b);
                Ok(format!("appended {n} line(s)"))
            }
            TextEdit::Delete { condition, line } => {
                let before = self.lines.len();
                self.lines.retain(|l| !line_matches(l, line, *condition));
                let n = before - self.lines.len();
                if n == 0 {
                    return Ok(format!("nothing to delete for \"{line}\""));
                }
                Ok(format!("deleted {n} line(s) matching \"{line}\""))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_and_line_endings() {
        let mut d = TextDoc::parse(b"cars\r\nadmiral, admiral\r\nend\r\n");
        d.apply(&TextEdit::Insert { after: false, line: "end".into(), condition: Condition::Equal, content: "mycar, mycar".into() }).unwrap();
        // Idempotent.
        assert!(d.apply(&TextEdit::Insert { after: false, line: "end".into(), condition: Condition::Equal, content: "mycar, mycar".into() }).unwrap().contains("skipped"));
        d.apply(&TextEdit::Replace { line: "admiral*".into(), condition: Condition::Mask, content: "admiral, admiral2".into() }).unwrap();
        d.apply(&TextEdit::Add { content: "# added".into() }).unwrap();
        d.apply(&TextEdit::Delete { condition: Condition::StartWith, line: "#".into() }).unwrap();
        assert_eq!(d.to_bytes(), b"cars\r\nadmiral, admiral2\r\nmycar, mycar\r\nend\r\n");
        assert!(d.apply(&TextEdit::Insert { after: true, line: "nope".into(), condition: Condition::Equal, content: "x".into() }).is_err());
    }

    #[test]
    fn conditions() {
        assert!(line_matches("  Hello World ", "hello world", Condition::Equal));
        assert!(line_matches("hello world", "HELLO", Condition::StartWith));
        assert!(line_matches("hello world", "WORLD", Condition::EndWith));
        assert!(line_matches("hello world", "lo wo", Condition::Contains));
        assert!(line_matches("hello world", "h*d", Condition::Mask));
        assert!(line_matches("hello world", "lo*or", Condition::Mask));
        assert!(!line_matches("hello world", "x*", Condition::Mask));
    }
}
