//! Layer parsing for the Teboview (.tvw) boardview format.
//!
//! A Rust port of the layer parsing in BoardRipper's `tvw-parser.ts`
//! (AGPL-3.0), which is itself derived from eagleview by Pavel Kovalenko (MIT).
//!
//! All values are little-endian. "fixed" values are i32 in hundredths of a mil,
//! returned by the reader as mils (f64).
//!
//! Set the environment variable `TVW_TRACE=1` to print a parse trace to stderr.

use std::sync::OnceLock;

use crate::formats::tvw::reader::Reader;

pub const LT_TOP: u32 = 1;
pub const LT_BOTTOM: u32 = 2;
pub const LT_SIGNAL: u32 = 3;
pub const LT_PLANE: u32 = 4;

/// True when `TVW_TRACE=1` is set. Checked once, then cached.
fn trace_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("TVW_TRACE")
        .map(|v| v == "1")
        .unwrap_or(false)
    })
}

/// eprintln!("[tvw] ...") when tracing is on.
macro_rules! trace {
    ($($arg:tt)*) => {
        if trace_on() {
            eprintln!("[tvw] {}", format_args!($($arg)*));
        }
    };
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShapeKind {
    Round,
    Rect,
    RoundRect,
    Poly,
    Other(u32),
}

impl ShapeKind {
    fn from_type(t: u32) -> Self {
        match t {
            0 => ShapeKind::Round,
            1 => ShapeKind::Rect,
            3 => ShapeKind::RoundRect,
            5 => ShapeKind::Poly,
            other => ShapeKind::Other(other),
        }
    }
}

/// Pad aperture. Sizes in mils, `turn` in degrees, polygon points shape-local mils.
#[derive(Debug, Clone)]
pub struct Shape {
    pub kind: ShapeKind,
    pub width: f64,
    pub height: f64,
    pub turn: f64,
    pub polygon: Option<Vec<(f64, f64)>>,
}

#[derive(Debug, Clone)]
pub struct Pad {
    pub net: i32,
    pub dcode: u32,
    pub x: f64,
    pub y: f64,
    pub is_exposed: bool,
    pub is_copper: bool,
}

#[derive(Debug, Clone)]
pub struct Hole {
    pub net: i32,
    pub tool: u32,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayerKind {
    Logic,
    Through,
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub kind: LayerKind,
    pub name: String,
    pub layer_type: u32,
    // Logic only
    pub shapes: Vec<Shape>,
    pub pads: Vec<Pad>,
    // Read and discarded; counts kept for diagnostics
    pub line_count: usize,
    pub arc_count: usize,
    pub surface_count: usize,
    // Through only
    pub tool_sizes: Vec<f64>,
    pub holes: Vec<Hole>,
}

impl Layer {
    fn empty(kind: LayerKind, name: String, layer_type: u32) -> Self {
        Layer {
            kind,
            name,
            layer_type,
            shapes: Vec::new(),
            pads: Vec::new(),
            line_count: 0,
            arc_count: 0,
            surface_count: 0,
            tool_sizes: Vec::new(),
            holes: Vec::new(),
        }
    }

    /// Shape for a pad's dcode: `shapes[dcode - 10]` if in range, else None.
    pub fn shape_for(&self, dcode: u32) -> Option<&Shape> {
        if dcode < 10 {
            return None;
        }
        self.shapes.get((dcode - 10) as usize)
    }
}

/// Skips `count * size` bytes, erroring (instead of overflowing) on garbage counts.
fn skip_n(r: &mut Reader, count: u32, size: usize, what: &str) -> Result<(), String> {
    let n = (count as usize).checked_mul(size).ok_or_else(|| {
        format!(
            "at byte 0x{:x}: {} count {} is too large",
            r.pos(),
                what,
                count
        )
    })?;
    r.skip(n)
}

/// Loads one layer. Returns Ok(None) for an unknown object type.
pub fn load_layer(r: &mut Reader) -> Result<Option<Layer>, String> {
    let layer_pos = r.pos();

    // 1. Object type: up to 4 u32, zeros are padding.
    let mut kind = None;
    let mut objtype = 0u32;
    for _ in 0..4 {
        let v = r.u32()?;
        match v {
            0 => continue,
            1 => {
                objtype = v;
                kind = Some(LayerKind::Through);
                break;
            }
            3 => {
                objtype = v;
                kind = Some(LayerKind::Logic);
                break;
            }
            _ => return Ok(None),
        }
    }
    let kind = match kind {
        Some(k) => k,
        None => return Ok(None),
    };

    // 2. Layer header.
    let hdr_pos = r.pos();
    let a = r.u32()?;
    let b = r.u32()?;
    if a != 2 || b != 1 {
        eprintln!(
            "warning: at byte 0x{:x}: unexpected layer header values {}, {} (expected 2, 1)",
                  hdr_pos, a, b
        );
    }
    let name = r.pstr()?;
    let _initial_name = r.pstr()?;
    let _path = r.pstr()?;
    let layer_type = r.u32()?;
    let _pad_color = r.u32()?;
    let _line_color = r.u32()?;

    trace!(
        "layer @0x{:x} objtype {} name {:?} type {}",
        layer_pos,
        objtype,
        name,
        layer_type
    );

    let mut layer = Layer::empty(kind.clone(), name, layer_type);

    // 3. Body.
    match kind {
        LayerKind::Logic => load_logic(r, &mut layer)?,
        LayerKind::Through => load_through(r, &mut layer)?,
    }
    Ok(Some(layer))
}

fn load_shapes(r: &mut Reader) -> Result<Vec<Shape>, String> {
    let shapes_pos = r.pos();
    let max_dcode = r.u32()?;
    let mut shapes = Vec::new();
    if max_dcode == 0 {
        trace!("  shapes @0x{:x} max_dcode 0 count 0", shapes_pos);
        return Ok(shapes);
    }
    let count = max_dcode.saturating_sub(10);
    trace!(
        "  shapes @0x{:x} max_dcode {} count {}",
        shapes_pos,
        max_dcode,
        count
    );

    for i in 0..count {
        let _marker = r.u32()?;
        let width = r.fixed()?;
        let height = r.fixed()?;
        let type_pos = r.pos();
        let t = r.u32()?;
        let mut turn = 0.0;
        let mut polygon: Option<Vec<(f64, f64)>> = None;

        if !matches!(t, 0 | 1 | 3) {
            trace!("    shape #{} type {} @0x{:x}", i, t, type_pos);
        }

        match t {
            0 => {
                r.fixed()?;
                r.fixed()?;
            }
            1 | 3 => {
                turn = r.f32()? as f64;
                r.i32()?;
            }
            5 => {
                r.u32()?;
                r.pstr()?;
                for _ in 0..4 {
                    r.fixed()?;
                }
                let sub_count = r.u32()?;
                for _ in 0..sub_count {
                    let sub_pos = r.pos();
                    let sub_type = r.u32()?;
                    trace!("      poly sub type {} @0x{:x}", sub_type, sub_pos);
                    match sub_type {
                        2 => {
                            r.skip(12)?;
                            let n = r.u32()?;
                            let mut pts = Vec::new();
                            for _ in 0..n {
                                pts.push(r.point()?);
                            }
                            if polygon.is_none() && pts.len() >= 3 {
                                polygon = Some(pts);
                            }
                        }
                        5 => {
                            r.u32()?;
                            r.u32()?;
                            r.u32()?;
                            for _ in 0..4 {
                                r.fixed()?;
                            }
                            r.i32()?;
                        }
                        _ => {} // as in the original: nothing more is read
                    }
                }
            }
            other => {
                eprintln!(
                    "warning: at byte 0x{:x}: unknown shape type {}",
                    type_pos, other
                );
                r.fixed()?;
                r.fixed()?;
            }
        }

        shapes.push(Shape {
            kind: ShapeKind::from_type(t),
                    width,
                    height,
                    turn,
                    polygon,
        });
    }
    Ok(shapes)
}

fn load_lines(r: &mut Reader, layer: &mut Layer) -> Result<(), String> {
    let pos = r.pos();
    let count = r.u32()?;
    trace!("  lines @0x{:x} count {}", pos, count);
    if count > 0 {
        r.u32()?;
        skip_n(r, count, 24, "line")?;
        layer.line_count += count as usize;
    }
    Ok(())
}

fn load_arcs(r: &mut Reader, layer: &mut Layer) -> Result<(), String> {
    let pos = r.pos();
    let count = r.u32()?;
    trace!("  arcs @0x{:x} count {}", pos, count);
    if count > 0 {
        r.u32()?;
        skip_n(r, count, 28, "arc")?;
        layer.arc_count += count as usize;
    }
    Ok(())
}

fn load_surfaces(r: &mut Reader, layer: &mut Layer) -> Result<(), String> {
    let pos = r.pos();
    let count = r.u32()?;
    trace!("  surfaces @0x{:x} count {}", pos, count);
    if count > 0 {
        r.u32()?;
        for i in 0..count {
            let surf_pos = r.pos();
            let net = r.i32()?;
            let n = r.u32()?;
            skip_n(r, n, 8, "surface point")?;
            let _line_width = r.i32()?;
            let voids = r.u32()?;
            trace!(
                "    surface #{} @0x{:x} net {} edges {} voids {}",
                i,
                surf_pos,
                net,
                n,
                voids
            );
            if voids > 0 {
                for j in 0..voids {
                    let void_pos = r.pos();
                    let _tag = r.u32()?;
                    let m = r.u32()?;
                    trace!("      void #{} @0x{:x} edges {}", j, void_pos, m);
                    skip_n(r, m, 8, "void point")?;
                }
                // Flags follow the void list, and only exist when there are voids.
                let _flags = r.u32()?;
            }
        }
        layer.surface_count += count as usize;
    }
    Ok(())
}

fn load_pads(r: &mut Reader, layer: &mut Layer) -> Result<(), String> {
    let pos = r.pos();
    let count = r.u32()?;
    trace!("  pads @0x{:x} count {}", pos, count);
    if count == 0 {
        trace!("  pads end @0x{:x}", r.pos());
        return Ok(());
    }
    r.u32()?; // marker
    for _ in 0..count {
        let net = r.i32()?;
        let dcode = r.u32()?;
        let (x, y) = r.point()?;
        let is_exposed = r.u8()? != 0;
        let is_copper = r.u8()? != 0;
        let tp_param = r.u8()?;
        if is_copper {
            let something = r.u8()? != 0;
            if tp_param == 1 {
                r.skip(12)?;
            }
            if is_exposed || something {
                r.skip(16)?;
            }
            let has_hole = r.u8()? != 0;
            r.u8()?;
            if has_hole {
                r.skip(7)?;
                r.skip(8)?;
                r.skip(1)?;
            }
        }
        layer.pads.push(Pad {
            net,
            dcode,
            x,
            y,
            is_exposed,
            is_copper,
        });
    }
    trace!("  pads end @0x{:x}", r.pos());
    Ok(())
}

fn load_logic(r: &mut Reader, layer: &mut Layer) -> Result<(), String> {
    layer.shapes = load_shapes(r)?;

    if !layer.shapes.is_empty() {
        let order_pos = r.pos();
        let data_order = r.u32()?;
        trace!("  data_order {} @0x{:x}", data_order, order_pos);
        r.u32()?;
        r.u32()?;

        load_pads(r, layer)?;
        load_lines(r, layer)?;
        load_arcs(r, layer)?;
        load_surfaces(r, layer)?;

        if data_order == 2 {
            trace!("  extra lines/arcs (data_order 2) @0x{:x}", r.pos());
            r.skip(16)?;
            load_lines(r, layer)?;
            load_arcs(r, layer)?;
            r.u32()?;
        }
    }

    // Unknown items: count, param; if count > 0 the items then ONE u32;
    // then ONE more u32, always.
    let unk_pos = r.pos();
    let count = r.u32()?;
    let _param = r.u32()?;
    trace!("  unknown items @0x{:x} count {}", unk_pos, count);
    if count > 0 {
        for _ in 0..count {
            r.pstr()?;
            r.skip(8)?;
            r.skip(24)?;
            r.skip(3)?;
            r.skip(4)?;
        }
        r.u32()?;
    }
    r.u32()?;

    // Testpoints.
    let tp_pos = r.pos();
    let n1 = r.u32()?;
    for _ in 0..n1 {
        r.u8()?;
        r.skip(16)?;
        r.skip(8)?;
        r.skip(4)?;
        r.u8()?;
        r.skip(8)?;
        r.skip(4)?;
    }
    r.u32()?;
    r.u32()?;

    let n2 = r.u32()?;
    r.u32()?;
    skip_n(r, n2, 54, "testpoint")?;

    let n3 = r.u32()?;
    r.u32()?;
    skip_n(r, n3, 54, "testpoint")?;

    let ts = r.u32()?;
    let ts_param = r.u32()?;
    trace!(
        "  testpoints @0x{:x} n1 {} n2 {} n3 {} ts {}",
        tp_pos,
        n1,
        n2,
        n3,
        ts
    );
    for _ in 0..ts {
        r.skip(8)?;
        r.u8()?;
        if ts_param == 1 {
            r.skip(12)?;
        }
    }
    trace!("  layer end @0x{:x}", r.pos());
    Ok(())
}

fn load_through(r: &mut Reader, layer: &mut Layer) -> Result<(), String> {
    r.u32()?;
    r.u32()?;
    let tools_pos = r.pos();
    let tool_count = r.u32()?;
    trace!("  tools @0x{:x} count {}", tools_pos, tool_count);
    if tool_count == 0 {
        trace!("  layer end @0x{:x}", r.pos());
        return Ok(());
    }
    for _ in 0..tool_count - 1 {
        r.u8()?;
        r.u8()?;
        let size = r.fixed()?;
        r.skip(20)?;
        r.skip(3)?;
        layer.tool_sizes.push(size);
    }
    r.u8()?;

    let drills_pos = r.pos();
    let drill_count = r.u32()?;
    trace!("  drills @0x{:x} count {}", drills_pos, drill_count);
    r.u32()?;
    r.skip(16)?;

    for _ in 0..drill_count {
        let code_pos = r.pos();
        let code = r.u8()?;
        match code {
            0x08 => {
                let net = r.i32()?;
                let tool = r.u32()?;
                let (x, y) = r.point()?;
                layer.holes.push(Hole { net, tool, x, y });
            }
            0x0A => {
                r.i32()?;
                r.u32()?;
                r.point()?;
                r.point()?;
                r.u32()?;
            }
            0x0B => {
                r.i32()?;
                r.u32()?;
                r.point()?;
                r.fixed()?;
                r.f32()?;
                r.f32()?;
            }
            other => {
                return Err(format!(
                    "unknown drill code 0x{:x} at byte 0x{:x}",
                    other, code_pos
                ));
            }
        }
    }
    trace!("  layer end @0x{:x}", r.pos());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::tvw::testbuf::Buf;

    fn header(b: &mut Buf) {
        b.u32(2)
        .u32(1)
        .pstr("TOP")
        .pstr("TOP")
        .pstr("c:\\x")
        .u32(LT_TOP)
        .u32(0)
        .u32(0);
    }

    #[test]
    fn logic_layer() {
        let mut b = Buf::new();
        b.u32(0).u32(3);
        header(&mut b);

        // Shapes: max_dcode 12 -> 2 shapes.
        b.u32(12);
        b.u32(1).fixed(20.0).fixed(20.0).u32(0).fixed(0.0).fixed(0.0);
        b.u32(1).fixed(30.0).fixed(10.0).u32(1).f32(90.0).i32(0);

        // data_order, 0, 1
        b.u32(1).u32(0).u32(1);

        // Pads.
        b.u32(2).u32(2);
        b.i32(5).u32(10).point(100.0, 200.0).u8(0).u8(0).u8(0);
        b.i32(-1)
        .u32(11)
        .point(300.0, 400.0)
        .u8(1)
        .u8(1)
        .u8(0)
        .u8(0) // something
        .zeros(16) // exposed bbox
        .u8(1) // has_hole
        .u8(0)
        .zeros(7 + 8 + 1);

        // Lines, arcs, surfaces.
        b.u32(1).u32(0).zeros(24);
        b.u32(0);
        b.u32(0);

        // Unknown items.
        b.u32(0).u32(0).u32(7);

        // Testpoints.
        b.u32(0);
        b.u32(0).u32(4);
        b.u32(0).u32(0);
        b.u32(0).u32(0);
        b.u32(0).u32(0);

        let data = b.data.clone();
        let mut r = Reader::new(&data);
        let layer = load_layer(&mut r).unwrap().expect("layer");

        assert_eq!(layer.kind, LayerKind::Logic);
        assert_eq!(layer.name, "TOP");
        assert_eq!(layer.layer_type, LT_TOP);
        assert_eq!(layer.shapes.len(), 2);

        assert_eq!(layer.shapes[0].kind, ShapeKind::Round);
        let s = &layer.shapes[1];
        assert_eq!(s.kind, ShapeKind::Rect);
        assert_eq!(s.width, 30.0);
        assert_eq!(s.height, 10.0);
        assert_eq!(s.turn, 90.0);

        assert_eq!(layer.pads.len(), 2);
        let p0 = &layer.pads[0];
        assert_eq!((p0.net, p0.dcode, p0.x, p0.y), (5, 10, 100.0, 200.0));
        assert!(!p0.is_copper && !p0.is_exposed);
        let p1 = &layer.pads[1];
        assert_eq!((p1.net, p1.dcode, p1.x, p1.y), (-1, 11, 300.0, 400.0));
        assert!(p1.is_copper && p1.is_exposed);

        assert_eq!(layer.line_count, 1);
        assert_eq!(layer.arc_count, 0);
        assert_eq!(layer.surface_count, 0);

        assert_eq!(layer.shape_for(11).unwrap().kind, ShapeKind::Rect);
        assert!(layer.shape_for(9).is_none());
        assert!(layer.shape_for(12).is_none());

        assert!(r.is_at_end(), "reader at 0x{:x} of 0x{:x}", r.pos(), data.len());
    }

    /// Logic layer up to (not including) the surfaces section:
    /// object type, header, one Round shape, data_order 1, one pad, lines 0, arcs 0.
    fn logic_until_surfaces(b: &mut Buf) {
        b.u32(0).u32(3);
        header(b);
        b.u32(11);
        b.u32(1).fixed(20.0).fixed(20.0).u32(0).fixed(0.0).fixed(0.0);
        b.u32(1).u32(0).u32(1);
        b.u32(1).u32(2);
        b.i32(5).u32(10).point(100.0, 200.0).u8(0).u8(0).u8(0);
        b.u32(0); // lines
        b.u32(0); // arcs
    }

    /// Everything after the surfaces section: unknown items and testpoints.
    fn logic_after_surfaces(b: &mut Buf) {
        b.u32(0).u32(0).u32(7);
        b.u32(0);
        b.u32(0).u32(4);
        b.u32(0).u32(0);
        b.u32(0).u32(0);
        b.u32(0).u32(0);
    }

    fn triangle(b: &mut Buf) {
        b.point(0.0, 0.0).point(10.0, 0.0).point(0.0, 10.0);
    }

    #[test]
    fn surfaces_with_and_without_voids() {
        let mut b = Buf::new();
        logic_until_surfaces(&mut b);

        b.u32(2).u32(2);
        // Surface A: no voids -> no flags u32.
        b.i32(-1).u32(3);
        triangle(&mut b);
        b.i32(0).u32(0);
        // Surface B: one void -> flags u32 after the void list.
        b.i32(7).u32(3);
        triangle(&mut b);
        b.i32(0).u32(1);
        b.u32(0).u32(3);
        triangle(&mut b);
        b.u32(0); // flags

        logic_after_surfaces(&mut b);

        let data = b.data.clone();
        let mut r = Reader::new(&data);
        let layer = load_layer(&mut r).unwrap().expect("layer");
        assert_eq!(layer.surface_count, 2);
        assert_eq!(layer.pads.len(), 1);
        assert!(r.is_at_end(), "reader at 0x{:x} of 0x{:x}", r.pos(), data.len());
    }

    #[test]
    fn single_surface_without_voids() {
        let mut b = Buf::new();
        logic_until_surfaces(&mut b);

        b.u32(1).u32(2);
        b.i32(-1).u32(3);
        triangle(&mut b);
        b.i32(0).u32(0); // voids 0, no flags

        logic_after_surfaces(&mut b);

        let data = b.data.clone();
        let mut r = Reader::new(&data);
        let layer = load_layer(&mut r).unwrap().expect("layer");
        assert_eq!(layer.surface_count, 1);
        assert!(r.is_at_end(), "reader at 0x{:x} of 0x{:x}", r.pos(), data.len());
    }

    #[test]
    fn through_layer() {
        let mut b = Buf::new();
        b.u32(0).u32(1);
        header(&mut b);
        b.u32(0).u32(0).u32(3);
        b.u8(0).u8(0).fixed(12.0).zeros(23);
        b.u8(0).u8(0).fixed(40.0).zeros(23);
        b.u8(0);

        b.u32(3).u32(0).zeros(16);
        b.u8(0x08).i32(2).u32(1).point(10.0, 20.0);
        b.u8(0x0A)
        .i32(2)
        .u32(0)
        .point(0.0, 0.0)
        .point(5.0, 5.0)
        .u32(0);
        b.u8(0x0B)
        .i32(2)
        .u32(0)
        .point(1.0, 1.0)
        .fixed(5.0)
        .f32(0.0)
        .f32(90.0);

        let data = b.data.clone();
        let mut r = Reader::new(&data);
        let layer = load_layer(&mut r).unwrap().expect("layer");

        assert_eq!(layer.kind, LayerKind::Through);
        assert_eq!(layer.tool_sizes, vec![12.0, 40.0]);
        assert_eq!(layer.holes.len(), 1);
        let h = &layer.holes[0];
        assert_eq!((h.net, h.tool, h.x, h.y), (2, 1, 10.0, 20.0));
        assert!(r.is_at_end());
    }

    #[test]
    fn unknown_drill_code_errors() {
        let mut b = Buf::new();
        b.u32(0).u32(1);
        header(&mut b);
        b.u32(0).u32(0).u32(1); // tool_count 1 -> zero tools
        b.u8(0);
        b.u32(1).u32(0).zeros(16);
        b.u8(0x09);

        let data = b.data.clone();
        let mut r = Reader::new(&data);
        let err = load_layer(&mut r).unwrap_err();
        assert!(err.contains("0x9"), "{}", err);
    }

    #[test]
    fn unknown_object_type_is_none() {
        let mut b = Buf::new();
        b.u32(7);
        let data = b.data.clone();
        let mut r = Reader::new(&data);
        assert!(load_layer(&mut r).unwrap().is_none());
    }

    #[test]
    fn through_layer_without_tools() {
        let mut b = Buf::new();
        b.u32(0).u32(1);
        header(&mut b);
        b.u32(0).u32(0).u32(0);

        let data = b.data.clone();
        let mut r = Reader::new(&data);
        let layer = load_layer(&mut r).unwrap().expect("layer");
        assert_eq!(layer.kind, LayerKind::Through);
        assert!(layer.tool_sizes.is_empty());
        assert!(layer.holes.is_empty());
        assert!(r.is_at_end());
    }
}
