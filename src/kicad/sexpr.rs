//! A tiny builder and printer for KiCad S-expressions.
//!
//! KiCad files are nested lists in parentheses, for example:
//!
//! ```text
//! (pad "1" smd circle
//!   (at 0.225 0)
//!   (size 0.25 0.25)
//! )
//! ```
//!
//! Build a tree with `list`, `atom`, `string` and `num`, then print it with
//! `to_pretty()` (or `{}` formatting).

use std::fmt;

/// One element of a KiCad S-expression.
#[derive(Debug, Clone, PartialEq)]
pub enum SExpr {
    /// Printed as-is: keywords and symbols like smd, circle, yes.
    Atom(String),
    /// Printed in double quotes, escaped.
    Str(String),
    /// A length in mm (or a plain number), printed with fmt_num (rounded to 0.001).
    Num(f64),
    /// An angle in degrees, printed with fmt_deg (up to 6 decimals, not rounded to 0.001).
    Deg(f64),
    /// ( ... )
    List(Vec<SExpr>),
}

/// A bare word, e.g. `smd`.
pub fn atom(s: &str) -> SExpr {
    SExpr::Atom(s.to_string())
}

/// A quoted string, e.g. `"C9208"`.
pub fn string(s: &str) -> SExpr {
    SExpr::Str(s.to_string())
}

/// A number, e.g. `0.25`.
pub fn num(v: f64) -> SExpr {
    SExpr::Num(v)
}

/// An angle in degrees, e.g. `90`.
pub fn deg(v: f64) -> SExpr {
    SExpr::Deg(v)
}

/// A list whose first element is the keyword `name`, e.g. `(at 1 2)`.
pub fn list(name: &str, children: Vec<SExpr>) -> SExpr {
    let mut items = Vec::with_capacity(children.len() + 1);
    items.push(atom(name));
    items.extend(children);
    SExpr::List(items)
}

/// Format a length in mm: rounded half away from zero to 0.001 mm, at most
/// 3 decimals, no trailing zeros or '.', and never "-0".
pub fn fmt_num(v: f64) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    // -0.0 (e.g. from -0.0004) equals 0.0: print plain "0".
    if r == 0.0 {
        return "0".to_string();
    }
    let s = format!("{:.3}", r);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Format an angle: at most 6 decimals, no trailing zeros, no trailing '.', never "-0".
pub fn fmt_deg(v: f64) -> String {
    let mut s = format!("{:.6}", v);
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

/// Make text safe to put inside double quotes in a KiCad file.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => {} // drop carriage returns (Windows line endings)
            _ => out.push(c),
        }
    }
    out
}

impl SExpr {
    /// Print the whole tree with KiCad-style indentation (no final newline).
    pub fn to_pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }

    /// Append this element to `out`. The caller has already written the
    /// indentation for the first line; `indent` is this element's depth in spaces.
    fn write(&self, out: &mut String, indent: usize) {
        match self {
            SExpr::Atom(s) => out.push_str(s),
            SExpr::Str(s) => {
                out.push('"');
                out.push_str(&escape(s));
                out.push('"');
            }
            SExpr::Num(v) => out.push_str(&fmt_num(*v)),
            SExpr::Deg(v) => out.push_str(&fmt_deg(*v)),
            SExpr::List(children) => {
                let has_list = children.iter().any(|c| matches!(c, SExpr::List(_)));

                if !has_list {
                    // Simple list: everything on one line, e.g. (at 1.5 -2 90).
                    out.push('(');
                    for (i, child) in children.iter().enumerate() {
                        if i > 0 {
                            out.push(' ');
                        }
                        child.write(out, indent);
                    }
                    out.push(')');
                    return;
                }

                // The leading non-list children go on the first line, e.g. (pad "1" smd circle.
                let leading = children
                .iter()
                .take_while(|c| !matches!(c, SExpr::List(_)))
                .count();
                out.push('(');
                for (i, child) in children[..leading].iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    child.write(out, indent);
                }

                // Every remaining child on its own line, 2 spaces deeper.
                let inner = indent + 2;
                for child in &children[leading..] {
                    out.push('\n');
                    push_spaces(out, inner);
                    child.write(out, inner);
                }

                // Closing parenthesis on its own line, at this list's depth.
                out.push('\n');
                push_spaces(out, indent);
                out.push(')');
            }
        }
    }
}

/// Append `n` spaces.
fn push_spaces(out: &mut String, n: usize) {
    out.extend(std::iter::repeat(' ').take(n));
}

impl fmt::Display for SExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_pretty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(fmt_num(1.0), "1");
        assert_eq!(fmt_num(0.45000000001), "0.45");
        assert_eq!(fmt_num(-0.0), "0");
        assert_eq!(fmt_num(-1.25), "-1.25");
        assert_eq!(fmt_num(0.0000001), "0");
        assert_eq!(fmt_num(1e6), "1000000");
    }

    #[test]
    fn escaping() {
        assert_eq!(escape("a\"b\\c"), "a\\\"b\\\\c");
        assert_eq!(escape("x\r\ny"), "x\\ny");
    }

    #[test]
    fn inline_list() {
        let e = list("at", vec![num(1.5), num(-2.0), num(90.0)]);
        assert_eq!(e.to_pretty(), "(at 1.5 -2 90)");
    }

    #[test]
    fn lengths_round_to_micrometres() {
        assert_eq!(fmt_num(0.7500004), "0.75");
        assert_eq!(fmt_num(-0.0004), "0");
        assert_eq!(fmt_num(1.404149), "1.404");
        assert_eq!(fmt_num(55.28133936 * 0.0254), "1.404");
    }

    #[test]
    fn angles_are_not_rounded_to_micrometres() {
        assert_eq!(fmt_deg(90.0), "90");
        assert_eq!(fmt_deg(12.3456789), "12.345679");
        let e = list("at", vec![num(1.0), num(2.0), deg(90.0)]);
        assert_eq!(e.to_pretty(), "(at 1 2 90)");
    }

    #[test]
    fn nested_list() {
        let e = list(
            "pad",
            vec![
                string("1"),
                     atom("smd"),
                     atom("circle"),
                     list("at", vec![num(0.225), num(0.0)]),
                     list("size", vec![num(0.25), num(0.25)]),
            ],
        );
        assert_eq!(
            e.to_pretty(),
                   "(pad \"1\" smd circle\n  (at 0.225 0)\n  (size 0.25 0.25)\n)"
        );
    }

    #[test]
    fn deeper_nesting_indents_each_level() {
        let e = list(
            "footprint",
            vec![
                string("R"),
                     list("pad", vec![string("1"), list("at", vec![num(0.0), num(0.0)])]),
            ],
        );
        assert_eq!(
            e.to_pretty(),
                   "(footprint \"R\"\n  (pad \"1\"\n    (at 0 0)\n  )\n)"
        );
    }
}
