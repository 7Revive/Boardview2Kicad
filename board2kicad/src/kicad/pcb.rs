use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::footprint::{Footprint, Library, Placement};
use crate::kicad::sexpr::{atom, list, num, string, SExpr};
use crate::model::{Board, Part, Pin, MIL_TO_MM};

/// Smallest pad diameter written, in mm.
const MIN_PAD_MM: f64 = 0.05;

/// Space between the outermost pins and the board outline, in mm.
const EDGE_MARGIN_MM: f64 = 2.0;

/// Counts from writing a board, useful for a summary message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardStats {
    /// Footprints written to the board.
    pub footprints: usize,
    /// Nets written (not counting the empty net 0).
    pub nets: usize,
    /// Placements skipped because their part or footprint was missing or inconsistent.
    pub skipped: usize,
}

/// Convert a point from mils (Y up) to KiCad mm (Y down).
fn to_kicad(x: f64, y: f64) -> (f64, f64) {
    (x * MIL_TO_MM, -y * MIL_TO_MM)
}

/// Turn values that are zero apart from rounding noise into exact zeros.
fn clean(v: f64) -> f64 {
    if v.abs() < 1e-9 {
        0.0
    } else {
        v
    }
}

/// (sin, cos) of an angle in degrees. Multiples of 90 are exact, so a
/// 90-degree turn doesn't leave tiny errors like 6e-17.
fn sin_cos_deg(deg: f64) -> (f64, f64) {
    let d = deg.rem_euclid(360.0);
    if d == 0.0 {
        (0.0, 1.0)
    } else if d == 90.0 {
        (1.0, 0.0)
    } else if d == 180.0 {
        (0.0, -1.0)
    } else if d == 270.0 {
        (-1.0, 0.0)
    } else {
        d.to_radians().sin_cos()
    }
}

/// KiCad origin (ox, oy) and angle θ (degrees, 0..360) of a placement.
pub(crate) fn frame(pl: &Placement) -> (f64, f64, f64) {
    // Bottom placements are stored in mirrored (top-view) space: undo that.
    let x = if pl.bottom { -pl.x } else { pl.x };
    let (ox, oy) = to_kicad(x, pl.y);
    let theta = if pl.bottom {
        (180.0 - pl.rotation).rem_euclid(360.0)
    } else {
        pl.rotation.rem_euclid(360.0)
    };
    (ox, oy, theta)
}

/// Position of a pin inside its footprint (lx, ly), in KiCad mm, for a
/// footprint at (ox, oy) with angle θ. This undoes KiCad's own placement
/// rotation, so KiCad puts the pad exactly on the original pin.
pub(crate) fn pad_local(ox: f64, oy: f64, theta: f64, pin: &Pin) -> (f64, f64) {
    let (px, py) = to_kicad(pin.x, pin.y);
    let (dx, dy) = (px - ox, py - oy);
    let (s, c) = sin_cos_deg(theta);
    (clean(dx * c - dy * s), clean(dx * s + dy * c))
}

/// One entry of the layer table, e.g. (0 "F.Cu" signal) or (36 "B.SilkS" user "B.Silks
fn layer(number: usize, name: &str, kind: &str, user_name: Option<&str>) -> SExpr {
    let mut items = vec![num(number as f64), string(name), atom(kind)];
    if let Some(u) = user_name {
        items.push(string(u));
    }
    SExpr::List(items)
}

/// The standard two-sided layer stack.
fn layers() -> SExpr {
    list(
        "layers",
         vec![
             layer(0, "F.Cu", "signal", None),
         layer(31, "B.Cu", "signal", None),
         layer(32, "B.Adhes", "user", Some("B.Adhesive")),
         layer(33, "F.Adhes", "user", Some("F.Adhesive")),
         layer(34, "B.Paste", "user", None),
         layer(35, "F.Paste", "user", None),
         layer(36, "B.SilkS", "user", Some("B.Silkscreen")),
         layer(37, "F.SilkS", "user", Some("F.Silkscreen")),
         layer(38, "B.Mask", "user", None),
         layer(39, "F.Mask", "user", None),
         layer(40, "Dwgs.User", "user", Some("User.Drawings")),
         layer(41, "Cmts.User", "user", Some("User.Comments")),
         layer(42, "Eco1.User", "user", Some("User.Eco1")),
         layer(43, "Eco2.User", "user", Some("User.Eco2")),
         layer(44, "Edge.Cuts", "user", None),
         layer(45, "Margin", "user", None),
         layer(46, "B.CrtYd", "user", Some("B.Courtyard")),
         layer(47, "F.CrtYd", "user", Some("F.Courtyard")),
         layer(48, "B.Fab", "user", None),
         layer(49, "F.Fab", "user", None),
         ],
    )
}

/// Board outline: bounding box of all pins, grown by EDGE_MARGIN_MM.
fn edge_cuts(board: &Board) -> SExpr {
    let mut x1 = f64::INFINITY;
    let mut y1 = f64::INFINITY;
    let mut x2 = f64::NEG_INFINITY;
    let mut y2 = f64::NEG_INFINITY;
    for pin in board.parts.iter().flat_map(|p| &p.pins) {
        let (x, y) = to_kicad(pin.x, pin.y);
        x1 = x1.min(x);
        y1 = y1.min(y);
        x2 = x2.max(x);
        y2 = y2.max(y);
    }
    // A board without pins: use a small box around the origin.
    if x1 > x2 {
        (x1, y1, x2, y2) = (0.0, 0.0, 0.0, 0.0);
    }
    let m = EDGE_MARGIN_MM;
    list(
        "gr_rect",
         vec![
             list("start", vec![num(x1 - m), num(y1 - m)]),
         list("end", vec![num(x2 + m), num(y2 + m)]),
         list(
             "stroke",
              vec![
                  list("width", vec![num(0.1)]),
              list("type", vec![atom("default")]),
              ],
         ),
         list("fill", vec![atom("none")]),
         list("layer", vec![string("Edge.Cuts")]),
         ],
    )
}

/// Text style; bottom-side text is mirrored so it reads correctly from below.
fn effects(bottom: bool) -> SExpr {
    let mut items = vec![list(
        "font",
        vec![
            list("size", vec![num(0.5), num(0.5)]),
                              list("thickness", vec![num(0.1)]),
        ],
    )];
    if bottom {
        items.push(list("justify", vec![atom("mirror")]));
    }
    list("effects", items)
}

/// A footprint text property ("Reference" or "Value").
fn property(name: &str, value: &str, y: f64, theta: f64, layer: &str, bottom: bool) -> SExpr {
list(
    "property",
     vec![
         string(name),
     string(value),
     list("at", vec![num(0.0), num(y), num(theta)]),
     list("layer", vec![string(layer)]),
     effects(bottom),
     ],
)
}

/// One placed footprint, or None if the placement doesn't fit its part/footprint.
fn placed_footprint(
    pl: &Placement,
    fp: &Footprint,
    part: &Part,
    nets: &HashMap<&str, usize>,
) -> Option<SExpr> {
    // Every footprint pad needs a valid pin; otherwise skip this placement.
    if pl.pad_pins.len() != fp.pads.len() || pl.pad_pins.iter().any(|&i| i >= part.pins.len()) {
        return None;
    }

    let (ox, oy, theta) = frame(pl);
    let side = if pl.bottom { "B" } else { "F" };
    let id = fp.id.to_string();

    let mut items = vec![
        string(&format!("Board:{}", id)),
        list("layer", vec![string(&format!("{}.Cu", side))]),
        list("at", vec![num(ox), num(oy), num(theta)]),
        property("Reference", &pl.refdes, -1.0, theta, &format!("{}.SilkS", side), pl.bottom),
        property("Value", &id, 1.0, theta, &format!("{}.Fab", side), pl.bottom),
        list("attr", vec![atom("smd")]),
    ];

    for (pad, &pin_index) in fp.pads.iter().zip(&pl.pad_pins) {
        let pin = &part.pins[pin_index];
        let (lx, ly) = pad_local(ox, oy, theta, pin);
        let d = (2.0 * pin.radius * MIL_TO_MM).max(MIN_PAD_MM);

        let mut pad_items = vec![
            string(&pad.number),
            atom("smd"),
            atom("circle"),
            list("at", vec![num(lx), num(ly), num(theta)]),
            list("size", vec![num(d), num(d)]),
            list(
                "layers",
                 vec![
                     string(&format!("{}.Cu", side)),
                 string(&format!("{}.Paste", side)),
                 string(&format!("{}.Mask", side)),
                 ],
            ),
        ];
        if !pin.net.is_empty() {
            if let Some(&n) = nets.get(pin.net.as_str()) {
                pad_items.push(list("net", vec![num(n as f64), string(&pin.net)]));
            }
        }
        if !pin.name.is_empty() {
            pad_items.push(list("pinfunction", vec![string(&pin.name)]));
        }
        items.push(list("pad", pad_items));
    }

    Some(list("footprint", items))
}

/// Build the whole board and count what was written.
pub fn board_sexpr_with_stats(board: &Board, lib: &Library) -> (SExpr, BoardStats) {
    // Net name -> net number (1, 2, 3...; 0 is the "no net" entry).
    let net_names = board.net_names();
    let nets: HashMap<&str, usize> = net_names
    .iter()
    .enumerate()
    .map(|(i, n)| (n.as_str(), i + 1))
    .collect();

    // Fast lookups (board.part() searches the whole list every time).
    let parts: HashMap<&str, &Part> = board.parts.iter().map(|p| (p.refdes.as_str(), p)).collect();
    let footprints: HashMap<usize, &Footprint> = lib.footprints.iter().map(|f| (f.id, f)).collect();

    let mut items = vec![
        list("version", vec![num(20240108.0)]),
        list("generator", vec![string("fz2kicad")]),
        list("generator_version", vec![string("0.1")]),
        list(
            "general",
             vec![
                 list("thickness", vec![num(1.6)]),
             list("legacy_teardrops", vec![atom("no")]),
             ],
        ),
        list("paper", vec![string("A3")]),
        layers(),
        list("setup", vec![list("pad_to_mask_clearance", vec![num(0.0)])]),
        list("net", vec![num(0.0), string("")]),
    ];
    for (i, name) in net_names.iter().enumerate() {
        items.push(list("net", vec![num((i + 1) as f64), string(name)]));
    }
    items.push(edge_cuts(board));

    let mut placed = 0;
    let mut skipped = 0;
    for pl in &lib.placements {
        let found = match (parts.get(pl.refdes.as_str()), footprints.get(&pl.footprint_id)) {
            (Some(part), Some(fp)) => placed_footprint(pl, fp, part, &nets),
            _ => None,
        };
        match found {
            Some(e) => {
                items.push(e);
                placed += 1;
            }
            None => skipped += 1,
        }
    }

    let stats = BoardStats {
        footprints: placed,
        nets: net_names.len(),
        skipped,
    };
    (list("kicad_pcb", items), stats)
}

/// Build the whole board as one S-expression.
pub fn board_sexpr(board: &Board, lib: &Library) -> SExpr {
    board_sexpr_with_stats(board, lib).0
}

/// Write the board to a .kicad_pcb file.
pub fn write_board(board: &Board, lib: &Library, path: &Path) -> Result<(), String> {
    let text = format!("{}\n", board_sexpr(board, lib).to_pretty());
    fs::write(path, text).map_err(|e| format!("cannot write {}: {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::footprint::build;

    /// Where KiCad will put a pad: its own placement formula, written out separately.
    fn absolute(ox: f64, oy: f64, theta: f64, lx: f64, ly: f64) -> (f64, f64) {
        let (s, c) = theta.to_radians().sin_cos();
        (ox + lx * c + ly * s, oy - lx * s + ly * c)
    }

    /// Make a part from (number, name, net, x, y) tuples; radius 5 mils.
    fn part(refdes: &str, bottom: bool, pins: &[(&str, &str, &str, f64, f64)]) -> Part {
    Part {
        refdes: refdes.to_string(),
        insertion_code: "1".to_string(),
        sym_name: String::new(),
        bottom,
        rotation: None,
        part_number: None,
        description: None,
        pins: pins
        .iter()
        .map(|&(number, name, net, x, y)| Pin {
        number: number.to_string(),
        name: name.to_string(),
        net: net.to_string(),
        x,
        y,
        radius: 5.0,
            })
    .collect(),
    }
}

/// Horizontal and vertical 2-pin parts, two asymmetric bottom parts, a rotated 4-pin part.
/// Horizontal and vertical 2-pin parts, two asymmetric bottom parts, a rotated 4-pin part.
fn test_board() -> Board {
    Board {
        parts: vec![
            part("R1", false, &[("1", "", "GND", 0.0, 0.0), ("2", "", "VCC", 20.0, 0.0)]),
            part("R2", false, &[("1", "", "GND", 100.0, 100.0), ("2", "", "SIG", 100.0, 120.0)]),
            part("Q1", true, &[("1", "G", "SIG", -200.0, 50.0), ("2", "S", "GND", -230.0, 50.0), ("3", "D", "VCC", -200.0, 60.0)]),
            part("Q2", true, &[("1", "G", "SIG", 500.0, -100.0), ("2", "S", "GND", 500.0, -130.0), ("3", "D", "VCC", 490.0, -100.0)]),
            part("U1", false, &[("1", "VIN", "VCC", 300.0, 300.0), ("2", "", "", 300.0, 310.0), ("3", "", "", 290.0, 310.0), ("4", "", "GND", 290.0, 300.0)]),
        ],
        warnings: Vec::new(),
    }
}

/// Calls `check(placement, footprint pad, pin, lx, ly, ox, oy, θ)` for every pad w
fn for_each_pad(
    board: &Board,
    lib: &Library,
    mut check: impl FnMut(&Placement, &crate::footprint::Pad, &Pin, f64, f64, f64, f64, f64),
) {
    for pl in &lib.placements {
        let part = board.part(&pl.refdes).unwrap();
        let fp = &lib.footprints[pl.footprint_id - 1];
        let (ox, oy, theta) = frame(pl);
        for (i, &pin_index) in pl.pad_pins.iter().enumerate() {
            let pin = &part.pins[pin_index];
            let (lx, ly) = pad_local(ox, oy, theta, pin);
            check(pl, &fp.pads[i], pin, lx, ly, ox, oy, theta);
        }
    }
}

fn close(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
}

    #[test]
    fn pads_land_on_original_pins() {
        let board = test_board();
        let lib = build(&board);
        let mut checked = 0;
        for_each_pad(&board, &lib, |pl, _pad, pin, lx, ly, ox, oy, theta| {
            let got = absolute(ox, oy, theta, lx, ly);
            let want = (pin.x * 0.0254, -pin.y * 0.0254);
            assert!(close(got, want), "{} pin {}: got {:?}, want {:?}", pl.refdes, pin.number, got, want);
            checked += 1;
        });
        assert_eq!(checked, 14);
    }

    #[test]
    fn top_pads_match_library() {
        let board = test_board();
        let lib = build(&board);
        for_each_pad(&board, &lib, |pl, pad, _pin, lx, ly, _, _, _| {
            if !pl.bottom {
                let want = (pad.x * 0.0254, -pad.y * 0.0254);
                assert!(close((lx, ly), want), "{}: {:?} vs {:?}", pl.refdes, (lx, ly), want);
            }
        });
    }

    #[test]
    fn bottom_pads_match_flipped_library() {
        let board = test_board();
        let lib = build(&board);
        for_each_pad(&board, &lib, |pl, pad, _pin, lx, ly, _, _, _| {
            if pl.bottom {
                let want = (pad.x * 0.0254, pad.y * 0.0254);
                assert!(close((lx, ly), want), "{}: {:?} vs {:?}", pl.refdes, (lx, ly), want);
            }
        });
    }

    #[test]
    fn text_contents() {
        let board = test_board();
        let lib = build(&board);
        let (sexpr, stats) = board_sexpr_with_stats(&board, &lib);
        let text = sexpr.to_pretty();

        assert!(text.contains("(net 0 \"\")"));
        assert!(
            text.lines()
            .map(str::trim)
            .any(|l| l.starts_with("(net ") && l.ends_with("\"GND\")")),
                "no GND net line"
        );
        assert!(text.contains("(pinfunction \"VIN\")"));
        assert!(text.contains("(layer \"B.Cu\")"));
        assert_eq!(stats.footprints, 5);
        assert_eq!(stats.nets, 3);
        assert_eq!(stats.skipped, 0);
    }

    /// Run with: cargo test kicad::pcb::tests::real_file -- --ignored --nocapture
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
        let lib = build(&board);
        let path = Path::new("out/Board.kicad_pcb");
        write_board(&board, &lib, path).expect("writing the board failed");

        let (_, stats) = board_sexpr_with_stats(&board, &lib);
        println!("wrote {}", path.display());
        println!("footprints: {}", stats.footprints);
        println!("nets:       {}", stats.nets);
        println!("skipped:    {}", stats.skipped);

        for refdes in ["C9208", "VY5000"] {
            let Some(pl) = lib.placements.iter().find(|p| p.refdes == refdes) else {
                println!("{} not placed", refdes);
                continue;
            };
            let part = board.part(refdes).unwrap();
            let fp = &lib.footprints[pl.footprint_id - 1];
            let (ox, oy, theta) = frame(pl);
            println!(
                "{}: footprint {} at ({:.6}, {:.6}) angle {} {}",
                     refdes,
                     fp.id,
                     ox,
                     oy,
                     theta,
                     if pl.bottom { "bottom" } else { "top" }
            );
            for (i, &pin_index) in pl.pad_pins.iter().enumerate() {
                let pin = &part.pins[pin_index];
                let (lx, ly) = pad_local(ox, oy, theta, pin);
                let (ax, ay) = absolute(ox, oy, theta, lx, ly);
                println!(
                    "  pad {:>5} -> ({:.6}, {:.6})  net {:?}",
                         fp.pads[i].number, ax, ay, pin.net
                );
            }
        }
    }
}
