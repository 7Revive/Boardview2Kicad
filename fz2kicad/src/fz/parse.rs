//! Parse the two decompressed blocks of an ASUS .fz file into a Board.
//!
//! - content: '!'-separated text with a parts section and a pins section.
//!   "A!" lines are column headers, "S!" lines are data rows.
//! - descr: tab-separated bill of materials (part number, description,
//!   quantity, list of refdes).
//!
//! Everything is stored in mils. If the file says "UNIT:millimeters",
//! coordinates and radii are converted to mils here.

use std::collections::{HashMap, HashSet};

use crate::model::{Board, Part, Pin};

/// Multiply a millimetre value by this to get mils.
const MM_TO_MILS: f64 = 1000.0 / 25.4;

/// Which kind of section the current "A!" header started.
enum Section {
    Parts,
    Pins,
    Unknown,
}

/// Parse both blocks into a Board. Never fails: problems become warnings.
pub fn parse(content: &[u8], descr: &[u8]) -> Board {
    let mut board = Board::default();
    // refdes -> position in board.parts, for fast lookup.
    let mut index: HashMap<String, usize> = HashMap::new();

    parse_content(&to_text(content), &mut board, &mut index);
    parse_descr(&to_text(descr), &mut board, &index);

    board
}

/// Turn bytes into text: UTF-8 if valid, otherwise Latin-1 (one byte = one character).
fn to_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// Split text into trimmed, non-empty lines.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n').map(str::trim).filter(|l| !l.is_empty())
}

/// Read a number: trims spaces, accepts ',' as the decimal point.
/// Returns None for empty or invalid text.
fn num(s: &str) -> Option<f64> {
    s.trim()
    .replace(',', ".")
    .parse::<f64>()
    .ok()
    .filter(|v| v.is_finite())
}

/// Look up a field in a data row by its column name.
/// Returns "" if the column doesn't exist or the row is too short.
fn get<'a>(cols: &[String], fields: &[&'a str], name: &str) -> &'a str {
    cols.iter()
    .position(|c| c == name)
    .and_then(|i| fields.get(i).copied())
    .map(str::trim)
    .unwrap_or("")
}

/// Parse the content block (parts and pins).
fn parse_content(text: &str, board: &mut Board, index: &mut HashMap<String, usize>) {
    let mut unit = 1.0; // multiplier to mils; 1.0 means the file is already in mils
    let mut section: Option<Section> = None; // None until the first header
    let mut cols: Vec<String> = Vec::new(); // column names of the current section
    let mut warned_sections: HashSet<String> = HashSet::new();

    for line in lines(text) {
        if let Some(value) = line.strip_prefix("UNIT:") {
            // Unit switch: millimetres need converting, anything else is treated as mils.
            unit = if value.trim().eq_ignore_ascii_case("millimeters") {
                MM_TO_MILS
            } else {
                1.0
            };
        } else if let Some(rest) = line.strip_prefix("A!") {
            // New section header: read the column names.
            cols = rest.split('!').map(|c| c.trim().to_string()).collect();
            if cols.last().is_some_and(|c| c.is_empty()) {
                cols.pop(); // the line ends with '!', leaving an empty last entry
            }

            let first = cols.first().map(String::as_str).unwrap_or("");
            section = Some(match first {
                "REFDES" => Section::Parts,
                "NET_NAME" => Section::Pins,
                _ => {
                    // Warn once per unknown section name.
                    if warned_sections.insert(first.to_string()) {
                        board
                        .warnings
                        .push(format!("unknown section '{}' ignored", first));
                    }
                    Section::Unknown
                }
            });
        } else if let Some(rest) = line.strip_prefix("S!") {
            // Data row: split into fields and hand it to the right handler.
            let fields: Vec<&str> = rest.split('!').collect();
            match section {
                Some(Section::Parts) => parse_part_row(&cols, &fields, board, index),
                Some(Section::Pins) => parse_pin_row(&cols, &fields, unit, board, index),
                // Unknown section, or a row before any header: ignore.
                _ => {}
            }
        }
        // Any other line: ignore.
    }
}

/// Handle one row of the parts section.
fn parse_part_row(
    cols: &[String],
    fields: &[&str],
    board: &mut Board,
    index: &mut HashMap<String, usize>,
) {
    let refdes = get(cols, fields, "REFDES");
    if refdes.is_empty() {
        board
        .warnings
        .push("part with empty REFDES skipped".to_string());
        return;
    }
    if index.contains_key(refdes) {
        board
        .warnings
        .push(format!("duplicate part {}, keeping the first", refdes));
        return;
    }

    // Rotation: empty means "not given"; text that isn't a number gets a warning.
    let rotate_text = get(cols, fields, "SYM_ROTATE");
    let rotation = num(rotate_text);
    if rotation.is_none() && !rotate_text.is_empty() {
        board.warnings.push(format!(
            "part {}: rotation '{}' is not a number, ignored",
            refdes, rotate_text
        ));
    }

    index.insert(refdes.to_string(), board.parts.len());
    board.parts.push(Part {
        refdes: refdes.to_string(),
                     insertion_code: get(cols, fields, "COMP_INSERTION_CODE").to_string(),
                     sym_name: get(cols, fields, "SYM_NAME").to_string(),
                     bottom: get(cols, fields, "SYM_MIRROR") == "YES",
                     rotation,
                     part_number: None,
                     description: None,
                     pins: Vec::new(),
    });
}

/// Handle one row of the pins section.
fn parse_pin_row(
    cols: &[String],
    fields: &[&str],
    unit: f64,
    board: &mut Board,
    index: &HashMap<String, usize>,
) {
    let refdes = get(cols, fields, "REFDES");
    let pin_number = get(cols, fields, "PIN_NUMBER");
    let pin_name = get(cols, fields, "PIN_NAME");

    // Pad number: PIN_NUMBER, unless it's empty or "0", then the pin name.
    let number = if pin_number.is_empty() || pin_number == "0" {
        pin_name
    } else {
        pin_number
    };

    // The part must already exist.
    let Some(&part_index) = index.get(refdes) else {
        board.warnings.push(format!(
            "pin of unknown part {} (pin {}), skipped",
                                    refdes, number
        ));
        return;
    };

    // Position must be numeric.
    let x_text = get(cols, fields, "PIN_X");
    let y_text = get(cols, fields, "PIN_Y");
    let (Some(x), Some(y)) = (num(x_text), num(y_text)) else {
        board.warnings.push(format!(
            "part {} pin {}: position ('{}', '{}') is not a number, skipped",
                                    refdes, number, x_text, y_text
        ));
        return;
    };

    // Radius: empty means 0; text that isn't a number also becomes 0, with a warning.
    let radius_text = get(cols, fields, "RADIUS");
    let radius = num(radius_text).unwrap_or_else(|| {
        if !radius_text.is_empty() {
            board.warnings.push(format!(
                "part {} pin {}: radius '{}' is not a number, using 0",
                refdes, number, radius_text
            ));
        }
        0.0
    });

    board.parts[part_index].pins.push(Pin {
        number: number.to_string(),
                                      name: pin_name.to_string(),
                                      net: get(cols, fields, "NET_NAME").to_string(),
                                      x: x * unit,
                                      y: y * unit,
                                      radius: radius * unit,
    });
}

/// Parse the descr block and attach part numbers and descriptions to parts.
fn parse_descr(text: &str, board: &mut Board, index: &HashMap<String, usize>) {
    for line in text.split('\n') {
        // Only strip line endings here, not tabs or spaces: an empty first
        // column starts with a tab, and trimming it would shift every column.
        let line = line.trim_end_matches(['\r', '\n']);

        // Skip the header line and short lines (e.g. a board description line).
        if line.starts_with("PARTNUMBER") {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 4 {
            continue;
        }

        let part_number = fields[0].trim();
        let description = fields[1].trim();

        // Column 3 lists every refdes that uses this part number.
        for refdes in fields[3].split_whitespace() {
            // Unknown refdes are common and harmless: ignore silently.
            let Some(&i) = index.get(refdes) else {
                continue;
            };
            let part = &mut board.parts[i];

            if let Some(existing) = &part.description {
                board.warnings.push(format!(
                    "{} appears more than once in the descr block, keeping \"{}\"",
                    refdes, existing
                ));
                continue;
            }

            part.part_number = (!part_number.is_empty()).then(|| part_number.to_string());
            part.description = (!description.is_empty()).then(|| description.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTENT: &str = "A!REFDES!COMP_INSERTION_CODE!SYM_NAME!SYM_MIRROR!SYM_ROTATE!
    S!C1!1!!NO!!\r\n\
    S!U1!1!!YES!90!\r\n\
    A!NET_NAME!REFDES!PIN_NUMBER!PIN_NAME!PIN_X!PIN_Y!TEST_POINT!RADIUS!\r\n\
    S!GND!C1!1!1!-730!-959.86!!4.92!\r\n\
    S!VCC!C1!2!2!-730!-942,14!!4.92!\r\n\
    S!SIG!U1!0!AJ31!10!20!!6!\r\n\
    S!X!NOPE!1!1!0!0!!1!\r\n";

    /// Find a pin by part and pad number, or fail the test.
    fn pin<'a>(board: &'a Board, refdes: &str, number: &str) -> &'a Pin {
        board
        .part(refdes)
        .unwrap_or_else(|| panic!("part {} missing", refdes))
        .pins
        .iter()
        .find(|p| p.number == number)
        .unwrap_or_else(|| panic!("pin {}.{} missing", refdes, number))
    }

    #[test]
    fn parses_parts_and_pins() {
        let board = parse(CONTENT.as_bytes(), b"");

        assert_eq!(board.parts.len(), 2);
        assert_eq!(board.part("C1").unwrap().pins.len(), 2);
        assert_eq!(pin(&board, "C1", "2").y, -942.14);

        let u1 = board.part("U1").unwrap();
        assert!(u1.bottom);
        assert_eq!(u1.rotation, Some(90.0));
        assert_eq!(u1.pins[0].number, "AJ31");

        let nope = board.warnings.iter().filter(|w| w.contains("NOPE")).count();
        assert_eq!(nope, 1, "warnings: {:?}", board.warnings);
    }

    #[test]
    fn converts_millimeters_to_mils() {
        let text = format!("UNIT:millimeters\n{}", CONTENT);
        let board = parse(text.as_bytes(), b"");

        let expected = -730.0 * 1000.0 / 25.4;
        let x = pin(&board, "C1", "1").x;
        assert!((x - expected).abs() < 1e-6, "x = {}, expected {}", x, expected);
    }

    #[test]
    fn attaches_descriptions() {
        let descr = "PARTNUMBER DESCRIPTION QTY LOCATION PARTNUMBER2\r\n\
P1\tCAP 10UF (0402)\t2\tC1 X9 \t\r\n";
        let board = parse(CONTENT.as_bytes(), descr.as_bytes());

        let c1 = board.part("C1").unwrap();
        assert_eq!(c1.part_number.as_deref(), Some("P1"));
        assert_eq!(c1.description.as_deref(), Some("CAP 10UF (0402)"));
        assert!(
            !board.warnings.iter().any(|w| w.contains("X9")),
                "warnings: {:?}",
                board.warnings
        );
    }

    /// Run with: cargo test real_file -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_file() {
        let (Ok(content), Ok(descr)) = (
            std::fs::read("out/content.txt"),
                                        std::fs::read("out/descr.txt"),
        ) else {
            println!("out/content.txt or out/descr.txt not found; run `fz2kicad dump` first");
            return;
        };

        let board = parse(&content, &descr);

        println!("parts:    {}", board.parts.len());
        println!("pins:     {}", board.pin_count());
        println!("nets:     {}", board.net_names().len());
        println!("warnings: {}", board.warnings.len());
        for w in board.warnings.iter().take(10) {
            println!("  {}", w);
        }

        for refdes in ["C9208", "VY5000"] {
            match board.part(refdes) {
                Some(part) => println!("{:#?}", part),
                                                                                       None => println!("{} not found", refdes),
            }
        }
    }
}
