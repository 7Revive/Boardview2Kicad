//! Little-endian binary cursor for the Teboview (.tvw) boardview format.
//!
//! All reads advance the cursor. Every error message starts with the byte
//! position (in hex) where the problem happened.

pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

#[allow(clippy::len_without_is_empty)]
impl<'a> Reader<'a> {
    pub fn data(&self) -> &'a [u8] {
        self.data
    }
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn is_at_end(&self) -> bool {
        self.pos >= self.data.len()
    }

    pub fn seek(&mut self, pos: usize) -> Result<(), String> {
        if pos > self.data.len() {
            return Err(format!(
                "at byte 0x{:x}: cannot seek to 0x{:x}, file is only {} bytes",
                self.pos,
                pos,
                self.data.len()
            ));
        }
        self.pos = pos;
        Ok(())
    }

    pub fn skip(&mut self, n: usize) -> Result<(), String> {
        self.take(n).map(|_| ())
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        self.take(n)
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    pub fn i16(&mut self) -> Result<i16, String> {
        Ok(i16::from_le_bytes(self.array::<2>()?))
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.array::<4>()?))
    }

    pub fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.array::<4>()?))
    }

    pub fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.array::<4>()?))
    }

    /// Reads a u32 at the current position without moving the cursor.
    pub fn peek_u32(&self) -> Result<u32, String> {
        let mut tmp = Reader {
            data: self.data,
            pos: self.pos,
        };
        tmp.u32()
    }

    /// Reads an i32 in hundredths of a mil and returns mils.
    pub fn fixed(&mut self) -> Result<f64, String> {
        Ok(self.i32()? as f64 / 100.0)
    }

    /// Reads two fixed values (x, y) in mils.
    pub fn point(&mut self) -> Result<(f64, f64), String> {
        let x = self.fixed()?;
        let y = self.fixed()?;
        Ok((x, y))
    }

    /// Pascal string: one length byte, then that many bytes.
    /// UTF-8 if valid, otherwise each byte is treated as Latin-1.
    pub fn pstr(&mut self) -> Result<String, String> {
        let n = self.u8()? as usize;
        let raw = self.take(n)?;
        match std::str::from_utf8(raw) {
            Ok(s) => Ok(s.to_string()),
            Err(_) => Ok(raw.iter().map(|&b| b as char).collect()),
        }
    }

    /// Reads a u32 count and rejects values above `max`.
    pub fn count(&mut self, max: u32, what: &str) -> Result<usize, String> {
        let start = self.pos;
        let n = self.u32()?;
        if n > max {
            return Err(format!(
                "at byte 0x{:x}: too many {}: {} (max {})",
                               start, what, n, max
            ));
        }
        Ok(n as usize)
    }

    /// Position of the first occurrence of `pattern` at or after `from`.
    pub fn find(&self, pattern: &[u8], from: usize) -> Option<usize> {
        if from > self.data.len() {
            return None;
        }
        if pattern.is_empty() {
            return Some(from);
        }
        self.data[from..]
        .windows(pattern.len())
        .position(|w| w == pattern)
        .map(|i| i + from)
    }

    // ---- private helpers ----

    /// Takes `n` bytes and advances, or returns a "need N bytes" error.
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let left = self.remaining();
        if n > left {
            return Err(format!(
                "at byte 0x{:x}: need {} bytes, only {} left",
                self.pos, n, left
            ));
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    /// Takes exactly N bytes as a fixed-size array (built on `take`).
    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_primitives_and_advances() {
        let mut v: Vec<u8> = vec![0x7F];
        v.extend_from_slice(&(-2i16).to_le_bytes());
        v.extend_from_slice(&0xDEADBEEFu32.to_le_bytes());
        v.extend_from_slice(&(-1i32).to_le_bytes());
        v.extend_from_slice(&1.5f32.to_le_bytes());
        assert_eq!(&v[1..3], &[0xFE, 0xFF]);

        let mut r = Reader::new(&v);
        assert_eq!(r.len(), 15);
        assert_eq!(r.u8().unwrap(), 0x7F);
        assert_eq!(r.pos(), 1);
        assert_eq!(r.i16().unwrap(), -2);
        assert_eq!(r.pos(), 3);
        assert_eq!(r.u32().unwrap(), 0xDEADBEEF);
        assert_eq!(r.pos(), 7);
        assert_eq!(r.i32().unwrap(), -1);
        assert_eq!(r.pos(), 11);
        assert_eq!(r.f32().unwrap(), 1.5);
        assert_eq!(r.pos(), 15);
        assert_eq!(r.remaining(), 0);
        assert!(r.is_at_end());
    }

    #[test]
    fn fixed_and_point() {
        let mut v = Vec::new();
        v.extend_from_slice(&12345600i32.to_le_bytes());
        v.extend_from_slice(&100i32.to_le_bytes());
        v.extend_from_slice(&(-250i32).to_le_bytes());

        let mut r = Reader::new(&v);
        assert_eq!(r.fixed().unwrap(), 123456.0);
        assert_eq!(r.point().unwrap(), (1.0, -2.5));
        assert!(r.is_at_end());
    }

    #[test]
    fn pascal_strings() {
        let d = [3u8, b'G', b'N', b'D'];
        assert_eq!(Reader::new(&d).pstr().unwrap(), "GND");

        let d = [0u8];
        let mut r = Reader::new(&d);
        assert_eq!(r.pstr().unwrap(), "");
        assert_eq!(r.pos(), 1);

        let d = [2u8, 0xE9, 0x41];
        assert_eq!(Reader::new(&d).pstr().unwrap(), "éA");
    }

    #[test]
    fn read_past_end_errors() {
        let d = [1u8, 2];
        let mut r = Reader::new(&d);
        let err = r.u32().unwrap_err();
        assert!(err.contains("0x"), "{}", err);
        assert!(err.contains("need"), "{}", err);
        assert_eq!(r.pos(), 0, "failed read must not move the cursor");

        let d = [5u8, b'a'];
        let err = Reader::new(&d).pstr().unwrap_err();
        assert!(err.contains("0x") && err.contains("need"), "{}", err);
    }

    #[test]
    fn count_limits() {
        let d = 5u32.to_le_bytes();
        assert_eq!(Reader::new(&d).count(10, "parts").unwrap(), 5);

        let d = 50u32.to_le_bytes();
        let err = Reader::new(&d).count(10, "parts").unwrap_err();
        assert!(err.contains("too many"), "{}", err);
        assert!(err.contains("parts"), "{}", err);
    }

    #[test]
    fn find_pattern() {
        let d = b"xxxxProbeDB#yyyyProbeDB#";
        let r = Reader::new(d);
        assert_eq!(r.find(b"ProbeDB#", 0), Some(4));
        assert_eq!(r.find(b"ProbeDB#", 5), Some(16));
        assert_eq!(r.find(b"Missing", 0), None);
        assert_eq!(r.find(b"ProbeDB#", 100), None);
    }

    #[test]
    fn seek_and_peek() {
        let d = 0x01020304u32.to_le_bytes();
        let mut r = Reader::new(&d);

        assert_eq!(r.peek_u32().unwrap(), 0x01020304);
        assert_eq!(r.pos(), 0);
        assert_eq!(r.u32().unwrap(), 0x01020304);

        assert!(r.seek(4).is_ok());
        assert!(r.seek(5).is_err());
        assert!(r.seek(2).is_ok());
        assert_eq!(r.pos(), 2);
        assert!(r.peek_u32().is_err());
        assert_eq!(r.pos(), 2);
    }
}
