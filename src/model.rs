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

/// The outline of a pad.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PadKind {
    Circle,
    Rect,
    RoundRect,
    Oval,
}

/// Pad geometry in MILS. `angle` is the pad's rotation in degrees,
/// counter-clockwise, in the same frame as the pin coordinates (board
/// frame, Y up).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PadGeom {
    pub kind: PadKind,
    pub w: f64,
    pub h: f64,
    pub angle: f64,
}

impl PadGeom {
    /// A round pad of the given diameter.
    pub fn circle(diameter: f64) -> Self {
        PadGeom {
            kind: PadKind::Circle,
            w: diameter,
            h: diameter,
            angle: 0.0,
        }
    }

    /// Same pad, described in a standard way so equal pads compare equal:
    /// - Circle: w = h = max(w, h), angle 0
    /// - others: angle reduced into [0, 180) (all four kinds look the same
    ///   after a 180 degree turn); if the angle is within 0.01 of 90, swap
    ///   w and h and set angle 0; if within 0.01 of 0 or 180 -> 0;
    ///   a Rect/RoundRect/Oval with w == h (within 0.001) and an angle that
    ///   is a multiple of 90 -> angle 0.
    pub fn normalized(self) -> Self {
        const ANGLE_TOL: f64 = 0.01;
        const SIZE_TOL: f64 = 0.001;

        if self.kind == PadKind::Circle {
            let d = self.w.max(self.h);
            return PadGeom::circle(d);
        }

        let mut w = self.w;
        let mut h = self.h;
        let mut a = self.angle.rem_euclid(180.0);

        if (a - 90.0).abs() <= ANGLE_TOL {
            std::mem::swap(&mut w, &mut h);
            a = 0.0;
        } else if a <= ANGLE_TOL || a >= 180.0 - ANGLE_TOL {
            // Also catches rem_euclid returning 180.0 for tiny negative angles.
            a = 0.0;
        }

        // A square looks the same at every multiple of 90 degrees. (After
        // the steps above such angles are already 0; kept for clarity.)
        if (w - h).abs() <= SIZE_TOL {
            let q = a / 90.0;
            if (q - q.round()).abs() * 90.0 <= ANGLE_TOL {
                a = 0.0;
            }
        }

        PadGeom {
            kind: self.kind,
            w,
            h,
            angle: a,
        }
    }

    /// Rotated by `deg` (counter-clockwise), then normalized.
    pub fn rotated(self, deg: f64) -> Self {
        PadGeom {
            angle: self.angle + deg,
            ..self
        }
        .normalized()
    }

    /// Mirrored left-right (x -> -x): angle -> -angle, then normalized.
    pub fn mirrored_x(self) -> Self {
        PadGeom {
            angle: -self.angle,
            ..self
        }
        .normalized()
    }
}

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
    /// Real pad shape, if the source format has one.
    /// None = round pad of diameter 2 * radius.
    pub pad: Option<PadGeom>,
}

impl Pin {
    /// The pad shape to draw: `pad` if known, else a circle of diameter 2 * radius.
    pub fn pad_geom(&self) -> PadGeom {
        self.pad.unwrap_or_else(|| PadGeom::circle(2.0 * self.radius))
    }
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
            pad: None,
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

    fn rect(w: f64, h: f64, angle: f64) -> PadGeom {
        PadGeom {
            kind: PadKind::Rect,
            w,
            h,
            angle,
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

    #[test]
    fn rect_at_90_swaps_sides() {
        assert_eq!(rect(20.0, 10.0, 90.0).normalized(), rect(10.0, 20.0, 0.0));
    }

    #[test]
    fn rect_at_270_and_180() {
        assert_eq!(rect(20.0, 10.0, 270.0).normalized(), rect(10.0, 20.0, 0.0));
        assert_eq!(rect(20.0, 10.0, 180.0).normalized(), rect(20.0, 10.0, 0.0));
        assert_eq!(rect(20.0, 10.0, -90.0).normalized(), rect(10.0, 20.0, 0.0));
        assert_eq!(rect(20.0, 10.0, 90.005).normalized(), rect(10.0, 20.0, 0.0));
    }

    #[test]
    fn mirrored_x_negates_angle() {
        let m = rect(20.0, 10.0, 30.0).mirrored_x();
        assert_eq!((m.w, m.h), (20.0, 10.0));
        assert!((m.angle - 150.0).abs() < 1e-9, "{}", m.angle);
    }

    #[test]
    fn circle_ignores_angle() {
        let c = PadGeom::circle(12.0);
        assert_eq!((c.kind, c.w, c.h, c.angle), (PadKind::Circle, 12.0, 12.0, 0.0));

        let tilted = PadGeom {
            kind: PadKind::Circle,
            w: 12.0,
            h: 10.0,
            angle: 45.0,
        };
        assert_eq!(tilted.normalized(), PadGeom::circle(12.0));
    }

    #[test]
    fn rotated_by_90() {
        assert_eq!(rect(20.0, 10.0, 0.0).rotated(90.0), rect(10.0, 20.0, 0.0));
    }

    #[test]
    fn square_and_odd_angles() {
        // A square at a multiple of 90 is angle 0.
        assert_eq!(rect(15.0, 15.0, 270.0).normalized(), rect(15.0, 15.0, 0.0));
        // A non-right angle is kept.
        assert_eq!(rect(20.0, 10.0, 45.0).normalized(), rect(20.0, 10.0, 45.0));
    }

    #[test]
    fn pad_geom_falls_back_to_circle() {
        let p = pin("1", "");
        assert_eq!(p.pad_geom(), PadGeom::circle(10.0));
    }
}
