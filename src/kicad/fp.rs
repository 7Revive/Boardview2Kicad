//! Write footprints as KiCad 8 footprint files (.kicad_mod).
//!
//! The footprints are in mils with Y pointing up. KiCad uses millimetres
//! with Y pointing down, so every coordinate is converted and Y is negated.
//!
//! Pads with a known shape (`Pad::geom`) are written as circle, rect,
//! roundrect or oval pads with their real size and angle. Pads without one
//! are written as round pads of diameter 2 * radius. Through-hole footprints
//! get a drill hole sized from the pad, because boardview files don't store
//! the real drill size.

use std::fs;
use std::path::Path;

use crate::footprint::{Footprint, Library, Pad};
use crate::kicad::sexpr::{atom, list, num, string, SExpr};
use crate::model::{PadKind, MIL_TO_MM};

/// Smallest pad size we write, in mm. Pads with radius 0 still get a visible size.
const MIN_PAD_MM: f64 = 0.05;

/// Space between the pads and the courtyard outline, in mm.
const COURTYARD_MARGIN_MM: f64 = 0.25;

/// Corner rounding for roundrect pads (fraction of the shorter side).
const ROUNDRECT_RATIO: f64 = 0.25;

/// Convert a point from mils (Y up) to KiCad millimetres (Y down).
fn to_mm(x: f64, y: f64) -> (f64, f64) {
    (x * MIL_TO_MM, -y * MIL_TO_MM)
}

/// Pad diameter in mm (twice the radius), never smaller than MIN_PAD_MM.
fn diameter_mm(pad: &Pad) -> f64 {
    (2.0 * pad.radius * MIL_TO_MM).max(MIN_PAD_MM)
}

/// KiCad shape name, size (w, h) in mm and angle in degrees for one pad.
fn pad_shape(pad: &Pad) -> (&'static str, f64, f64, f64) {
    match pad.geom {
        None => {
            let d = diameter_mm(pad);
            ("circle", d, d, 0.0)
        }
        Some(g) => {
            let shape = match g.kind {
                PadKind::Circle => "circle",
                PadKind::Rect => "rect",
                PadKind::RoundRect => "roundrect",
                PadKind::Oval => "oval",
            };
            let w = (g.w * MIL_TO_MM).max(MIN_PAD_MM);
            let h = (g.h * MIL_TO_MM).max(MIN_PAD_MM);
            (shape, w, h, g.angle)
        }
    }
}

/// Drill diameter (mm) for a through-hole pad whose smaller side is `size_mm`:
/// 60 % of the pad, at least 0.2 mm, and always smaller than the pad itself.
pub fn drill_for(size_mm: f64) -> f64 {
    let d = (size_mm * 0.6).max(0.2);
    let d = if d >= size_mm { size_mm * 0.6 } else { d };
    (d * 100.0).round() / 100.0
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

/// One pad: SMD, or through-hole with a drill.
fn pad_sexpr(pad: &Pad, through_hole: bool) -> SExpr {
    let (x, y) = to_mm(pad.x, pad.y);
    let (shape, w, h, angle) = pad_shape(pad);

    let at = if angle == 0.0 {
        list("at", vec![num(x), num(y)])
    } else {
        list("at", vec![num(x), num(y), num(angle)])
    };

    let mut children = vec![
        string(&pad.number),
        atom(if through_hole { "thru_hole" } else { "smd" }),
        atom(shape),
        at,
        list("size", vec![num(w), num(h)]),
    ];
    if through_hole {
        children.push(list("drill", vec![num(drill_for(w.min(h)))]));
        children.push(list("layers", vec![string("*.Cu"), string("*.Mask")]));
    } else {
        children.push(list(
            "layers",
            vec![string("F.Cu"), string("F.Paste"), string("F.Mask")],
        ));
    }
    if shape == "roundrect" {
        children.push(list("roundrect_rratio", vec![num(ROUNDRECT_RATIO)]));
    }

    list("pad", children)
}

/// Bounding box of all pads (each pad's rotated extent) in mm, grown by the
/// courtyard margin. Returns (x1, y1, x2, y2) with (x1, y1) as the minimum corner.
fn courtyard(pads: &[Pad]) -> (f64, f64, f64, f64) {
    let mut x1 = f64::INFINITY;
    let mut y1 = f64::INFINITY;
    let mut x2 = f64::NEG_INFINITY;
    let mut y2 = f64::NEG_INFINITY;

    for pad in pads {
        let (x, y) = to_mm(pad.x, pad.y);
        let (_, w, h, angle) = pad_shape(pad);
        let a = angle.to_radians();
        let (s, c) = (a.sin().abs(), a.cos().abs());
        let half_x = (c * w + s * h) / 2.0;
        let half_y = (s * w + c * h) / 2.0;
        x1 = x1.min(x - half_x);
        y1 = y1.min(y - half_y);
        x2 = x2.max(x + half_x);
        y2 = y2.max(y + half_y);
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
        list(
            "attr",
             vec![atom(if fp.through_hole { "through_hole" } else { "smd" })],
        ),
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
    children.extend(fp.pads.iter().map(|p| pad_sexpr(p, fp.through_hole)));

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
    use crate::model::PadGeom;

    fn pad(number: &str, x: f64, y: f64, radius: f64) -> Pad {
        Pad {
            number: number.to_string(),
            x,
            y,
            radius,
            geom: None,
        }
    }

    /// A pad at the origin with the given shape.
    fn shaped_pad(kind: PadKind, w: f64, h: f64, angle: f64) -> Pad {
        Pad {
            number: "1".to_string(),
            x: 0.0,
            y: 0.0,
            radius: w.max(h) / 2.0,
            geom: Some(PadGeom { kind, w, h, angle }),
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

    fn text_of(pads: Vec<Pad>) -> String {
        footprint_sexpr(&footprint(pads, None)).to_pretty()
    }

    /// The two-pad footprint used by several tests.
    fn two_pad_text() -> String {
        text_of(vec![pad("1", -8.86, 0.0, 4.92), pad("2", 8.86, 0.0, 4.92)])
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
        let text = text_of(vec![pad("1", 0.0, 10.0, 4.0)]);
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

    #[test]
    fn rect_pad() {
        let text = text_of(vec![shaped_pad(PadKind::Rect, 20.0, 10.0, 0.0)]);
        assert!(text.contains("(pad \"1\" smd rect"), "text:\n{}", text);
        assert!(text.contains("(size 0.508 0.254)"), "text:\n{}", text);
        assert!(text.contains("(at 0 0)"), "text:\n{}", text);
    }

    #[test]
    fn rotated_rect_pad_has_angle() {
        let text = text_of(vec![shaped_pad(PadKind::Rect, 20.0, 10.0, 30.0)]);
        assert!(text.contains("(at 0 0 30)"), "text:\n{}", text);
    }

    #[test]
    fn roundrect_pad() {
        let text = text_of(vec![shaped_pad(PadKind::RoundRect, 20.0, 10.0, 0.0)]);
        assert!(text.contains("roundrect"), "text:\n{}", text);
        assert!(text.contains("(roundrect_rratio 0.25)"), "text:\n{}", text);
    }

    #[test]
    fn oval_pad() {
        let text = text_of(vec![shaped_pad(PadKind::Oval, 20.0, 10.0, 0.0)]);
        assert!(text.contains("(pad \"1\" smd oval"), "text:\n{}", text);
    }

    #[test]
    fn courtyard_uses_rotated_extent() {
        let (x1, _, x2, _) = courtyard(&[shaped_pad(PadKind::Rect, 40.0, 20.0, 90.0)]);
        assert!((x1 + 0.504).abs() < 1e-6, "{}", x1);
        assert!((x2 - 0.504).abs() < 1e-6, "{}", x2);
    }

    #[test]
    fn through_hole_pads_have_drill() {
        let mut fp = footprint(vec![shaped_pad(PadKind::Circle, 60.0, 60.0, 0.0)], None);
        fp.through_hole = true;
        let text = footprint_sexpr(&fp).to_pretty();
        assert!(text.contains("(pad \"1\" thru_hole circle"), "text:\n{}", text);
        assert!(text.contains("(layers \"*.Cu\" \"*.Mask\")"), "text:\n{}", text);
        // 60 mils = 1.524 mm -> 60 % = 0.9144 -> 0.91 mm.
        assert!(text.contains("(drill 0.91)"), "text:\n{}", text);
        assert!(text.contains("(attr through_hole)"), "text:\n{}", text);
    }

    #[test]
    fn drill_is_smaller_than_pad() {
        assert_eq!(drill_for(1.0), 0.6);
        assert!(drill_for(0.25) < 0.25);
        assert!(drill_for(0.1) < 0.1);
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

        let board = crate::formats::fz::parse::parse(&content, &descr);
        let lib = crate::footprint::build(&board);
        let dir = Path::new("out/Board.pretty");
        let written = write_library(&lib, dir).expect("writing the library failed");
        println!("wrote {} footprints to {}", written, dir.display());
    }
}
