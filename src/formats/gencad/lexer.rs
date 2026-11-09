//! Line and section lexer for GenCAD (.cad) files.
//!
//! A GenCAD file is a list of sections such as `$HEADER ... $ENDHEADER`.
//! Each line inside a section is a keyword followed by parameters separated
//! by spaces or tabs. A parameter may be a double-quoted string containing
//! spaces; inside quotes, `""` stands for one literal quote.

/// One line inside a section.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// 1-based line number in the file.
    pub number: usize,
    /// First token, uppercased.
    pub keyword: String,
    /// Remaining tokens, quotes removed.
    pub args: Vec<String>,
}

/// One `$NAME ... $ENDNAME` block.
#[derive(Debug, Clone)]
pub struct Section {
    /// Section name, uppercase, without the `$` (e.g. "SHAPES").
    pub name: String,
    pub lines: Vec<Line>,
}

impl Section {
    /// First line with the given keyword (case-insensitive).
    pub fn find(&self, keyword: &str) -> Option<&Line> {
        self.lines
        .iter()
        .find(|l| l.keyword.eq_ignore_ascii_case(keyword))
    }
}

/// Bytes to text: UTF-8 if valid, otherwise Latin-1. A leading UTF-8 BOM is removed.
pub fn to_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// Splits a line on spaces/tabs, keeping quoted strings together.
/// Quotes are removed, `""` inside quotes becomes `"`, and an unterminated
/// quote runs to the end of the line.
pub fn tokenize(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false; // so that "" yields an empty token
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    current.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(c);
            }
        } else if c == ' ' || c == '\t' {
            if in_token {
                tokens.push(std::mem::take(&mut current));
                in_token = false;
            }
        } else if c == '"' {
            in_quotes = true;
            in_token = true;
        } else {
            current.push(c);
            in_token = true;
        }
    }
    if in_token {
        tokens.push(current);
    }
    tokens
}

/// Splits the whole file into sections.
pub fn sections(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut current: Option<Section> = None;

    for (i, raw) in text.split('\n').enumerate() {
        let number = i + 1;
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with('$') {
            let word = line.split_whitespace().next().unwrap_or("").to_uppercase();
            // Either "$END..." or a new section: both close the open one.
            if let Some(s) = current.take() {
                out.push(s);
            }
            if !word.starts_with("$END") {
                current = Some(Section {
                    name: word[1..].to_string(),
                               lines: Vec::new(),
                });
            }
            continue;
        }

        if let Some(s) = current.as_mut() {
            let mut tokens = tokenize(line).into_iter();
            if let Some(first) = tokens.next() {
                s.lines.push(Line {
                    number,
                    keyword: first.to_uppercase(),
                             args: tokens.collect(),
                });
            }
        }
        // Lines outside any section are ignored.
    }

    if let Some(s) = current.take() {
        out.push(s);
    }
    out
}

/// First section with the given name (case-insensitive, without `$`).
pub fn section<'a>(all: &'a [Section], name: &str) -> Option<&'a Section> {
    all.iter().find(|s| s.name.eq_ignore_ascii_case(name))
}

/// Parses a number, ignoring surrounding whitespace.
pub fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_keeps_quoted_strings() {
        assert_eq!(
            tokenize(r#"COMPONENT "U 12"  PLACE"#),
                   vec!["COMPONENT", "U 12", "PLACE"]
        );
        assert_eq!(tokenize("A\tB  C"), vec!["A", "B", "C"]);
        assert_eq!(tokenize(r#"X """#), vec!["X", ""]);
        assert_eq!(tokenize(r#"X "open end"#), vec!["X", "open end"]);
        assert!(tokenize("   ").is_empty());
    }

    #[test]
    fn tokenize_doubled_quote() {
        assert_eq!(tokenize(r#"DEVICE "a""b" x"#), vec!["DEVICE", "a\"b", "x"]);
    }

    #[test]
    fn splits_sections_with_crlf() {
        let text = "$HEADER\r\n\
GENCAD 1.4\r\n\
UNITS INCH\r\n\
$ENDHEADER\r\n\
\r\n\
$SHAPES\r\n\
SHAPE \"R 0402\"\r\n\
PIN 1 PAD1 -0.02 0 TOP 0 0\r\n\
$ENDSHAPES\r\n";
let all = sections(text);
assert_eq!(all.len(), 2);
assert_eq!(all[0].name, "HEADER");
assert_eq!(all[1].name, "SHAPES");

let header = section(&all, "header").unwrap();
let units = header.find("UNITS").unwrap();
assert_eq!(units.args, vec!["INCH"]);
assert_eq!(units.number, 3);
assert_eq!(header.find("GENCAD").unwrap().number, 2);

let shapes = section(&all, "SHAPES").unwrap();
assert_eq!(shapes.lines.len(), 2);
assert_eq!(shapes.lines[0].keyword, "SHAPE");
assert_eq!(shapes.lines[0].args, vec!["R 0402"]);
assert_eq!(shapes.lines[0].number, 7);
assert_eq!(shapes.lines[1].keyword, "PIN");
assert_eq!(shapes.lines[1].number, 8);
assert_eq!(num(&shapes.lines[1].args[2]), Some(-0.02));
    }

    #[test]
    fn unclosed_section_is_closed_by_next_marker() {
        let text = "stray line\n$Pads\nPAD P1 ROUND\n$Shapes\nshape S1\n$ENDSHAPES\n";
        let all = sections(text);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "PADS");
        assert_eq!(all[0].lines.len(), 1);
        assert_eq!(all[1].name, "SHAPES");
        assert_eq!(all[1].lines[0].keyword, "SHAPE");
        assert_eq!(all[1].lines[0].args, vec!["S1"]);
    }

    #[test]
    fn to_text_bom_and_latin1() {
        assert_eq!(to_text(b"\xEF\xBB\xBFabc"), "abc");
        assert_eq!(to_text(&[b'x', 0xE9, b'y']), "x\u{e9}y");
        assert_eq!(to_text("é".as_bytes()), "é");
    }

    #[test]
    fn num_parses_trimmed() {
        assert_eq!(num(" 1.5 "), Some(1.5));
        assert_eq!(num("-2"), Some(-2.0));
        assert_eq!(num("abc"), None);
    }
}
