//! Format-neutral description of a board: its parts, their pins and the
//! nets those pins are connected to.
//!
//! Nothing here knows about .fz files or KiCad. The .fz reader fills these
//! structures in, and the KiCad writer reads them.
//!
//! Units: every coordinate and size is an f64 in MILS (thousandths of an
//! inch), exactly as stored in the source file. Conversion to millimetres
//! (using MIL_TO_MM) happens later, when writing output.

use std::collections::BTreeSet;

/// One mil in millimetres (1 mil = 0.001 inch = 0.0254 mm).
pub const MIL_TO_MM: f64 = 0.0254;

/// One pin (pad) of a part.
#[derive(Debug, Clone, PartialEq)]
pub struct Pin {
    /// Pad number, e.g. "1" or "H20" (becomes the KiCad pad number).
    pub number: String,
    /// Pin function/name; may equal `number` or be empty.
    pub name: String,
    /// Net name; empty string means no net.
    pub net: String,
    /// Board X position, mils.
    pub x: f64,
    /// Board Y position, mils (Y points up, as in the file).
    pub y: f64,
    /// Half the pad size, mils.
    pub radius: f64,
}

/// One component on the board.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    /// Reference designator, e.g. "C9208".
    pub refdes: String,
    /// Raw COMP_INSERTION_CODE, e.g. "1" or "2".
    pub insertion_code: String,
    /// Raw SYM_NAME (often empty).
    pub sym_name: String,
    /// True if SYM_MIRROR == "YES" (part sits on the bottom side).
    pub bottom: bool,
    /// SYM_ROTATE in degrees; None if the field was empty.
    pub rotation: Option<f64>,
    /// Part number from the descr block.
    pub part_number: Option<String>,
    /// Description from the descr block, e.g. "MLCC 12PF/50V(0201) NPO 5%".
    pub description: Option<String>,
    /// All pins of this part.
    pub pins: Vec<Pin>,
}

/// A whole board.
#[derive(Debug, Clone, Default)]
pub struct Board {
    /// All parts, in file order.
    pub parts: Vec<Part>,
    /// Problems found while parsing, kept to show the user.
    pub warnings: Vec<String>,
}

impl Board {
    /// All distinct, non-empty net names across all pins, sorted alphabetically.
    pub fn net_names(&self) -> Vec<String> {
        // A BTreeSet removes duplicates and keeps its items sorted.
        let mut names: BTreeSet<&str> = BTreeSet::new();
        for part in &self.parts {
            for pin in &part.pins {
                if !pin.net.is_empty() {
                    names.insert(pin.net.as_str());
                }
            }
        }
        names.into_iter().map(String::from).collect()
    }

    /// Total number of pins on the board.
    pub fn pin_count(&self) -> usize {
        self.parts.iter().map(|p| p.pins.len()).sum()
    }

    /// Find a part by its reference designator (exact match).
    pub fn part(&self, refdes: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.refdes == refdes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Make a pin with only the fields these tests care about.
    fn pin(number: &str, net: &str) -> Pin {
        Pin {
            number: number.to_string(),
            name: String::new(),
            net: net.to_string(),
            x: 0.0,
            y: 0.0,
            radius: 5.0,
        }
    }

    /// Make a part with the given pins and everything else left blank.
    fn part(refdes: &str, pins: Vec<Pin>) -> Part {
        Part {
            refdes: refdes.to_string(),
            insertion_code: "1".to_string(),
            sym_name: String::new(),
            bottom: false,
            rotation: None,
            part_number: None,
            description: None,
            pins,
        }
    }

    /// Two parts, four pins, nets "GND", "GND", "VCC" and "" (no net).
    fn sample_board() -> Board {
        Board {
            parts: vec![
                part("C1", vec![pin("1", "VCC"), pin("2", "GND")]),
                part("R1", vec![pin("1", "GND"), pin("2", "")]),
            ],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn net_names_are_unique_sorted_and_skip_empty() {
        assert_eq!(sample_board().net_names(), vec!["GND", "VCC"]);
    }

    #[test]
    fn pin_count_counts_all_pins() {
        assert_eq!(sample_board().pin_count(), 4);
    }

    #[test]
    fn part_lookup() {
        let board = sample_board();
        assert_eq!(board.part("C1").map(|p| p.refdes.as_str()), Some("C1"));
        assert!(board.part("X").is_none());
    }
}
