//! Tiny little-endian byte writer for building fake Teboview (.tvw) data in tests.
//!
//! Every method appends to the buffer and returns `&mut Self` so calls chain:
//!
//! ```ignore
//! let mut b = Buf::new();
//! b.u8(7).u32(1).fixed(123.45).pstr("GND");
//! ```

#[derive(Default)]
pub struct Buf {
    pub data: Vec<u8>,
}

impl Buf {
    pub fn new() -> Self {
        Buf { data: Vec::new() }
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.data.push(v);
        self
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.data.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.data.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn i16(&mut self, v: i16) -> &mut Self {
        self.data.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.data.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// Writes a value in mils as an i32 in hundredths of a mil.
    pub fn fixed(&mut self, mils: f64) -> &mut Self {
        self.i32((mils * 100.0).round() as i32)
    }

    /// Writes two fixed values (x, y) in mils.
    pub fn point(&mut self, x: f64, y: f64) -> &mut Self {
        self.fixed(x).fixed(y)
    }

    /// Pascal string: 1 length byte, then the UTF-8 bytes. Panics if longer than 255 b
    pub fn pstr(&mut self, s: &str) -> &mut Self {
        let raw = s.as_bytes();
        assert!(
            raw.len() <= 255,
                "pstr too long: {} bytes (max 255)",
                raw.len()
        );
        self.data.push(raw.len() as u8);
        self.data.extend_from_slice(raw);
        self
    }

    pub fn zeros(&mut self, n: usize) -> &mut Self {
        self.data.resize(self.data.len() + n, 0);
        self
    }

    pub fn bytes(&mut self) -> &[u8] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::tvw::reader::Reader;

    #[test]
    fn round_trip_through_reader() {
        let mut b = Buf::new();
        b.u8(7).u32(1).fixed(123.45).pstr("GND");

        let mut r = Reader::new(b.bytes());
        assert_eq!(r.u8().unwrap(), 7);
        assert_eq!(r.u32().unwrap(), 1);
        assert_eq!(r.fixed().unwrap(), 123.45);
        assert_eq!(r.pstr().unwrap(), "GND");
        assert!(r.is_at_end());
    }
}
