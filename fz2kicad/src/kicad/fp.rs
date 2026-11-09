//! Write footprints as KiCad 8 footprint files (.kicad_mod).
//!
//! The footprints are in mils with Y pointing up. KiCad uses millimetres
//! with Y pointing down, so every coordinate is converted and Y is negated.
//!
//! Every pad is written as a round SMD pad. Boardview files only give a pad
//! position and radius, so the real pad shapes and drill holes are unknown.

use std::fs;
use std::path::Path;

use crate::footprint::{Footprint, Library, Pad};
use crate::kicad::sexpr::{atom, list, num, string, SExpr};
use crate::model::MIL_TO_MM;

/// Smallest pad diameter we write, in mm. Pads with radius 0 still get a visible size.
const MIN_PAD_MM: f64 = 0.05;

/// Space between the pads and the courtyard outline, in mm.
const COURTYARD_MARGIN_MM: f64 = 0.25;

/// Convert a point from mils (Y up) to KiCad millimetres (Y down).
fn to_mm(x: f64, y: f64) -> (f64, f64) {
    (x * MIL_TO_MM, -y * MIL_TO_MM)
}

/// Pad diameter in mm (twice the radius), never smaller than MIN_PAD_MM.
fn diameter_mm(pad: &Pad) -> f64 {
    (2.0 * pad.radius * MIL_TO_MM).max(MIN_PAD_MM)
}

/// The standard text style: (effects (font (size 1 1) (thickness 0.15))).
fn text_effects() -> SExpr {
    list(
        "effects",
         vec![list(
             "font",
             vec![
                 list("size", vec![num(1.0), num(1.0)]),
                   list("thickness", vec![num(0.15)]),
             ],
         )],
    )
}

/// One footprint property, e.g. the "Reference" text.
fn property(name: &str, value: &str, y: f64, layer: &str) -> SExpr {
    list(
        "property",
         vec![
             string(name),
         string(value),
         list("at", vec![num(0.0), num(y)]),
         list("layer", vec![string(layer)]),
         text_effects(),
         ],
    )
}

/// One round SMD pad.
fn pad_sexpr(pad: &Pad) -> SExpr {
    let (x, y) = to_mm(pad.x, pad.y);
    let d = diameter_mm(pad);
    list(
        "pad",
         vec![
             string(&pad.number),
         atom("smd"),
         atom("circle"),
         list("at", vec![num(x), num(y)]),
         list("size", vec![num(d), num(d)]),
         list(
             "layers",
              vec![string("F.Cu"), string("F.Paste"), string("F.Mask")],
         ),
         ],
    )
}

/// Bounding box of all pad circles in mm, grown by the courtyard margin.
/// Returns (x1, y1, x2, y2) with (x1, y1) as the minimum corner.
fn courtyard(pads: &[Pad]) -> (f64, f64, f64, f64) {
    let mut x1 = f64::INFINITY;
    let mut y1 = f64::INFINITY;
    let mut x2 = f64::NEG_INFINITY;
    let mut y2 = f64::NEG_INFINITY;

    for pad in pads {
        let (x, y) = to_mm(pad.x, pad.y);
        let r = diameter_mm(pad) / 2.0;
        x1 = x1.min(x - r);
        y1 = y1.min(y - r);
        x2 = x2.max(x + r);
        y2 = y2.max(y + r);
    }

    // No pads (shouldn't happen): use a box around the origin.
    if pads.is_empty() {
        (x1, y1, x2, y2) = (0.0, 0.0, 0.0, 0.0);
    }

    let m = COURTYARD_MARGIN_MM;
    (x1 - m, y1 - m, x2 + m, y2 + m)
}

/// Build the complete S-expression for one footprint.
pub fn footprint_sexpr(fp: &Footprint) -> SExpr {
    let name = fp.id.to_string();
    let (x1, y1, x2, y2) = courtyard(&fp.pads);

    let descr = format!(
        "e.g. {}, {}, used {}x",
        fp.example_refdes,
        fp.example_description.as_deref().unwrap_or("no description"),
                        fp.count
    );

    let mut children = vec![
        string(&name),
        list("version", vec![num(20240108.0)]),
        list("generator", vec![string("fz2kicad")]),
        list("layer", vec![string("F.Cu")]),
        list("descr", vec![string(&descr)]),
        // Reference text above the courtyard, value text below it.
        property("Reference", "REF**", y1 - 1.0, "F.SilkS"),
        property("Value", &name, y2 + 1.0, "F.Fab"),
        list("attr", vec![atom("smd")]),
        // Courtyard rectangle: the area the part occupies.
        list(
            "fp_rect",
             vec![
                 list("start", vec![num(x1), num(y1)]),
             list("end", vec![num(x2), num(y2)]),
             list(
                 "stroke",
                  vec![
                      list("width", vec![num(0.05)]),
                  list("type", vec![atom("default")]),
                  ],
             ),
             list("fill", vec![atom("none")]),
             list("layer", vec![string("F.CrtYd")]),
             ],
        ),
    ];

    // One pad per footprint pad, in order.
    children.extend(fp.pads.iter().map(pad_sexpr));

    list("footprint", children)
}

/// Write every footprint to `<dir>/<id>.kicad_mod`. Returns how many were written.
pub fn write_library(lib: &Library, dir: &Path) -> Result<usize, String> {
    fs::create_dir_all(dir)
    .map_err(|e| format!("cannot create folder {}: {}", dir.display(), e))?;

    for fp in &lib.footprints {
        let path = dir.join(format!("{}.kicad_mod", fp.id));
        let text = format!("{}\n", footprint_sexpr(fp).to_pretty());
        fs::write(&path, text).map_err(|e| format!("cannot write {}: {}", path.display(), e))?;
    }

    Ok(lib.footprints.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(number: &str, x: f64, y: f64, radius: f64) -> Pad {
        Pad {
            number: number.to_string(),
                                                                                       x,
                                                                                       y,
                                                                                       radius,
        }
    }

    fn footprint(pads: Vec<Pad>, description: Option<&str>) -> Footprint {
        Footprint {
            id: 1,
            pads,
            through_hole: false,
            example_refdes: "C1".to_string(),
                                                                                       example_description: description.map(String::from),
                                                                                       count: 1,
        }
    }

    /// The two-pad footprint used by several tests.
    fn two_pad_text() -> String {
        let fp = footprint(
            vec![pad("1", -8.86, 0.0, 4.92), pad("2", 8.86, 0.0, 4.92)],
                           None,
        );
        footprint_sexpr(&fp).to_pretty()
    }

    #[test]
    fn two_pad_footprint() {
        let text = two_pad_text();
        for expected in [
            "(footprint \"1\"",
            "(version 20240108)",
            "(pad \"1\" smd circle",
            "(at -0.225044 0)",
            "(size 0.249936 0.249936)",
            "(layers \"F.Cu\" \"F.Paste\" \"F.Mask\")",
        ] {
            assert!(text.contains(expected), "missing {:?} in:\n{}", expected, text);
        }
    }

    #[test]
    fn y_is_flipped() {
        let fp = footprint(vec![pad("1", 0.0, 10.0, 4.0)], None);
        let text = footprint_sexpr(&fp).to_pretty();
        assert!(text.contains("(at 0 -0.254)"), "text:\n{}", text);
    }

    #[test]
    fn courtyard_surrounds_pads() {
        let text = two_pad_text();
        assert!(text.contains("(start -0.600012 -0.374968)"), "text:\n{}", text);
        assert!(text.contains("(end 0.600012 0.374968)"), "text:\n{}", text);
    }

    #[test]
    fn description_is_escaped() {
        let fp = footprint(vec![pad("1", 0.0, 0.0, 4.0)], Some("CAP \"big\""));
        let text = footprint_sexpr(&fp).to_pretty();
        assert!(text.contains("CAP \\\"big\\\""), "text:\n{}", text);
    }

    /// Run with: cargo test kicad::fp::tests::real_file -- --ignored --nocapture
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

        let board = crate::fz::parse::parse(&content, &descr);
        let lib = crate::footprint::build(&board);
        let dir = Path::new("out/fz_board.pretty");
        let written = write_library(&lib, dir).expect("writing the library failed");
        println!("wrote {} footprints to {}", written, dir.display());
    }
}
