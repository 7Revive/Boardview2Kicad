//! GenCAD (.cad) reader.
//!
//! Ported from BoardRipper's `cad-parser.ts` (AGPL-3.0).
//!
//! Reads the $HEADER, $PADS, $PADSTACKS, $SHAPES, $COMPONENTS, $SIGNALS and
//! $DEVICES sections and produces a format-neutral `Board`. All coordinates
//! and sizes are converted to mils (Y up).

pub mod lexer;

use std::collections::{HashMap, HashSet};

use crate::model::{Board, Part, Pin};
use lexer::{num, section, sections, to_text, Line, Section};

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Joins all args with single spaces (for names that may contain spaces).
fn join(args: &[String]) -> String {
    args.join(" ")
}

/// Argument `i` as a number.
fn arg(line: &Line, i: usize) -> Option<f64> {
    line.args.get(i).and_then(|s| num(s))
}

/// Argument `i` uppercased, or "" if missing.
fn arg_upper(line: &Line, i: usize) -> String {
    line.args
    .get(i)
    .map(|s| s.to_uppercase())
    .unwrap_or_default()
}

/// Grows an optional bounding box (min_x, min_y, max_x, max_y) by a point.
fn grow(b: &mut Option<(f64, f64, f64, f64)>, x: f64, y: f64) {
    *b = Some(match *b {
        None => (x, y, x, y),
              Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
    });
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// True if the file starts with $HEADER and mentions GENCAD in its first 4 KiB.
pub fn is_gencad(data: &[u8]) -> bool {
    let head = &data[..data.len().min(4096)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    let upper = text.to_uppercase();
    upper.starts_with("$HEADER") && upper.contains("GENCAD")
}

// ---------------------------------------------------------------------------
// 1. Units
// ---------------------------------------------------------------------------

/// Multiplier from file units to mils.
fn units_factor(all: &[Section], warnings: &mut Vec<String>) -> f64 {
    let line = match section(all, "HEADER").and_then(|h| h.find("UNITS")) {
        Some(l) => l,
        None => {
            warnings.push("no UNITS in $HEADER; assuming mils".to_string());
            return 1.0;
        }
    };
    match arg_upper(line, 0).as_str() {
        "INCH" => 1000.0,
        "THOU" | "MIL" | "MILS" => 1.0,
        "MM" => 1000.0 / 25.4,
        "USER" => match arg(line, 1) {
            Some(n) if n > 0.0 => 1000.0 / n,
            _ => {
                warnings.push(format!(
                    "line {}: UNITS USER without a valid number; assuming mils",
                    line.number
                ));
                1.0
            }
        },
        other => {
            warnings.push(format!(
                "line {}: unknown UNITS {:?}; assuming mils",
                line.number, other
            ));
            1.0
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Pads
// ---------------------------------------------------------------------------

struct PadAcc {
    name: String,
    circle: f64,
    bbox: Option<(f64, f64, f64, f64)>,
}

fn finish_pad(acc: PadAcc, f: f64, out: &mut HashMap<String, f64>) {
    let radius = if acc.circle > 0.0 {
        acc.circle
    } else {
        acc.bbox
        .map(|(x0, y0, x1, y1)| (x1 - x0).min(y1 - y0) / 2.0)
        .unwrap_or(0.0)
    };
    let r = radius * f;
    if r > 0.0 && !acc.name.is_empty() {
        out.insert(acc.name, r);
    }
}

/// Pad name -> radius in mils.
fn read_pads(all: &[Section], f: f64) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    let Some(sec) = section(all, "PADS") else {
        return out;
    };
    let mut cur: Option<PadAcc> = None;

    for line in &sec.lines {
        match line.keyword.as_str() {
            "PAD" => {
                if let Some(acc) = cur.take() {
                    finish_pad(acc, f, &mut out);
                }
                cur = Some(PadAcc {
                    name: line.args.first().cloned().unwrap_or_default(),
                           circle: 0.0,
                           bbox: None,
                });
            }
            "RECTANGLE" => {
                if let (Some(acc), Some(w), Some(h)) = (cur.as_mut(), arg(line, 2), arg(line, 3)) {
                    grow(&mut acc.bbox, 0.0, 0.0);
                    grow(&mut acc.bbox, w.abs(), h.abs());
                }
            }
            "CIRCLE" => {
                if let (Some(acc), Some(r)) = (cur.as_mut(), arg(line, 2)) {
                    acc.circle = acc.circle.max(r.abs());
                }
            }
            "LINE" | "ARC" => {
                if let (Some(acc), Some(x1), Some(y1), Some(x2), Some(y2)) = (
                    cur.as_mut(),
                                                                              arg(line, 0),
                                                                              arg(line, 1),
                                                                              arg(line, 2),
                                                                              arg(line, 3),
                ) {
                    grow(&mut acc.bbox, x1, y1);
                    grow(&mut acc.bbox, x2, y2);
                }
            }
            _ => {}
        }
    }
    if let Some(acc) = cur.take() {
        finish_pad(acc, f, &mut out);
    }
    out
}

// ---------------------------------------------------------------------------
// 3. Padstacks
// ---------------------------------------------------------------------------

/// Padstack name -> radius in mils (smallest outer-layer pad, else smallest other).
fn read_padstacks(all: &[Section], pads: &HashMap<String, f64>) -> HashMap<String, f64> {
    let mut outer: HashMap<String, f64> = HashMap::new();
    let mut other: HashMap<String, f64> = HashMap::new();
    let Some(sec) = section(all, "PADSTACKS") else {
        return outer;
    };
    let mut cur: Option<String> = None;

    for line in &sec.lines {
        match line.keyword.as_str() {
            "PADSTACK" => cur = line.args.first().cloned(),
            "PAD" => {
                let Some(stack) = cur.as_ref() else { continue };
                let Some(pad_name) = line.args.first() else { continue };
                let layer = arg_upper(line, 1);
                if layer.contains("MASK") || layer.contains("PASTE") || layer.contains("GLUE") {
                    continue;
                }
                let Some(&r) = pads.get(pad_name) else { continue };
                let target = if matches!(layer.as_str(), "TOP" | "BOTTOM" | "ALL") {
                    &mut outer
                } else {
                    &mut other
                };
                let e = target.entry(stack.clone()).or_insert(r);
                if r < *e {
                    *e = r;
                }
            }
            _ => {}
        }
    }

    for (k, v) in other {
        outer.entry(k).or_insert(v);
    }
    outer
}

// ---------------------------------------------------------------------------
// 4. Shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ShapePin {
    name: String,
    padstack: String,
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, Default)]
struct Shape {
    through: bool,
    pins: Vec<ShapePin>,
}

fn read_shapes(all: &[Section], f: f64) -> HashMap<String, Shape> {
    let mut shapes: HashMap<String, Shape> = HashMap::new();
    let Some(sec) = section(all, "SHAPES") else {
        return shapes;
    };
    let mut cur: Option<String> = None;

    for line in &sec.lines {
        match line.keyword.as_str() {
            "SHAPE" => {
                let name = join(&line.args);
                shapes.insert(name.clone(), Shape::default());
                cur = Some(name);
            }
            "INSERT" => {
                if let Some(s) = cur.as_ref().and_then(|n| shapes.get_mut(n)) {
                    let v = arg_upper(line, 0);
                    s.through = v == "TH" || v == "THROUGHHOLE";
                }
            }
            "PIN" => {
                if line.args.len() < 5 {
                    continue;
                }
                if let (Some(s), Some(x), Some(y)) =
                    (cur.as_ref().and_then(|n| shapes.get_mut(n)), arg(line, 2), arg(line, 3))
                    {
                        s.pins.push(ShapePin {
                            name: line.args[0].clone(),
                                    padstack: line.args[1].clone(),
                                    x: x * f,
                                    y: y * f,
                        });
                    }
            }
            _ => {}
        }
    }

    for s in shapes.values_mut() {
        let pins = std::mem::take(&mut s.pins);
        s.pins = dedupe_shape_pins(pins);
    }
    shapes
}

/// Some files list a shape's pins twice (in two coordinate frames). Keep one set.
fn dedupe_shape_pins(pins: Vec<ShapePin>) -> Vec<ShapePin> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (i, p) in pins.iter().enumerate() {
        let g = match index.get(&p.name) {
            Some(&g) => g,
            None => {
                groups.push(Vec::new());
                index.insert(p.name.clone(), groups.len() - 1);
                groups.len() - 1
            }
        };
        groups[g].push(i);
    }
    if groups.iter().all(|g| g.len() <= 1) {
        return pins;
    }
    let picked = pick_by_delta(&pins, &groups).unwrap_or_else(|| pick_by_anchor(&pins, &groups));
    picked.into_iter().map(|i| pins[i].clone()).collect()
}

/// Squared distance of the centroid of the chosen pins from (0, 0).
fn centroid_dist2(pins: &[ShapePin], set: &[usize]) -> f64 {
    if set.is_empty() {
        return 0.0;
    }
    let n = set.len() as f64;
    let cx = set.iter().map(|&i| pins[i].x).sum::<f64>() / n;
    let cy = set.iter().map(|&i| pins[i].y).sum::<f64>() / n;
    cx * cx + cy * cy
}

fn distinct_positions(pins: &[ShapePin], group: &[usize]) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    for &i in group {
        let (x, y) = (pins[i].x, pins[i].y);
        if !out
            .iter()
            .any(|&(qx, qy)| (qx - x).abs() <= 0.01 && (qy - y).abs() <= 0.01)
            {
                out.push((x, y));
            }
    }
    out
}

fn pick_by_delta(pins: &[ShapePin], groups: &[Vec<usize>]) -> Option<Vec<usize>> {
    // Count the offset between the two copies, rounded to 0.5 (stored as 2x ints).
    let mut counts: HashMap<(i64, i64), usize> = HashMap::new();
    let mut pairs = 0usize;
    for g in groups {
        let d = distinct_positions(pins, g);
        if d.len() == 2 {
            let mut dx = ((d[1].0 - d[0].0) * 2.0).round() as i64;
            let mut dy = ((d[1].1 - d[0].1) * 2.0).round() as i64;
            if dx < 0 || (dx == 0 && dy < 0) {
                dx = -dx;
                dy = -dy;
            }
            *counts.entry((dx, dy)).or_insert(0) += 1;
            pairs += 1;
        }
    }
    if pairs == 0 {
        return None;
    }
    let (&key, &best) = counts
    .iter()
    .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))?;
    if (best as f64) < 0.8 * pairs as f64 || key == (0, 0) {
        return None;
    }
    let (ddx, ddy) = (key.0 as f64 / 2.0, key.1 as f64 / 2.0);
    let dot = |i: usize| pins[i].x * ddx + pins[i].y * ddy;

    let mut low = Vec::with_capacity(groups.len());
    let mut high = Vec::with_capacity(groups.len());
    for g in groups {
        let lo = *g
        .iter()
        .min_by(|&&a, &&b| dot(a).partial_cmp(&dot(b)).unwrap_or(std::cmp::Ordering::Equal))?;
        let hi = *g
        .iter()
        .max_by(|&&a, &&b| dot(a).partial_cmp(&dot(b)).unwrap_or(std::cmp::Ordering::Equal))?;
        low.push(lo);
        high.push(hi);
    }
    if centroid_dist2(pins, &low) <= centroid_dist2(pins, &high) {
        Some(low)
    } else {
        Some(high)
    }
}

fn pick_by_anchor(pins: &[ShapePin], groups: &[Vec<usize>]) -> Vec<usize> {
    // Anchor = first group with the most entries.
    let mut anchor = &groups[0];
    for g in groups {
        if g.len() > anchor.len() {
            anchor = g;
        }
    }

    let mut best: Option<(f64, Vec<usize>)> = None;
    for &a in anchor {
        let (ax, ay) = (pins[a].x, pins[a].y);
        let d2 = |i: usize| (pins[i].x - ax).powi(2) + (pins[i].y - ay).powi(2);
        let picked: Vec<usize> = groups
        .iter()
        .map(|g| {
            *g.iter()
            .min_by(|&&p, &&q| d2(p).partial_cmp(&d2(q)).unwrap_or(std::cmp::Ordering::Equal))
            .expect("groups are never empty")
        })
        .collect();
        let score = centroid_dist2(pins, &picked);
        if best.as_ref().map_or(true, |(s, _)| score < *s) {
            best = Some((score, picked));
        }
    }
    best.map(|(_, p)| p).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 5. Components
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mirror {
    None,
    /// MIRRORY: negate local X.
    Y,
    /// MIRRORX: negate local Y.
    X,
}

#[derive(Debug, Clone)]
struct Comp {
    name: String,
    x: f64,
    y: f64,
    bottom: bool,
    rot: f64,
    shape: String,
    mirror: Mirror,
    device: String,
}

impl Comp {
    fn new(name: String) -> Self {
        Comp {
            name,
            x: 0.0,
            y: 0.0,
            bottom: false,
            rot: 0.0,
            shape: String::new(),
            mirror: Mirror::None,
            device: String::new(),
        }
    }

    fn same_placement(&self, o: &Comp) -> bool {
        self.name == o.name
        && self.x == o.x
        && self.y == o.y
        && self.bottom == o.bottom
        && self.rot == o.rot
        && self.shape == o.shape
        && self.mirror == o.mirror
    }
}

fn read_components(all: &[Section], f: f64, warnings: &mut Vec<String>) -> Vec<Comp> {
    let Some(sec) = section(all, "COMPONENTS") else {
        return Vec::new();
    };
    let mut raw: Vec<Comp> = Vec::new();
    let mut cur: Option<Comp> = None;

    for line in &sec.lines {
        if line.keyword == "COMPONENT" {
            if let Some(c) = cur.take() {
                raw.push(c);
            }
            cur = Some(Comp::new(join(&line.args)));
            continue;
        }
        let Some(c) = cur.as_mut() else { continue };
        match line.keyword.as_str() {
            "PLACE" => {
                if let (Some(x), Some(y)) = (arg(line, 0), arg(line, 1)) {
                    c.x = x * f;
                    c.y = y * f;
                }
            }
            "LAYER" => c.bottom = arg_upper(line, 0) == "BOTTOM",
            "ROTATION" => {
                if let Some(r) = arg(line, 0) {
                    c.rot = r;
                }
            }
            "SHAPE" => {
                c.shape = line.args.first().cloned().unwrap_or_default();
                c.mirror = match arg_upper(line, 1).as_str() {
                    "MIRRORY" => Mirror::Y,
                    "MIRRORX" => Mirror::X,
                    _ => Mirror::None,
                };
            }
            "DEVICE" => c.device = join(&line.args),
            _ => {}
        }
    }
    if let Some(c) = cur.take() {
        raw.push(c);
    }

    // a) Back-to-back duplicates.
    let mut kept: Vec<Comp> = Vec::with_capacity(raw.len());
    for c in raw {
        if kept.last().map_or(false, |k| k.same_placement(&c)) {
            continue;
        }
        kept.push(c);
    }

    // b) Revisions: a repeated name starts a new pass; keep only the last pass.
    let mut passes: Vec<Vec<Comp>> = vec![Vec::new()];
    let mut seen: HashSet<String> = HashSet::new();
    for c in kept {
        if seen.contains(&c.name) {
            passes.push(Vec::new());
            seen.clear();
        }
        seen.insert(c.name.clone());
        passes.last_mut().expect("at least one pass").push(c);
    }
    let n_passes = passes.len();
    let last = passes.pop().unwrap_or_default();
    if n_passes > 1 {
        warnings.push(format!(
            "{} revisions found, using the last ({} components)",
                              n_passes,
                              last.len()
        ));
    }
    last
}

// ---------------------------------------------------------------------------
// 6. Signals and 7. Devices
// ---------------------------------------------------------------------------

/// (component, pin) -> net name.
fn read_signals(all: &[Section]) -> HashMap<(String, String), String> {
    let mut out = HashMap::new();
    let Some(sec) = section(all, "SIGNALS") else {
        return out;
    };
    let mut cur: Option<String> = None;
    for line in &sec.lines {
        match line.keyword.as_str() {
            "SIGNAL" => cur = Some(join(&line.args)),
            "NODE" => {
                if let (Some(net), Some(comp), Some(pin)) =
                    (cur.as_ref(), line.args.first(), line.args.get(1))
                    {
                        out.entry((comp.clone(), pin.clone()))
                        .or_insert_with(|| net.clone());
                    }
            }
            _ => {}
        }
    }
    out
}

/// Device name -> (value, part number), from "PART value//partnumber".
fn read_devices(all: &[Section]) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    let Some(sec) = section(all, "DEVICES") else {
        return out;
    };
    let mut cur: Option<String> = None;
    for line in &sec.lines {
        match line.keyword.as_str() {
            "DEVICE" => cur = Some(join(&line.args)),
            "PART" => {
                if let Some(dev) = cur.as_ref() {
                    let text = join(&line.args);
                    let (value, pn) = match text.split_once("//") {
                        Some((l, r)) => (l.trim().to_string(), r.trim().to_string()),
                        None => (text.trim().to_string(), String::new()),
                    };
                    out.insert(dev.clone(), (value, pn));
                }
            }
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 8. Shape coordinate conventions
// ---------------------------------------------------------------------------

fn span(values: impl Iterator<Item = f64>) -> f64 {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for v in values {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if hi >= lo {
        hi - lo
    } else {
        0.0
    }
}

/// Some exporters write shape pins at an offset (not centred on the origin);
/// re-centre those shapes on their pin centroid.
fn center_shapes(shapes: &mut HashMap<String, Shape>, comps: &[Comp]) {
    let sx = span(shapes.values().flat_map(|s| s.pins.iter().map(|p| p.x)));
    let sy = span(shapes.values().flat_map(|s| s.pins.iter().map(|p| p.y)));
    let px = span(comps.iter().map(|c| c.x));
    let py = span(comps.iter().map(|c| c.y));
    let world_coord_shapes = px > 0.0 && py > 0.0 && sx > 2.0 * px && sy > 2.0 * py;
    if world_coord_shapes {
        return;
    }

    let placed_off_origin: HashSet<String> = comps
    .iter()
    .filter(|c| c.x != 0.0 || c.y != 0.0)
    .map(|c| c.shape.clone())
    .collect();

    for (name, shape) in shapes.iter_mut() {
        if shape.pins.is_empty() || !placed_off_origin.contains(name) {
            continue;
        }
        let mut bbox = None;
        for p in &shape.pins {
            grow(&mut bbox, p.x, p.y);
        }
        let (x0, y0, x1, y1) = bbox.expect("shape has pins");
        let (bcx, bcy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let hw = if x1 - x0 > 0.0 { (x1 - x0) / 2.0 } else { 1.0 };
        let hh = if y1 - y0 > 0.0 { (y1 - y0) / 2.0 } else { 1.0 };
        if (bcx.abs() / hw).max(bcy.abs() / hh) >= 2.0 {
            let n = shape.pins.len() as f64;
            let mx = shape.pins.iter().map(|p| p.x).sum::<f64>() / n;
            let my = shape.pins.iter().map(|p| p.y).sum::<f64>() / n;
            for p in shape.pins.iter_mut() {
                p.x -= mx;
                p.y -= my;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 10. Butterfly fold
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Extent {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
    n: usize,
}

impl Extent {
    fn new() -> Self {
        Extent {
            min_x: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            min_y: f64::INFINITY,
            max_y: f64::NEG_INFINITY,
            n: 0,
        }
    }
    fn add(&mut self, x: f64, y: f64) {
        self.min_x = self.min_x.min(x);
        self.max_x = self.max_x.max(x);
        self.min_y = self.min_y.min(y);
        self.max_y = self.max_y.max(y);
        self.n += 1;
    }
}

/// Score and fold line for one axis: (t_lo, t_hi, b_lo, b_hi) along the axis,
/// (t_olo, t_ohi, b_olo, b_ohi) along the other axis.
#[allow(clippy::too_many_arguments)]
fn axis_score(
    t_lo: f64,
    t_hi: f64,
    b_lo: f64,
    b_hi: f64,
    t_olo: f64,
    t_ohi: f64,
    b_olo: f64,
    b_ohi: f64,
) -> Option<(f64, f64)> {
    let fold = if t_hi <= b_lo {
        (t_hi + b_lo) / 2.0
    } else if b_hi <= t_lo {
        (b_hi + t_lo) / 2.0
    } else {
        return None;
    };
    let (ts, bs) = (t_hi - t_lo, b_hi - b_lo);
    let widest = ts.max(bs);
    if widest <= 0.0 {
        return None;
    }
    let width_ratio = ts.min(bs) / widest;
    if width_ratio < 0.6 {
        return None;
    }
    let overlap = (t_ohi.min(b_ohi) - t_olo.max(b_olo)).max(0.0);
    let smaller = (t_ohi - t_olo).min(b_ohi - b_olo);
    if smaller <= 0.0 {
        return None;
    }
    let overlap_ratio = overlap / smaller;
    if overlap_ratio < 0.6 {
        return None;
    }
    Some((overlap_ratio * width_ratio, fold))
}

/// Some files draw the bottom side next to the top. Fold it back over the top.
fn fold_butterfly(board: &mut Board, warnings: &mut Vec<String>) {
    let mut top = Extent::new();
    let mut bot = Extent::new();
    for part in &board.parts {
        let e = if part.bottom { &mut bot } else { &mut top };
        for p in &part.pins {
            e.add(p.x, p.y);
        }
    }
    if top.n < 50 || bot.n < 50 {
        return;
    }

    let on_x = axis_score(
        top.min_x, top.max_x, bot.min_x, bot.max_x, top.min_y, top.max_y, bot.min_y, bot.max_y,
    );
    let on_y = axis_score(
        top.min_y, top.max_y, bot.min_y, bot.max_y, top.min_x, top.max_x, bot.min_x, bot.max_x,
    );

    let (use_x, fold) = match (on_x, on_y) {
        (Some((sx, fx)), Some((sy, fy))) => {
            if sx >= sy {
                (true, fx)
            } else {
                (false, fy)
            }
        }
        (Some((_, fx)), None) => (true, fx),
        (None, Some((_, fy))) => (false, fy),
        (None, None) => return,
    };

    for part in board.parts.iter_mut().filter(|p| p.bottom) {
        for p in part.pins.iter_mut() {
            if use_x {
                p.x = 2.0 * fold - p.x;
            } else {
                p.y = 2.0 * fold - p.y;
            }
        }
    }
    warnings.push(format!(
        "butterfly layout folded on {}",
        if use_x { "X" } else { "Y" }
    ));
}

// ---------------------------------------------------------------------------
// Parse
// ---------------------------------------------------------------------------

pub fn parse(data: &[u8]) -> Result<Board, String> {
    let text = to_text(data);
    let all = sections(&text);
    if all.is_empty() {
        return Err("no GenCAD sections found".to_string());
    }

    let mut warnings: Vec<String> = Vec::new();

    let f = units_factor(&all, &mut warnings);
    let pads = read_pads(&all, f);
    let stacks = read_padstacks(&all, &pads);
    let mut shapes = read_shapes(&all, f);
    let comps = read_components(&all, f, &mut warnings);
    let signals = read_signals(&all);
    let devices = read_devices(&all);

    center_shapes(&mut shapes, &comps);

    // 9. Build parts.
    let mut board = Board::default();
    let mut missing_shape = 0usize;

    for c in &comps {
        let Some(shape) = shapes.get(&c.shape) else {
            missing_shape += 1;
            continue;
        };
        let (sin, cos) = c.rot.to_radians().sin_cos();

        let pins: Vec<Pin> = shape
        .pins
        .iter()
        .map(|sp| {
            let (mut x, mut y) = (sp.x, sp.y);
            match c.mirror {
                Mirror::Y => x = -x,
                Mirror::X => y = -y,
                Mirror::None => {}
            }
            let rx = x * cos - y * sin;
            let ry = x * sin + y * cos;
            let net = signals
            .get(&(c.name.clone(), sp.name.clone()))
            .cloned()
            .unwrap_or_default();
            let radius = stacks
            .get(&sp.padstack)
            .or_else(|| pads.get(&sp.padstack))
            .copied()
            .unwrap_or(6.0);
            Pin {
                number: sp.name.clone(),
             name: sp.name.clone(),
             net,
             x: rx + c.x,
             y: ry + c.y,
             radius,
            }
        })
        .collect();

        let (value, pn) = devices
        .get(&c.device)
        .cloned()
        .unwrap_or_default();
        let description = if !value.is_empty() {
            Some(value)
        } else if !c.device.is_empty() && c.device != format!("Device {}", c.name) {
            Some(c.device.clone())
        } else {
            None
        };

        board.parts.push(Part {
            refdes: c.name.clone(),
                         insertion_code: if shape.through { "2" } else { "1" }.to_string(),
                         sym_name: c.shape.clone(),
                         bottom: c.bottom,
                         rotation: Some(c.rot),
                         part_number: if pn.is_empty() { None } else { Some(pn) },
                         description,
                         pins,
        });
    }

    if missing_shape > 0 {
        warnings.push(format!(
            "{} components reference a missing shape (skipped)",
                              missing_shape
        ));
    }

    fold_butterfly(&mut board, &mut warnings);

    if board.parts.is_empty() {
        warnings.push("no parts were produced".to_string());
    }
    board.warnings = warnings;
    Ok(board)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn file(units: &str, body: &str) -> String {
        format!("$HEADER\nGENCAD 1.4\nUNITS {}\n$ENDHEADER\n{}", units, body)
    }

    /// Two-pin shape S2: pins "1" at (-10, 0) and "2" at (10, 0), padstack PS.
    const SHAPE_S2: &str = "$SHAPES\nSHAPE S2\nPIN 1 PS -10 0 TOP 0 0\nPIN 2 PS 10 0 TOP 0 0\n$ENDSHAPES\n";

    #[test]
    fn detects_gencad() {
        assert!(is_gencad(b"$HEADER\nGENCAD 1.4\nUNITS INCH\n$ENDHEADER\n"));
        assert!(is_gencad(b"\xEF\xBB\xBF  $header\r\ngencad 1.4\r\n"));
        assert!(!is_gencad(b"hello"));
        assert!(!is_gencad(b"$HEADER\nSOMETHING ELSE\n"));
    }

    #[test]
    fn inch_units_and_placement() {
        let text = file(
            "INCH",
            "$SHAPES\nSHAPE S2\nPIN 1 P1 -0.01 0 TOP 0 0\nPIN 2 P1 0.01 0 TOP 0 0\n$ENDSHAPES\n\
$COMPONENTS\nCOMPONENT R1\nPLACE 1 2\nLAYER TOP\nROTATION 0\nSHAPE S2 0 0\n$ENDCOMPONENTS\n",
        );
        let board = parse(text.as_bytes()).unwrap();
        assert_eq!(board.parts.len(), 1);
        let p = &board.parts[0];
        assert_eq!(p.refdes, "R1");
        assert!(close(p.pins[0].x, 990.0) && close(p.pins[0].y, 2000.0));
        assert!(close(p.pins[1].x, 1010.0) && close(p.pins[1].y, 2000.0));
        assert_eq!(p.pins[0].radius, 6.0);
    }

    #[test]
    fn mirror_and_rotation_on_bottom() {
        let text = file(
            "THOU",
            "$SHAPES\nSHAPE SX\nPIN 1 PS 10 0 TOP 0 0\nPIN 2 PS -10 0 TOP 0 0\n$ENDSHAPES\n\
$COMPONENTS\nCOMPONENT U1\nPLACE 100 100\nLAYER BOTTOM\nROTATION 90\nSHAPE SX MIRRORY FLIP\n$ENDCOMPONENTS\n",
        );
        let board = parse(text.as_bytes()).unwrap();
        let p = &board.parts[0];
        assert!(p.bottom);
        assert_eq!(p.rotation, Some(90.0));
        let pin1 = p.pins.iter().find(|q| q.number == "1").unwrap();
        assert!(close(pin1.x, 100.0), "{}", pin1.x);
        assert!(close(pin1.y, 90.0), "{}", pin1.y);
    }

    #[test]
    fn pad_sizes_from_padstacks() {
        let text = file(
            "THOU",
            "$PADS\n\
PAD P1 RECTANGULAR 0\nRECTANGLE 0 0 20 30\n\
PAD P2 ROUND 0\nCIRCLE 0 0 8\n\
PAD PM ROUND 0\nCIRCLE 0 0 2\n\
$ENDPADS\n\
$PADSTACKS\n\
PADSTACK S1 0\nPAD P1 TOP 0 0\nPAD PM SOLDERMASK_TOP 0 0\n\
PADSTACK S2 0\nPAD P2 TOP 0 0\n\
$ENDPADSTACKS\n\
$SHAPES\nSHAPE SH\nPIN 1 S1 -10 0 TOP 0 0\nPIN 2 S2 10 0 TOP 0 0\n$ENDSHAPES\n\
$COMPONENTS\nCOMPONENT C1\nPLACE 0 0\nSHAPE SH\n$ENDCOMPONENTS\n",
        );
        let board = parse(text.as_bytes()).unwrap();
        let p = &board.parts[0];
        assert!(close(p.pins[0].radius, 10.0), "{}", p.pins[0].radius);
        assert!(close(p.pins[1].radius, 8.0), "{}", p.pins[1].radius);
    }

    #[test]
    fn signals_give_nets() {
        let text = file(
            "THOU",
            &format!(
                "{}$COMPONENTS\nCOMPONENT C1\nPLACE 0 0\nSHAPE S2\n$ENDCOMPONENTS\n\
$SIGNALS\nSIGNAL GND\nNODE C1 1\nSIGNAL \"NET A\"\nNODE C1 2\n$ENDSIGNALS\n",
SHAPE_S2
            ),
        );
        let board = parse(text.as_bytes()).unwrap();
        let p = &board.parts[0];
        assert_eq!(p.pins[0].net, "GND");
        assert_eq!(p.pins[1].net, "NET A");
    }

    #[test]
    fn devices_give_value_and_part_number() {
        let text = file(
            "THOU",
            &format!(
                "{}$COMPONENTS\nCOMPONENT C1\nPLACE 0 0\nSHAPE S2\nDEVICE DEV1\n$ENDCOMPONENTS\n\
$DEVICES\nDEVICE DEV1\nPART \"CAP 10UF (0402)//MURATA/GRM155\"\n$ENDDEVICES\n",
                     SHAPE_S2
            ),
        );
        let board = parse(text.as_bytes()).unwrap();
        let p = &board.parts[0];
        assert_eq!(p.description.as_deref(), Some("CAP 10UF (0402)"));
        assert_eq!(p.part_number.as_deref(), Some("MURATA/GRM155"));
    }

    #[test]
    fn keeps_only_last_revision() {
        let text = file(
            "THOU",
            &format!(
                "{}$COMPONENTS\n\
COMPONENT A\nPLACE 0 0\nSHAPE S2\n\
COMPONENT B\nPLACE 100 0\nSHAPE S2\n\
COMPONENT A\nPLACE 1000 0\nSHAPE S2\n\
COMPONENT B\nPLACE 1100 0\nSHAPE S2\n\
$ENDCOMPONENTS\n",
SHAPE_S2
            ),
        );
        let board = parse(text.as_bytes()).unwrap();
        assert_eq!(board.parts.len(), 2);
        let a = board.part("A").unwrap();
        assert!(close(a.pins[0].x, 990.0), "{}", a.pins[0].x);
        assert!(board.warnings.iter().any(|w| w.contains("revisions")), "{:?}", board.warnings);
    }

    #[test]
    fn drops_back_to_back_duplicates() {
        let text = file(
            "THOU",
            &format!(
                "{}$COMPONENTS\n\
COMPONENT C1\nPLACE 50 50\nLAYER TOP\nROTATION 0\nSHAPE S2\n\
COMPONENT C1\nPLACE 50 50\nLAYER TOP\nROTATION 0\nSHAPE S2\n\
$ENDCOMPONENTS\n",
SHAPE_S2
            ),
        );
        let board = parse(text.as_bytes()).unwrap();
        assert_eq!(board.parts.len(), 1);
        assert!(!board.warnings.iter().any(|w| w.contains("revisions")));
    }

    #[test]
    fn dedupes_shape_pins_in_two_frames() {
        let text = file(
            "THOU",
            "$SHAPES\nSHAPE D\n\
PIN 1 PS -10 0 TOP 0 0\nPIN 2 PS 10 0 TOP 0 0\nPIN 3 PS 0 10 TOP 0 0\n\
PIN 1 PS 490 500 TOP 0 0\nPIN 2 PS 510 500 TOP 0 0\nPIN 3 PS 500 510 TOP 0 0\n\
$ENDSHAPES\n\
$COMPONENTS\nCOMPONENT U1\nPLACE 0 0\nSHAPE D\n$ENDCOMPONENTS\n",
        );
        let board = parse(text.as_bytes()).unwrap();
        let p = &board.parts[0];
        assert_eq!(p.pins.len(), 3);
        let names: Vec<&str> = p.pins.iter().map(|q| q.number.as_str()).collect();
        assert_eq!(names, vec!["1", "2", "3"]);
        for q in &p.pins {
            assert!(q.x.abs() < 50.0 && q.y.abs() < 50.0, "{:?}", q);
        }
    }

    /// Run with: GENCAD_SAMPLE=path/to/file.cad cargo test gencad::tests::real_file -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_file() {
        let path = match std::env::var("GENCAD_SAMPLE") {
            Ok(p) => p,
            Err(_) => {
                eprintln!("note: set GENCAD_SAMPLE to a GenCAD file to run this test");
                return;
            }
        };
        let data = std::fs::read(&path).expect("read GENCAD_SAMPLE");
        let factor = units_factor(&sections(&to_text(&data)), &mut Vec::new());
        let board = parse(&data).expect("parse");

        let top = board.parts.iter().filter(|p| !p.bottom).count();
        let bottom = board.parts.len() - top;
        println!("units factor:  {} mils per file unit", factor);
        println!("parts:         {}", board.parts.len());
        println!("total pins:    {}", board.pin_count());
        println!("distinct nets: {}", board.net_names().len());
        println!("top / bottom:  {} / {}", top, bottom);
        println!("warnings ({}):", board.warnings.len());
        for w in &board.warnings {
            println!("  {}", w);
        }
        for p in board.parts.iter().take(3) {
            let mut shown = p.clone();
            shown.pins.truncate(4);
            println!("{:#?}", shown);
        }
    }
}
