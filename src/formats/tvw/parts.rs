//! Net table, probe/fixture skipping and parts parsing for the Teboview (.tvw)
//! boardview format.
//!
//! A Rust port of the corresponding parts of BoardRipper's `tvw-parser.ts`
//! (AGPL-3.0), which is itself derived from eagleview by Pavel Kovalenko (MIT).
//!
//! All values are little-endian. "fixed" values are i32 in hundredths of a mil,
//! returned by the reader as mils (f64).

use crate::formats::tvw::reader::Reader;

#[derive(Debug, Clone)]
pub struct TvwPin {
    pub handle: u32,
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct TvwPart {
    pub name: String,
    pub bbox_min: (f64, f64),
    pub bbox_max: (f64, f64),
    pub x: f64,
    pub y: f64,
    pub angle: i32,
    pub part_type: u32,
    /// Index into the file's layer list.
    pub layer: u32,
    pub value: String,
    pub package: String,
    pub serial: String,
    pub height: f64,
    pub pins: Vec<TvwPin>,
    /// Opposite-side pins (edge connectors).
    pub pins_ext: Option<Vec<TvwPin>>,
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn peek_u32_at(data: &[u8], pos: usize) -> Option<u32> {
    let end = pos.checked_add(4)?;
    if end > data.len() {
        return None;
    }
    let mut b = [0u8; 4];
    b.copy_from_slice(&data[pos..end]);
    Some(u32::from_le_bytes(b))
}

fn is_printable_byte(b: u8) -> bool {
    (0x20..=0x7E).contains(&b)
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

// ---------------------------------------------------------------------------
// 1. Net table
// ---------------------------------------------------------------------------

pub fn read_nets(r: &mut Reader) -> Result<Vec<String>, String> {
    r.skip(16)?;
    let count_pos = r.pos();
    let count = r.u32()?;
    let count2 = r.u32()?;
    if count != count2 {
        eprintln!(
            "warning: at byte 0x{:x}: net counts differ ({} vs {})",
                  count_pos, count, count2
        );
    }
    if count > 500_000 {
        return Err(format!(
            "at byte 0x{:x}: too many nets: {}",
            count_pos, count
        ));
    }
    let mut nets = Vec::with_capacity(count as usize);
    for _ in 0..count {
        nets.push(r.pstr()?);
    }
    Ok(nets)
}

/// Looks for `count, count, name, name, ...` with plausible printable names.
/// Returns (position of the first name, count).
pub fn scan_for_net_table(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let len = data.len();
    let mut p = from;
    while p + 8 <= len {
        let v1 = peek_u32_at(data, p)?;
        let v2 = peek_u32_at(data, p + 4)?;
        if v1 == v2 && (200..=50_000).contains(&v1) {
            let checked = v1.min(20) as usize;
            let mut q = p + 8;
            let mut non_empty = 0usize;
            let mut ok = true;
            for _ in 0..checked {
                if q >= len {
                    ok = false;
                    break;
                }
                let l = data[q] as usize;
                if l > 60 || q + 1 + l > len {
                    ok = false;
                    break;
                }
                if !data[q + 1..q + 1 + l].iter().all(|&b| is_printable_byte(b)) {
                    ok = false;
                    break;
                }
                if l > 0 {
                    non_empty += 1;
                }
                q += 1 + l;
            }
            if ok && non_empty >= checked.min(10) {
                return Some((p + 8, v1 as usize));
            }
        }
        p += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// 2. Probe / fixture / mysterious region
// ---------------------------------------------------------------------------

pub fn skip_probe_region(r: &mut Reader) -> Result<(), String> {
    skip_probe_registry(r)?;
    skip_fixture_registry(r)?;
    skip_mysterious_block(r)
}

fn skip_probe_data_item(r: &mut Reader) -> Result<(), String> {
    let present = r.u8()?;
    if present != 0 {
        r.skip(4)?;
        r.skip(20)?;
        r.skip(4)?;
    }
    Ok(())
}

fn skip_fixture_data(r: &mut Reader) -> Result<(), String> {
    r.u32()?;
    r.skip(24)?;
    r.skip(3)?;
    let n = r.u32()?;
    for _ in 0..n {
        skip_probe_data_item(r)?;
    }
    let boxes = r.u32()?;
    r.u32()?;
    r.skip(16)?;
    skip_n(r, boxes, 1 + 16, "fixture box")
}

fn skip_probe_data(r: &mut Reader) -> Result<(), String> {
    skip_fixture_data(r)?;
    r.skip(16)?;
    let n = r.u32()?;
    // u32 + i32 + 16 + i32 + 16
    skip_n(r, n, 4 + 4 + 16 + 4 + 16, "probe data")
}

fn skip_probe(r: &mut Reader) -> Result<(), String> {
    r.u8()?;
    r.u32()?;
    r.pstr()?;
    // size1, param1, size2, param2, size3, param3
    for _ in 0..6 {
        r.skip(4)?;
    }
    r.skip(4)?; // color
    r.skip(32)?;
    let has_body = r.u8()?;
    if has_body != 0 {
        skip_probe_data(r)?;
    }
    r.u32()?;
    r.skip(3)?;
    r.u8()?;
    r.skip(12)?;
    r.i32()?;
    r.skip(16)?;
    r.i32()?;
    r.skip(16)?;
    Ok(())
}

fn skip_probe_registry(r: &mut Reader) -> Result<(), String> {
    r.u32()?;
    r.u32()?;
    r.u32()?;
    r.pstr()?;
    r.u32()?;
    let packs = r.u32()?;
    for _ in 0..packs {
        let probes = r.u32()?;
        for _ in 0..probes {
            skip_probe(r)?;
        }
    }
    Ok(())
}

fn skip_fixture_variant(r: &mut Reader) -> Result<(), String> {
    r.pstr()?;
    r.pstr()?;
    r.u8()?;
    r.u8()?;
    skip_fixture_data(r)
}

fn skip_fixture_setting(r: &mut Reader) -> Result<(), String> {
    r.u32()?;
    r.pstr()?;
    r.u32()?;
    let n = r.u32()?;
    for _ in 0..n {
        skip_fixture_variant(r)?;
    }
    r.skip(8)
}

fn skip_fixture_registry(r: &mut Reader) -> Result<(), String> {
    r.u32()?;
    r.u32()?;
    for _ in 0..8 {
        r.pstr()?;
    }
    skip_fixture_setting(r)?; // top
    skip_fixture_setting(r)?; // bottom
    Ok(())
}

fn skip_mysterious_block(r: &mut Reader) -> Result<(), String> {
    r.u32()?;
    r.u32()?;
    r.skip(8)?;
    r.u32()?;
    r.u32()?;
    r.skip(2)?;
    r.skip(2)?;
    r.skip(16)?;
    r.skip(6)?;
    r.skip(16)?;

    // Some variants (e.g. LianBao) have 2 extra trailing bytes here.
    fn plausible(n: Option<u32>, s: Option<u32>) -> bool {
        match (n, s) {
            (Some(n), Some(s)) => n > 0 && n < 200_000 && s < 0x100,
            _ => false,
        }
    }
    let data = r.data();
    let pos = r.pos();
    let c1 = peek_u32_at(data, pos);
    let c1s = peek_u32_at(data, pos + 4);
    let c2 = peek_u32_at(data, pos + 2);
    let c2s = peek_u32_at(data, pos + 6);

    if plausible(c1, c1s) {
        // no trailing bytes
    } else if plausible(c2, c2s) {
        r.skip(2)?;
    } else {
        r.skip(2)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. Parts
// ---------------------------------------------------------------------------

fn load_pin(r: &mut Reader, trailing: bool) -> Result<(TvwPin, u32), String> {
    let handle = r.u32()?;
    r.u32()?;
    let id = r.u32()?;
    let name = r.pstr()?;
    let z2 = if trailing { r.u32()? } else { 0 };
    Ok((TvwPin { handle, id, name }, z2))
}

pub fn load_part(r: &mut Reader) -> Result<TvwPart, String> {
    let name = r.pstr()?;
    let bbox_min = r.point()?;
    let bbox_max = r.point()?;
    let (x, y) = r.point()?;
    let angle = r.i32()?; // plain degrees, not fixed
    let _decal = r.u32()?;
    let part_type = r.u32()?;
    r.u32()?;
    let height = r.i32()? as f64 / 100.0;
    let flag0 = r.u8()? != 0;
    let value = r.pstr()?;
    r.pstr()?;
    r.pstr()?;
    let package = r.pstr()?;
    let serial = if flag0 {
        let s = r.pstr()?;
        r.u32()?;
        s
    } else {
        String::new()
    };

    let count_pos = r.pos();
    let pin_count = r.u32()?;
    if pin_count > 100_000 {
        return Err(format!(
            "at byte 0x{:x}: too many pins: {}",
            count_pos, pin_count
        ));
    }
    let layer = r.u32()?;
    r.u32()?;

    let mut pins = Vec::with_capacity(pin_count as usize);
    let mut ext_sum: u64 = 0;
    for _ in 0..pin_count {
        let (pin, z2) = load_pin(r, true)?;
        ext_sum += z2 as u64;
        pins.push(pin);
    }

    let pins_ext = if ext_sum > 0 {
        if ext_sum > 100_000 {
            return Err(format!(
                "at byte 0x{:x}: too many extension pins: {}",
                r.pos(),
                               ext_sum
            ));
        }
        r.u32()?;
        r.u32()?;
        let mut ext = Vec::with_capacity(ext_sum as usize);
        for i in 0..ext_sum {
            let (pin, _) = load_pin(r, i != ext_sum - 1)?;
            ext.push(pin);
        }
        Some(ext)
    } else if pin_count > 0 && looks_like_pin_extension(r.data(), r.pos()) {
        r.u32()?;
        r.u32()?;
        let mut ext = Vec::with_capacity(pin_count as usize);
        for i in 0..pin_count {
            let (pin, _) = load_pin(r, i != pin_count - 1)?;
            ext.push(pin);
        }
        Some(ext)
    } else {
        None
    };

    Ok(TvwPart {
        name,
       bbox_min,
       bbox_max,
       x,
       y,
       angle,
       part_type,
       layer,
       value,
       package,
       serial,
       height,
       pins,
       pins_ext,
    })
}

fn looks_like_pin_extension(data: &[u8], pos: usize) -> bool {
    let (flag, reserved, handle, zero, id) = match (
        peek_u32_at(data, pos),
                                                    peek_u32_at(data, pos + 4),
                                                    peek_u32_at(data, pos + 8),
                                                    peek_u32_at(data, pos + 12),
                                                    peek_u32_at(data, pos + 16),
    ) {
        (Some(a), Some(b), Some(c), Some(d), Some(e)) => (a, b, c, d, e),
        _ => return false,
    };
    reserved == 0 && (1..=16).contains(&flag) && zero == 0 && handle != 0 && id <= 100_
}

fn printable_name(s: &str) -> bool {
    let n = s.len();
    (1..=40).contains(&n) && s.bytes().all(is_printable_byte)
}

pub fn looks_like_parts_header(data: &[u8], pos: usize) -> bool {
    let check = || -> Result<bool, String> {
        let mut r = Reader::new(data);
        r.seek(pos)?;
        let count = r.u32()?;
        r.u32()?;
        if !(2..=50_000).contains(&count) {
            return Ok(false);
        }
        for _ in 0..count.min(16) {
            let part = load_part(&mut r)?;
            if !printable_name(&part.name) {
                return Ok(false);
            }
        }
        Ok(true)
    };
    check().unwrap_or(false)
}

pub fn scan_for_parts_section(data: &[u8], from: usize) -> Option<usize> {
    let len = data.len();
    let mut p = from;
    while p + 8 < len {
        let count = peek_u32_at(data, p)?;
        if (2..=50_000).contains(&count) {
            // Cheap check: first part name is a short printable pstr.
            let l = data[p + 8] as usize;
            let start = p + 9;
            if (1..=40).contains(&l)
                && start + l <= len
                && data[start..start + l].iter().all(|&b| is_printable_byte(b))
                && looks_like_parts_header(data, p)
                {
                    return Some(p);
                }
        }
        p += 1;
    }
    None
}

/// Reads the parts section. Returns (parts, warnings). Never fails: problems
/// become warnings and the parts read so far are returned.
pub fn read_parts(r: &mut Reader, probe_region_start: usize) -> (Vec<TvwPart>, Vec<String>) {
let mut parts = Vec::new();
let mut warnings = Vec::new();

if !looks_like_parts_header(r.data(), r.pos()) {
    let expected = r.pos();
    match scan_for_parts_section(r.data(), probe_region_start) {
        Some(p) => {
            if let Err(e) = r.seek(p) {
                warnings.push(format!("could not seek to parts section: {}", e));
                return (parts, warnings);
            }
            warnings.push(format!(
                "parts section not at expected byte 0x{:x}; found by scanning at 0x{:x}",
                expected, p
            ));
        }
        None => {
            warnings.push("could not locate parts section".to_string());
            return (parts, warnings);
        }
    }
}

let count = match r.u32().and_then(|c| r.u32().map(|_| c)) {
    Ok(c) => c,
    Err(e) => {
        warnings.push(format!("could not read parts count: {}", e));
        return (parts, warnings);
    }
};

for i in 0..count {
    match load_part(r) {
        Ok(p) => parts.push(p),
        Err(e) => {
            warnings.push(format!("stopped reading parts at part {} of {}: {}", i, count, e));
            break;
        }
    }
}
(parts, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::tvw::testbuf::Buf;

    /// Writes everything of a normal part up to (and including) the pin-list header.
    fn write_part_head(b: &mut Buf, name: &str, layer: u32, pin_count: u32) {
        b.pstr(name)
        .point(0.0, 0.0) // bbox_min
        .point(10.0, 10.0) // bbox_max
        .point(5.0, 5.0) // position
        .i32(90) // angle
        .u32(0) // decal
        .u32(1) // part_type
        .u32(0)
        .i32(100) // height
        .u8(0) // flag0
        .pstr("10k") // value
        .pstr("")
        .pstr("")
        .pstr("0402") // package
        .u32(pin_count)
        .u32(layer)
        .u32(0);
    }

    fn write_pin(b: &mut Buf, handle: u32, id: u32, name: &str, z2: Option<u32>) {
        b.u32(handle).u32(0).u32(id).pstr(name);
        if let Some(z) = z2 {
            b.u32(z);
        }
    }

    fn write_part(b: &mut Buf, name: &str, layer: u32, pins: &[(u32, u32, &str)]) {
        write_part_head(b, name, layer, pins.len() as u32);
        for &(handle, id, pname) in pins {
            write_pin(b, handle, id, pname, Some(0));
        }
    }

    #[test]
    fn reads_nets() {
        let mut b = Buf::new();
        b.zeros(16).u32(3).u32(3).pstr("GND").pstr("VCC").pstr("");
        let data = b.data.clone();
        let mut r = Reader::new(&data);
        let nets = read_nets(&mut r).unwrap();
        assert_eq!(nets, vec!["GND", "VCC", ""]);
        assert!(r.is_at_end());
    }

    #[test]
    fn scans_for_net_table() {
        let mut b = Buf::new();
        for _ in 0..50 {
            b.u8(0xAB);
        }
        b.u32(250).u32(250);
        for i in 0..250 {
            b.pstr(&format!("NET_{}", i));
        }
        assert_eq!(scan_for_net_table(&b.data, 0), Some((58, 250)));
        assert_eq!(scan_for_net_table(&[0xAB; 100], 0), None);
    }

    #[test]
    fn loads_simple_part() {
        let mut b = Buf::new();
        write_part(&mut b, "C1", 5, &[(8, 1, "1"), (16, 2, "2")]);
        let data = b.data.clone();
        let mut r = Reader::new(&data);
        let p = load_part(&mut r).unwrap();

        assert_eq!(p.name, "C1");
        assert_eq!(p.layer, 5);
        assert_eq!(p.angle, 90);
        assert_eq!(p.height, 1.0);
        assert_eq!(p.value, "10k");
        assert_eq!(p.package, "0402");
        assert_eq!(p.pins.len(), 2);
        assert_eq!((p.pins[0].handle, p.pins[0].id, p.pins[0].name.as_str()), (8, 1, "1"));
        assert_eq!((p.pins[1].handle, p.pins[1].id, p.pins[1].name.as_str()), (16, 2, "2"));
        assert!(p.pins_ext.is_none());
        assert!(r.is_at_end());
    }

    #[test]
    fn loads_ext_contacts() {
    let mut b = Buf::new();
        write_part_head(&mut b, "J1", 5, 2);
        write_pin(&mut b, 8, 1, "1", Some(1));
        write_pin(&mut b, 16, 2, "2", Some(0));
        b.u32(1).u32(0);
        write_pin(&mut b, 24, 3, "1B", None);

        let data = b.data.clone();
        let mut r = Reader::new(&data);
        let p = load_part(&mut r).unwrap();

        assert_eq!(p.pins.len(), 2);
        let ext = p.pins_ext.expect("ext pins");
        assert_eq!(ext.len(), 1);
        assert_eq!((ext[0].handle, ext[0].id, ext[0].name.as_str()), (24, 3, "1B"));
        assert!(r.is_at_end());
    }

    fn three_parts(b: &mut Buf) {
    b.u32(3).u32(0);
        write_part(b, "R1", 5, &[(8, 1, "1"), (16, 2, "2")]);
        write_part(b, "R2", 5, &[(24, 3, "1"), (32, 4, "2")]);
        write_part(b, "R3", 6, &[(40, 5, "1"), (48, 6, "2")]);
    }

    #[test]
    fn read_parts_direct() {
    let mut b = Buf::new();
        three_parts(&mut b);
        let data = b.data.clone();
        let mut r = Reader::new(&data);
        let (parts, warnings) = read_parts(&mut r, 0);

        assert_eq!(parts.len(), 3);
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["R1", "R2", "R3"]);
        assert!(warnings.is_empty(), "{:?}", warnings);
        assert!(r.is_at_end());
    }

    #[test]
    fn read_parts_recovers_by_scanning() {
    let mut b = Buf::new();
        for _ in 0..40 {
            b.u8(0xEE);
    }
    three_parts(&mut b);
        let data = b.data.clone();
        let mut r = Reader::new(&data);
        let (parts, warnings) = read_parts(&mut r, 0);

        assert_eq!(parts.len(), 3);
        assert_eq!(parts[2].name, "R3");
        assert!(!warnings.is_empty());
    }

    const MYSTERIOUS_FIXED_LEN: usize = 4 + 4 + 8 + 4 + 4 + 2 + 2 + 16 + 6 + 16;

    #[test]
    fn mysterious_block_without_trailing_bytes() {
    let mut b = Buf::new();
        b.zeros(MYSTERIOUS_FIXED_LEN).u32(5).u32(0);
        let data = b.data.clone();
        let mut r = Reader::new(&data);
        skip_mysterious_block(&mut r).unwrap();
        assert_eq!(r.pos(), MYSTERIOUS_FIXED_LEN);
        assert_eq!(r.u32().unwrap(), 5);
    }

    #[test]
    fn mysterious_block_with_trailing_bytes() {
    let mut b = Buf::new();
        b.zeros(MYSTERIOUS_FIXED_LEN).u8(0xFF).u8(0xFF).u32(5).u32(0);
        let data = b.data.clone();
        let mut r = Reader::new(&data);
        skip_mysterious_block(&mut r).unwrap();
        assert_eq!(r.pos(), MYSTERIOUS_FIXED_LEN + 2);
        assert_eq!(r.u32().unwrap(), 5);
    }
    }
