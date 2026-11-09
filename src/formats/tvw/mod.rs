//! Teboview (.tvw) boardview reader.
//!
//! Ported from BoardRipper's `tvw-parser.ts` (AGPL-3.0), which is itself
//! derived from eagleview by Pavel Kovalenko (MIT).
//!
//! File layout (in order): obfuscated text header, layers, net table,
//! probe/fixture region (skipped), parts. Coordinates are in mils, Y up.

pub mod reader;
pub mod layers;
pub mod parts;
#[cfg(test)]
pub mod testbuf;

use std::collections::HashMap;
use std::fmt;

use crate::model::{Board, PadGeom, PadKind, Part, Pin};
use layers::{load_layer, Layer, LayerKind, Shape, ShapeKind, LT_BOTTOM, LT_PLANE, LT_SIGNAL, LT_TOP};
use parts::{read_nets, read_parts, scan_for_net_table, skip_probe_region, TvwPart, TvwPin};
use reader::Reader;

// ---------------------------------------------------------------------------
// Header text cipher
// ---------------------------------------------------------------------------

/// Decodes the simple substitution cipher used for header strings.
pub fn decode_string(s: &str) -> String {
    let a = 'a' as i32;
    let j = 'j' as i32;
    let k = 'k' as i32;
    let z = 'z' as i32;
    let big_a = 'A' as i32;
    let big_z = 'Z' as i32;
    let d0 = '0' as i32;
    let d9 = '9' as i32;

    s.chars()
    .enumerate()
    .map(|(i, ch)| {
        let c = ch as i32;
        let i = i as i32;
        let out = if (a..=j).contains(&c) {
            let mut x = c - (i % 3) - 4;
            if x < a {
                x += 10;
            }
            154 - x
        } else if (k..=z).contains(&c) {
            let mut x = c - (i % 10) - 5;
            if x < k {
                x += 16;
            }
            x
        } else if (big_a..=big_z).contains(&c) {
            let mut x = c + (i % 10) + 5;
            if x > big_z {
                x -= 26;
            }
            x
        } else if (d0..=d9).contains(&c) {
            let mut x = c + (i % 3) + 4;
            if x > d9 {
                x -= 10;
            }
            x + 49
        } else {
            return ch;
        };
        u32::try_from(out)
        .ok()
        .and_then(char::from_u32)
        .unwrap_or(ch)
    })
    .collect()
}

/// True if the data starts with a pstr that decodes to text beginning "Tebo".
pub fn is_tvw(data: &[u8]) -> bool {
    let n = match data.first() {
        Some(&n) => n as usize,
        None => return false,
    };
    if !(1..=60).contains(&n) || data.len() < 1 + n {
        return false;
    }
    let raw: String = data[1..1 + n].iter().map(|&b| b as char).collect();
    decode_string(&raw).starts_with("Tebo")
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// How many output pins got each kind of pad shape.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PadShapeCounts {
    pub circle: usize,
    pub rect: usize,
    pub roundrect: usize,
    /// Polygon shapes approximated by their bounding rectangle.
    pub poly_rect: usize,
    /// No usable shape (missing, unknown type or zero size): round fallback.
    pub none: usize,
}

impl fmt::Display for PadShapeCounts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pad shapes: circle {}, rect {}, roundrect {}, poly->rect {}, none {}",
            self.circle, self.rect, self.roundrect, self.poly_rect, self.none
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub kind: String,
    pub customer: String,
    pub date: String,
    pub layers: Vec<String>,
    pub nets: usize,
    pub tvw_parts: usize,
    pub pad_shapes: PadShapeCounts,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

pub fn parse(data: &[u8]) -> Result<Board, String> {
    parse_with_report(data).map(|(board, _)| board)
}

pub fn parse_with_report(data: &[u8]) -> Result<(Board, Report), String> {
    let mut r = Reader::new(data);
    let mut report = Report::default();
    let mut warnings: Vec<String> = Vec::new();

    // HEADER
    report.kind = decode_string(&r.pstr()?);
    r.u32()?;
    report.customer = decode_string(&r.pstr()?);
    r.pstr()?; // password
    report.date = decode_string(&r.pstr()?);
    r.pstr()?;
    r.pstr()?;
    r.pstr()?;
    r.u32()?;
    r.u32()?;
    r.u32()?;
    let count_pos = r.pos();
    let layer_count = r.u32()?;
    if layer_count > 1000 {
        return Err(format!(
            "at byte 0x{:x}: too many layers: {}",
            count_pos, layer_count
        ));
    }

    // LAYERS
    let mut layers: Vec<Layer> = Vec::new();
    let mut clean = true;
    for i in 0..layer_count {
        let start = r.pos();
        match load_layer(&mut r) {
            Ok(Some(l)) => {
                report.layers.push(format!(
                    "layer {}: \"{}\" type {} {:?} shapes {} pads {} lines {} holes {}",
                    i,
                    l.name,
                    l.layer_type,
                    l.kind,
                    l.shapes.len(),
                                           l.pads.len(),
                                           l.line_count,
                                           l.holes.len()
                ));
                layers.push(l);
            }
            Ok(None) => {
                clean = false;
                warnings.push(format!(
                    "layer {} at byte 0x{:x}: unknown object type, stopped reading layers",
                    i, start
                ));
                break;
            }
            Err(e) => {
                clean = false;
                warnings.push(format!(
                    "layer {} at byte 0x{:x}: {}, stopped reading layers",
                    i, start, e
                ));
                break;
            }
        }
    }

    // NETS
    let nets: Vec<String> = if clean {
        let start = r.pos();
        match read_nets(&mut r) {
            Ok(n) => n,
            Err(e) => {
                warnings.push(format!("net table at byte 0x{:x}: {}", start, e));
                recover_nets(&mut r, data, start, &mut warnings)
            }
        }
    } else {
        let from = r.pos();
        recover_nets(&mut r, data, from, &mut warnings)
    };
    report.nets = nets.len();

    // PROBES
    let probe_start = r.pos();
    if let Err(e) = skip_probe_region(&mut r) {
        warnings.push(format!(
            "probe/fixture region at byte 0x{:x}: {}",
            probe_start, e
        ));
    }

    // PARTS
    let (tvw_parts, part_warnings) = read_parts(&mut r, probe_start);
    warnings.extend(part_warnings);
    report.tvw_parts = tvw_parts.len();

    // CONVERT
    let (mut board, pad_shapes) = convert(&layers, &nets, &tvw_parts);
    report.pad_shapes = pad_shapes;
    warnings.append(&mut board.warnings);
    board.warnings = warnings;

    Ok((board, report))
}

/// Scans for the net table from `from`; reads it if found.
fn recover_nets(
    r: &mut Reader,
    data: &[u8],
    from: usize,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    match scan_for_net_table(data, from) {
        Some((p, n)) => {
            let mut nets = Vec::with_capacity(n);
            if let Err(e) = r.seek(p) {
                warnings.push(format!("could not seek to net table: {}", e));
                return nets;
            }
            for _ in 0..n {
                match r.pstr() {
                    Ok(s) => nets.push(s),
                    Err(e) => {
                        warnings.push(format!("net table cut short: {}", e));
                        break;
                    }
                }
            }
            warnings.push(format!(
                "recovered net table at byte 0x{:x} ({} nets)",
                                  p,
                                  nets.len()
            ));
            nets
        }
        None => {
            warnings.push("could not locate net table; pins will have no nets".to_string());
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Conversion to the format-neutral model
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Counters {
    non_copper: usize,
    pins_without_pad: usize,
    pads_without_shape: usize,
    parts_without_pins: usize,
    pad_shapes: PadShapeCounts,
}

/// Hands out unique refdes names: C1, then C1_2, C1_3, ...
#[derive(Default)]
struct Namer {
    used: HashMap<String, usize>,
    renamed: usize,
    example: Option<(String, String)>,
}

impl Namer {
    fn unique(&mut self, name: &str) -> String {
        let mut n = match self.used.get(name) {
            None => {
                self.used.insert(name.to_string(), 1);
                return name.to_string();
            }
            Some(&n) => n,
        };
        let candidate = loop {
            n += 1;
            let c = format!("{}_{}", name, n);
            if !self.used.contains_key(&c) {
                break c;
            }
        };
        self.used.insert(name.to_string(), n);
        self.used.insert(candidate.clone(), 1);
        self.renamed += 1;
        if self.example.is_none() {
            self.example = Some((name.to_string(), candidate.clone()));
        }
        candidate
    }
}

fn is_copper(layer: &Layer) -> bool {
    layer.kind == LayerKind::Logic
    && matches!(layer.layer_type, LT_TOP | LT_BOTTOM | LT_SIGNAL | LT_PLANE)
}

/// Real pad geometry from a TVW aperture shape (normalized), counting the result.
fn pad_from_shape(shape: Option<&Shape>, counts: &mut PadShapeCounts) -> Option<PadGeom> {
    let geom = shape.and_then(|s| {
        if s.width <= 0.0 || s.height <= 0.0 {
            return None;
        }
        let rect = |kind: PadKind, w: f64, h: f64| PadGeom {
            kind,
            w,
            h,
            angle: s.turn,
        };
        match s.kind {
            ShapeKind::Round => {
                counts.circle += 1;
                Some(PadGeom::circle(s.width.max(s.height)))
            }
            ShapeKind::Rect => {
                counts.rect += 1;
                Some(rect(PadKind::Rect, s.width, s.height))
            }
            ShapeKind::RoundRect => {
                counts.roundrect += 1;
                Some(rect(PadKind::RoundRect, s.width, s.height))
            }
            ShapeKind::Poly => {
                counts.poly_rect += 1;
                let (w, h) = match s.polygon.as_ref().filter(|p| p.len() >= 3) {
                    Some(pts) => {
                        let min_x = pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
                        let max_x = pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
                        let min_y = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
                        let max_y = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
                        (max_x - min_x, max_y - min_y)
                    }
                    None => (s.width, s.height),
                };
                if w > 0.0 && h > 0.0 {
                    Some(rect(PadKind::Rect, w, h))
                } else {
                    counts.poly_rect -= 1;
                    None
                }
            }
            ShapeKind::Other(_) => None,
        }
    });
    match geom {
        Some(g) => Some(g.normalized()),
        None => {
            counts.none += 1;
            None
        }
    }
}

fn resolve_pins(pins: &[TvwPin], layer: &Layer, nets: &[String], c: &mut Counters) -> Vec<Pin> {
    let mut out = Vec::with_capacity(pins.len());
    for pin in pins {
        let pad = match layer.pads.get((pin.handle / 8) as usize) {
            Some(p) => p,
            None => {
                c.pins_without_pad += 1;
                continue;
            }
        };
        let net = if pad.net >= 0 && (pad.net as usize) < nets.len() {
            nets[pad.net as usize].clone()
        } else {
            String::new()
        };
        let shape = layer.shape_for(pad.dcode);
        let radius = match shape {
            Some(s) => s.width.max(s.height) / 2.0,
            None => {
                c.pads_without_shape += 1;
                10.0
            }
        };
        let geom = pad_from_shape(shape, &mut c.pad_shapes);
        out.push(Pin {
            number: pin.id.to_string(),
                 name: pin.name.clone(),
                 net,
                 x: pad.x,
                 y: pad.y,
                 radius,
                 pad: geom,
        });
    }
    out
}

fn make_part(refdes: String, tp: &TvwPart, bottom: bool, pins: Vec<Pin>) -> Part {
    let description = format!("{} {}", tp.value, tp.package).trim().to_string();
    Part {
        refdes,
        insertion_code: "1".to_string(),
        sym_name: tp.package.clone(),
        bottom,
        rotation: Some(tp.angle as f64),
        part_number: if tp.serial.is_empty() {
            None
        } else {
            Some(tp.serial.clone())
        },
        description: if description.is_empty() {
            None
        } else {
            Some(description)
        },
        pins,
    }
}

fn convert(layers: &[Layer], nets: &[String], tvw_parts: &[TvwPart]) -> (Board, PadShapeCounts) {
    let mut board = Board::default();
    let mut c = Counters::default();
    let mut namer = Namer::default();

    for tp in tvw_parts {
        let layer = match layers.get(tp.layer as usize) {
            Some(l) if is_copper(l) => l,
            _ => {
                c.non_copper += 1;
                continue;
            }
        };
        let bottom = layer.layer_type == LT_BOTTOM;
        let mut produced = false;

        let pins = resolve_pins(&tp.pins, layer, nets, &mut c);
        if !pins.is_empty() {
            let refdes = namer.unique(&tp.name);
            board.parts.push(make_part(refdes, tp, bottom, pins));
            produced = true;
        }

        if let Some(ext) = &tp.pins_ext {
            let want = if bottom { LT_TOP } else { LT_BOTTOM };
            let opposite = layers
            .iter()
            .find(|l| l.kind == LayerKind::Logic && l.layer_type == want);
            match opposite {
                Some(ol) => {
                    let ext_pins = resolve_pins(ext, ol, nets, &mut c);
                    if !ext_pins.is_empty() {
                        let refdes = namer.unique(&format!("{}_2", tp.name));
                        board.parts.push(make_part(refdes, tp, !bottom, ext_pins));
                        produced = true;
                    }
                }
                None => c.pins_without_pad += ext.len(),
            }
        }

        if !produced {
            c.parts_without_pins += 1;
        }
    }

    let w = &mut board.warnings;
    if c.non_copper > 0 {
        w.push(format!("{} parts skipped (non-copper layer)", c.non_copper));
    }
    if c.pins_without_pad > 0 {
        w.push(format!("{} pins without pad (skipped)", c.pins_without_pad));
    }
    if c.pads_without_shape > 0 {
        w.push(format!(
            "{} pads without shape (radius 10 mils used)",
                       c.pads_without_shape
        ));
    }
    if c.parts_without_pins > 0 {
        w.push(format!("{} parts without pins (skipped)", c.parts_without_pins));
    }
    if namer.renamed > 0 {
        let (from, to) = namer.example.clone().unwrap_or_default();
        w.push(format!(
            "{} duplicate refdes renamed (e.g. {} -> {})",
                       namer.renamed, from, to
        ));
    }
    (board, c.pad_shapes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use layers::Pad;
    use testbuf::Buf;

    fn write_header(b: &mut Buf, layer_count: u32) {
        b.pstr("O95w-28ps49m 02v9o.")
        .u32(1)
        .pstr("G34vS4z")
        .pstr("")
        .pstr("H5y82 bj, cfbd")
        .pstr("")
        .pstr("")
        .pstr("")
        .u32(0)
        .u32(0)
        .u32(0)
        .u32(layer_count);
    }

    fn write_logic_layer(b: &mut Buf, name: &str, layer_type: u32, pads: &[(i32, f64, f64)]) {
        b.u32(0).u32(3).u32(2).u32(1);
        b.pstr(name).pstr(name).pstr("");
        b.u32(layer_type).u32(0).u32(0);
        // Shapes: max_dcode 11 -> one Round 20x20.
        b.u32(11);
        b.u32(1).fixed(20.0).fixed(20.0).u32(0).fixed(0.0).fixed(0.0);
        // data_order, 0, 1
        b.u32(1).u32(0).u32(1);
        // Pads.
        b.u32(pads.len() as u32).u32(2);
        for &(net, x, y) in pads {
            b.i32(net).u32(10).point(x, y).u8(0).u8(0).u8(0);
        }
        // Lines, arcs, surfaces.
        b.u32(0).u32(0).u32(0);
        // Unknown items.
        b.u32(0).u32(0).u32(7);
        // Testpoints.
        b.u32(0).u32(0).u32(4).u32(0).u32(0).u32(0).u32(0).u32(0).u32(0);
    }

    fn write_part(b: &mut Buf, name: &str, layer: u32, pins: &[(u32, u32, &str)]) {
        b.pstr(name)
        .point(0.0, 0.0)
        .point(10.0, 10.0)
        .point(5.0, 5.0)
        .i32(0) // angle
        .u32(0) // decal
        .u32(1) // part_type
        .u32(0)
        .i32(100) // height
        .u8(0) // flag0
        .pstr("10k")
        .pstr("")
        .pstr("")
        .pstr("0402")
        .u32(pins.len() as u32)
        .u32(layer)
        .u32(0);
        for &(handle, id, pname) in pins {
            b.u32(handle).u32(0).u32(id).pstr(pname).u32(0);
        }
    }

    fn sample_file() -> Vec<u8> {
        let mut b = Buf::new();
        write_header(&mut b, 2);
        write_logic_layer(&mut b, "TOP", LT_TOP, &[(0, 100.0, 100.0), (1, 140.0, 100.0)]);
        write_logic_layer(&mut b, "BOTTOM", LT_BOTTOM, &[(0, 500.0, 500.0)]);
        // Nets.
        b.zeros(16).u32(2).u32(2).pstr("GND").pstr("VCC");
        // Fake probe region: forces the parts scan.
        for _ in 0..30 {
            b.u8(0xEE);
        }
        // Parts.
        b.u32(3).u32(0);
        write_part(&mut b, "C1", 0, &[(0, 1, "1"), (8, 2, "2")]);
        write_part(&mut b, "C1", 0, &[(0, 1, "1"), (8, 2, "2")]);
        write_part(&mut b, "U1", 1, &[(0, 1, "A1")]);
        b.data.clone()
    }

    #[test]
    fn decodes_header_strings() {
        assert_eq!(decode_string("O95w-28ps49m 02v9o."), "Tebo-ictview files.");
        assert_eq!(decode_string("G34vS4z"), "LianBao");
        assert_eq!(decode_string("H5y82 bj, cfbd"), "March 25, 2021");
    }

    #[test]
    fn parses_small_file() {
        let data = sample_file();
        let (board, report) = parse_with_report(&data).unwrap();

        assert_eq!(report.kind, "Tebo-ictview files.");
        assert_eq!(report.customer, "LianBao");
        assert_eq!(report.date, "March 25, 2021");
        assert_eq!(report.layers.len(), 2);
        assert_eq!(report.nets, 2);
        assert_eq!(report.tvw_parts, 3);

        let names: Vec<&str> = board.parts.iter().map(|p| p.refdes.as_str()).collect();
        assert_eq!(names, vec!["C1", "C1_2", "U1"]);

        let c1 = &board.parts[0];
        assert!(!c1.bottom);
        assert_eq!(c1.pins.len(), 2);
        assert_eq!(c1.pins[0].net, "GND");
        assert_eq!(c1.pins[1].net, "VCC");
        assert_eq!(c1.pins[0].x, 100.0);
        assert_eq!(c1.pins[1].x, 140.0);
        assert_eq!(c1.pins[0].radius, 10.0);
        assert_eq!(c1.pins[0].pad, Some(PadGeom::circle(20.0)));
        assert_eq!(c1.pins[1].pad, Some(PadGeom::circle(20.0)));
        assert_eq!(c1.sym_name, "0402");
        assert_eq!(c1.description.as_deref(), Some("10k 0402"));

        let u1 = &board.parts[2];
        assert!(u1.bottom);
        assert_eq!(u1.pins[0].x, 500.0);
        assert_eq!(u1.pins[0].net, "GND");

        // C1 (2) + C1_2 (2) + U1 (1) pins, all Round.
        assert_eq!(report.pad_shapes.circle, 5);
        assert_eq!(report.pad_shapes.none, 0);

        assert!(
            board.warnings.iter().any(|w| w.contains("duplicate")),
                "{:?}",
                board.warnings
        );
    }

    #[test]
    fn rect_shape_becomes_normalized_rect_pad() {
        let layer = Layer {
            kind: LayerKind::Logic,
            name: "TOP".to_string(),
            layer_type: LT_TOP,
            shapes: vec![Shape {
                kind: ShapeKind::Rect,
                width: 30.0,
                height: 10.0,
                turn: 90.0,
                polygon: None,
            }],
            pads: vec![Pad {
                net: 0,
                dcode: 10,
                x: 1.0,
                y: 2.0,
                is_exposed: false,
                is_copper: true,
            }],
            line_count: 0,
            arc_count: 0,
            surface_count: 0,
            tool_sizes: Vec::new(),
            holes: Vec::new(),
        };
        let part = TvwPart {
            name: "R1".to_string(),
            bbox_min: (0.0, 0.0),
            bbox_max: (0.0, 0.0),
            x: 0.0,
            y: 0.0,
            angle: 0,
            part_type: 1,
            layer: 0,
            value: String::new(),
            package: String::new(),
            serial: String::new(),
            height: 0.0,
            pins: vec![TvwPin {
                handle: 0,
                id: 1,
                name: "1".to_string(),
            }],
            pins_ext: None,
        };

        let (board, counts) = convert(&[layer], &["GND".to_string()], &[part]);
        let pin = &board.parts[0].pins[0];
        assert_eq!(
            pin.pad,
            Some(PadGeom {
                kind: PadKind::Rect,
                w: 10.0,
                h: 30.0,
                angle: 0.0,
            })
        );
        assert_eq!(pin.radius, 15.0);
        assert_eq!(counts.rect, 1);
        assert_eq!(
            counts.to_string(),
                   "pad shapes: circle 0, rect 1, roundrect 0, poly->rect 0, none 0"
        );
    }

    #[test]
    fn detects_tvw() {
        assert!(is_tvw(&sample_file()));
        assert!(!is_tvw(b"hello"));
        assert!(!is_tvw(b""));
    }

    /// Run with: TVW_SAMPLE=path/to/file.tvw cargo test real_file -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_file() {
        let path = match std::env::var("TVW_SAMPLE") {
            Ok(p) => p,
            Err(_) => {
                eprintln!("note: set TVW_SAMPLE to a .tvw file to run this test");
                return;
            }
        };
        let data = std::fs::read(&path).expect("read TVW_SAMPLE");
        let (board, report) = parse_with_report(&data).expect("parse");

        println!("kind:      {}", report.kind);
        println!("customer:  {}", report.customer);
        println!("date:      {}", report.date);
        for l in &report.layers {
            println!("{}", l);
        }
        println!("nets:      {}", report.nets);
        println!("tvw parts: {}", report.tvw_parts);
        println!("{}", report.pad_shapes);

        println!("board parts:   {}", board.parts.len());
        println!("total pins:    {}", board.pin_count());
        println!("distinct nets: {}", board.net_names().len());
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
