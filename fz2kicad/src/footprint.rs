use std::collections::HashMap;
use crate::model::{Board, Part};

/// One pad of a footprint, in footprint-local coordinates (mils, top view).
#[derive(Debug, Clone)]
pub struct Pad {
    /// Pad number, e.g. "1" or "H20".
    pub number: String,
    /// X relative to the footprint origin, mils.
    pub x: f64,
    /// Y relative to the footprint origin, mils (Y up).
    pub y: f64,
    /// Half the pad size, mils.
    pub radius: f64,
}

/// One distinct pad layout.
#[derive(Debug, Clone)]
pub struct Footprint {
    /// 1-based id, in order of first appearance.
    pub id: usize,
    /// Pads in local coordinates; origin = bounding-box centre, top view.
    pub pads: Vec<Pad>,
    /// True if the first part using it had insertion_code == "2".
    pub through_hole: bool,
    /// The first part that used it.
    pub example_refdes: String,
    /// That part's description, if any.
    pub example_description: Option<String>,
    /// How many parts use it.
    pub count: usize,
}

/// Where and how one part uses a footprint.
#[derive(Debug, Clone)]
pub struct Placement {
    /// The part's reference designator.
    pub refdes: String,
    /// Which footprint (1-based id).
    pub footprint_id: usize,
    /// Board X of the footprint origin, mils (in mirrored space if bottom).
    pub x: f64,
    /// Board Y of the footprint origin, mils.
    pub y: f64,
    /// 0, 90, 180 or 270 degrees, counter-clockwise, Y up.
    pub rotation: f64,
    /// True if the part is on the bottom side.
    pub bottom: bool,
    /// pad_pins[i] = index into part.pins for footprint pad i.
    pub pad_pins: Vec<usize>,
}

/// The result: all footprints, how each part uses one, and parts left out.
#[derive(Debug, Clone)]
pub struct Library {
    pub footprints: Vec<Footprint>,
    pub placements: Vec<Placement>,
    /// Refdes of parts with no pins.
    pub skipped: Vec<String>,
}

/// The identity of a pad layout: (number, x, y, radius), in 0.1-mil steps, sorted.
type Key = Vec<(String, i64, i64, i64)>;

/// One rotation of one part, ready to compare with the others.
struct Candidate {
    key: Key,
    pads: Vec<Pad>,
    pad_pins: Vec<usize>,
}

/// Round to 0.1-mil steps, so tiny floating-point differences don't matter.
fn q(v: f64) -> i64 {
    (v * 10.0).round() as i64
}

/// Rotate a point clockwise by 90 degrees, `steps` times.
/// Uses swaps and sign flips only, so there is no rounding error.
fn rotate_cw(x: f64, y: f64, steps: usize) -> (f64, f64) {
    let (mut x, mut y) = (x, y);
    for _ in 0..steps % 4 {
        (x, y) = (y, -x);
    }
    (x, y)
}

/// Rotate a point counter-clockwise by 90 degrees, `steps` times.
fn rotate_ccw(x: f64, y: f64, steps: usize) -> (f64, f64) {
    let (mut x, mut y) = (x, y);
    for _ in 0..steps % 4 {
        (x, y) = (-y, x);
    }
    (x, y)
}

/// Centre of the smallest axis-aligned box that holds all points.
fn bbox_centre(points: &[(f64, f64)]) -> (f64, f64) {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &(x, y) in points {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0)
}

/// Rotate a part's local points clockwise by 90*k degrees and build its key.
fn candidate(part: &Part, local: &[(f64, f64)], k: usize) -> Candidate {
    // (key entry, original pin index, rotated x, rotated y)
    let mut entries: Vec<((String, i64, i64, i64), usize, f64, f64)> = local
    .iter()
    .enumerate()
    .map(|(i, &(x, y))| {
        let (rx, ry) = rotate_cw(x, y, k);
        let pin = &part.pins[i];
        ((pin.number.clone(), q(rx), q(ry), q(pin.radius)), i, rx, ry)
    })
    .collect();

    // Sort by the key entry; if two pads are identical, keep pin order.
    entries.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let pads = entries
    .iter()
    .map(|(_, i, x, y)| Pad {
        number: part.pins[*i].number.clone(),
         x: *x,
         y: *y,
         radius: part.pins[*i].radius,
    })
    .collect();
    let pad_pins = entries.iter().map(|e| e.1).collect();
    let key = entries.into_iter().map(|e| e.0).collect();

    Candidate { key, pads, pad_pins }
}

/// Find all distinct footprints on the board and how each part is placed.
pub fn build(board: &Board) -> Library {
    let mut lib = Library {
        footprints: Vec::new(),
        placements: Vec::new(),
        skipped: Vec::new(),
    };
    // Key -> index into lib.footprints.
    let mut seen: HashMap<Key, usize> = HashMap::new();

    for part in &board.parts {
        // 1. A part without pins has no footprint.
        if part.pins.is_empty() {
            lib.skipped.push(part.refdes.clone());
            continue;
        }

        // 2. Pin positions; bottom parts are mirrored so we see them from the top.
        let points: Vec<(f64, f64)> = part
        .pins
        .iter()
        .map(|p| (if part.bottom { -p.x } else { p.x }, p.y))
        .collect();

        // 3. Move the part so its bounding-box centre is at (0, 0).
        let (cx, cy) = bbox_centre(&points);
        let local: Vec<(f64, f64)> = points.iter().map(|&(x, y)| (x - cx, y - cy)).collect();

        // 4-5. Try all four rotations and keep the one with the smallest key.
        // Only a strictly smaller key wins, so ties go to the smallest k.
        let mut best_k = 0;
        let mut best = candidate(part, &local, 0);
        for k in 1..4 {
            let cand = candidate(part, &local, k);
            if cand.key < best.key {
                best_k = k;
                best = cand;
            }
        }
        let Candidate { key, pads, pad_pins } = best;

        // 6. Reuse a matching footprint, or create a new one.
        let footprint_id = match seen.get(&key) {
            Some(&index) => {
                lib.footprints[index].count += 1;
                index + 1
            }
            None => {
                let index = lib.footprints.len();
                lib.footprints.push(Footprint {
                    id: index + 1,
                    pads,
                    through_hole: part.insertion_code == "2",
                    example_refdes: part.refdes.clone(),
                                    example_description: part.description.clone(),
                                    count: 1,
                });
                seen.insert(key, index);
                index + 1
            }
        };

        // 7. Record how to get from the footprint back to this part:
        //    rotate counter-clockwise by 90*k, move to the centre, un-mirror.
        lib.placements.push(Placement {
            refdes: part.refdes.clone(),
                            footprint_id,
                            x: cx,
                            y: cy,
                            rotation: 90.0 * best_k as f64,
                            bottom: part.bottom,
                            pad_pins,
        });
    }

    lib
}

/// Board position (mils) of footprint pad number `pad` for this placement.
/// The reverse of `build`: rotate CCW, move into place, un-mirror.
/// Panics if `pad` is not a valid index into `fp.pads`.
pub fn to_board(fp: &Footprint, pl: &Placement, pad: usize) -> (f64, f64) {
    let p = &fp.pads[pad];
    let steps = ((pl.rotation / 90.0).round() as i64).rem_euclid(4) as usize;
    let (x, y) = rotate_ccw(p.x, p.y, steps);
    let (x, y) = (x + pl.x, y + pl.y);
    if pl.bottom {
        (-x, y)
    } else {
        (x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Pin;

    /// Make a part from (number, x, y) triples; radius 5, no net.
    fn part(refdes: &str, bottom: bool, pins: &[(&str, f64, f64)]) -> Part {
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
            .map(|&(n, x, y)| Pin {
                number: n.to_string(),
                 name: n.to_string(),
                 net: String::new(),
                 x,
                 y,
                 radius: 5.0,
            })
            .collect(),
        }
    }

    fn board(parts: Vec<Part>) -> Board {
        Board {
            parts,
            warnings: Vec::new(),
        }
    }

    /// Largest difference (mils) between to_board() and the real pin positions.
    fn max_error(board: &Board, lib: &Library) -> f64 {
        let parts: HashMap<&str, &Part> =
        board.parts.iter().map(|p| (p.refdes.as_str(), p)).collect();
        let mut worst: f64 = 0.0;
        for pl in &lib.placements {
            let fp = &lib.footprints[pl.footprint_id - 1];
            let part = parts[pl.refdes.as_str()];
            for (i, &pin_index) in pl.pad_pins.iter().enumerate() {
                let (x, y) = to_board(fp, pl, i);
                let pin = &part.pins[pin_index];
                worst = worst.max((x - pin.x).abs()).max((y - pin.y).abs());
            }
        }
        worst
    }

    #[test]
    fn rotated_copies_share_a_footprint() {
        let b = board(vec![
            part("R1", false, &[("1", 0.0, 0.0), ("2", 20.0, 0.0)]),
                      part("R2", false, &[("1", 100.0, 100.0), ("2", 100.0, 120.0)]),
        ]);
        let lib = build(&b);
        assert_eq!(lib.footprints.len(), 1);
        assert_eq!(lib.footprints[0].count, 2);
        assert_ne!(lib.placements[0].rotation, lib.placements[1].rotation);
    }

    #[test]
    fn mirrored_bottom_part_shares_a_footprint() {
        let shape = [("1", 0.0, 0.0), ("2", 30.0, 0.0), ("3", 0.0, 10.0)];
        // Bottom copy: moved by (200, 50), then x negated (as seen from the top).
        let mirrored: Vec<(&str, f64, f64)> = shape
        .iter()
        .map(|&(n, x, y)| (n, -(x + 200.0), y + 50.0))
        .collect();
        let b = board(vec![part("Q1", false, &shape), part("Q2", true, &mirrored)]);
        let lib = build(&b);
        assert_eq!(lib.placements[0].footprint_id, lib.placements[1].footprint_id);
    }

    #[test]
    fn different_pad_numbers_make_different_footprints() {
        let b = board(vec![
            part("D1", false, &[("1", 0.0, 0.0), ("2", 20.0, 0.0)]),
                      part("D2", false, &[("A", 0.0, 0.0), ("K", 20.0, 0.0)]),
        ]);
        assert_eq!(build(&b).footprints.len(), 2);
    }

    #[test]
    fn round_trip_restores_pin_positions() {
        let tri = [("1", 0.0, 0.0), ("2", 30.0, 0.0), ("3", 0.0, 10.0)];
        // Same triangle rotated 90 degrees CCW and moved.
        let tri_rot: Vec<(&str, f64, f64)> =
        tri.iter().map(|&(n, x, y)| (n, -y + 400.0, x - 300.0)).collect();
        // Same triangle mirrored to the bottom.
        let tri_bot: Vec<(&str, f64, f64)> =
        tri.iter().map(|&(n, x, y)| (n, -(x + 50.0), y + 75.0)).collect();

        let b = board(vec![
            part("R1", false, &[("1", 0.0, 0.0), ("2", 20.0, 0.0)]),
                      part("R2", false, &[("1", 100.0, 100.0), ("2", 100.0, 120.0)]),
                      part("R3", true, &[("1", -10.0, 5.0), ("2", -10.0, 25.0)]),
                      part("Q1", false, &tri),
                      part("Q2", false, &tri_rot),
                      part("Q3", true, &tri_bot),
                      part("TP1", false, &[("1", 33.5, -12.25)]),
                      part(
                          "U1",
                           false,
                           &[("1", 0.0, 0.0), ("2", 10.0, 0.0), ("3", 10.0, 10.0), ("4", 0.0, 10.0)],
                      ),
                      part(
                          "U2",
                           true,
                           &[("1", 500.0, 0.0), ("2", 500.0, 10.0), ("3", 490.0, 10.0), ("4", 490.0, 0.0)],
                      ),
        ]);
        let lib = build(&b);
        assert_eq!(lib.placements.len(), b.parts.len());
        let err = max_error(&b, &lib);
        assert!(err < 1e-6, "largest error {} mils", err);
    }

    #[test]
    fn part_without_pins_is_skipped() {
        let b = board(vec![part("MH1", false, &[])]);
        let lib = build(&b);
        assert_eq!(lib.skipped, vec!["MH1"]);
        assert!(lib.placements.is_empty());
    }

    /// Run with: cargo test footprint::tests::real_file -- --ignored --nocapture
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

        let b = crate::fz::parse::parse(&content, &descr);
        let lib = build(&b);

        println!("footprints: {}", lib.footprints.len());
        println!("skipped:    {}", lib.skipped.len());

        let mut by_use: Vec<&Footprint> = lib.footprints.iter().collect();
        by_use.sort_by(|a, b| b.count.cmp(&a.count).then(a.id.cmp(&b.id)));
        println!("most used:");
        for fp in by_use.iter().take(15) {
            println!(
                "  #{:<5} used {:>5}x, {:>4} pads, e.g. {} {:?}",
                fp.id,
                fp.count,
                fp.pads.len(),
                     fp.example_refdes,
                     fp.example_description
            );
        }

        for refdes in ["C9208", "VY5000"] {
            match lib.placements.iter().find(|p| p.refdes == refdes) {
                Some(pl) => {
                    println!("{:#?}", lib.footprints[pl.footprint_id - 1]);
                    println!("{:#?}", pl);
                }
                None => println!("{} not placed", refdes),
            }
        }

        println!("largest round-trip error: {} mils", max_error(&b, &lib));
    }
}
