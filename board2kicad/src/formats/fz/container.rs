//! The outer layer of an ASUS .fz boardview file.
//!
//! After decryption (n = file size in bytes):
//!
//! ```text
//!  offset 0      4                          n-L+4                 n-4      n
//!         +------+--------------------------+---------------------+--------+
//!         | ???? | zlib stream: content     | zlib stream: descr  |   L    |
//!         +------+--------------------------+---------------------+--------+
//!          4 unknown bytes                                         u32, little-endian
//! ```
//!
//! - content starts at offset 4; descr starts at offset n - L + 4.
//! - content: the parts and pins tables, as '!'-separated text.
//! - descr: the bill of materials (part numbers and descriptions), as tab-separated text.

use std::io::Read;

use flate2::read::ZlibDecoder;

use super::decrypt::{decrypt, Key};

/// The two decompressed blocks of a .fz file.
#[derive(Debug)]
pub struct Unpacked {
    /// Parts and pins text.
    pub content: Vec<u8>,
    /// Part numbers and descriptions text.
    pub descr: Vec<u8>,
    /// True if the file had to be decrypted first.
    pub was_encrypted: bool,
}

/// Does a zlib stream start at offset 4?
/// zlib streams start with 0x78 followed by 0x9C (default compression)
/// or 0xDA (best compression). Encrypted data almost never looks like this.
fn has_zlib_header_at_4(data: &[u8]) -> bool {
    data.len() > 6 && data[4] == 0x78 && (data[5] == 0x9C || data[5] == 0xDA)
}

/// Decrypt the file if needed, then decompress both blocks.
pub fn unpack(file: &[u8], key: Option<&Key>) -> Result<Unpacked, String> {
    // A real .fz file is much bigger than this. Anything smaller can't hold
    // two zlib streams plus the header and trailer.
    if file.len() < 16 {
        return Err("file is too small to be a .fz file".to_string());
    }

    // Work on our own copy so we can decrypt it in place.
    let mut data = file.to_vec();

    // If there's no zlib header where we expect one, the file is encrypted.
    let was_encrypted = !has_zlib_header_at_4(&data);

    if was_encrypted {
        let key = key.ok_or_else(|| {
            "this file is encrypted and no key was found \
(use --key or ~/.config/fz2kicad/key)"
.to_string()
        })?;

        decrypt(key, &mut data);

        // A correct key gives a valid zlib header. Anything else means
        // the key is wrong or this isn't a .fz file.
        if !has_zlib_header_at_4(&data) {
            return Err(
                "decryption produced garbage: wrong key, or not an ASUS .fz file".to_string(),
            );
        }
    }

    let n = data.len();

    // The last 4 bytes give the distance from the end of the file
    // back to the descr block (L).
    let l = u32::from_le_bytes([data[n - 4], data[n - 3], data[n - 2], data[n - 1]]) as usize;
    if l < 8 || l > n {
        return Err(format!(
            "invalid descr block length {} in the last 4 bytes (file is {} bytes)",
                           l, n
        ));
    }

    // Work out where the descr block starts and check that it is inside the file.
    let descr_start = n - l + 4;
    if descr_start <= 4 || descr_start >= n {
        return Err(format!(
            "descr block would start at offset {}, which is outside the file ({} bytes)",
                           descr_start, n
        ));
    }

    // Give each stream everything up to the end of the file. That's
    // deliberate: zlib knows where its own stream ends and ignores the rest.
    let content = inflate(&data[4..], "content")?;
    let descr = inflate(&data[descr_start..], "descr")?;

    Ok(Unpacked {
        content,
       descr,
       was_encrypted,
    })
}

/// Decompress one zlib stream. If the stream is damaged partway through,
/// keep what was decompressed so far and print a warning.
fn inflate(input: &[u8], what: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut decoder = ZlibDecoder::new(input);

    match decoder.read_to_end(&mut out) {
        Ok(_) => Ok(out),
        // read_to_end keeps the bytes it read before the error.
        Err(e) if !out.is_empty() => {
            eprintln!(
                "warning: {} block is damaged ({}); kept {} bytes",
                      what,
                      e,
                      out.len()
            );
            Ok(out)
        }
        Err(e) => Err(format!("cannot decompress {} block: {}", what, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::fz::decrypt::{encrypt, test_key};
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    const CONTENT: &[u8] =b"A!REFDES!COMP_INSERTION_CODE!SYM_NAME!SYM_MIRROR!SYM_ROTATE!\r\nS!U1!1!BGA!NO!0!\r\n";
    const DESCR: &[u8] = b"Board\r\nPARTNUMBER\tDESCRIPTION\tQTY\tLOCATION\tPARTNUMBER2";

    /// Compress bytes with zlib.
    fn zlib(data: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

/// Build an unencrypted .fz file using the layout described at the top.
fn fake_fz(content: &[u8], descr: &[u8]) -> Vec<u8> {
let zc = zlib(content);
let zd = zlib(descr);
let mut file = vec![0u8; 4];
file.extend_from_slice(&zc);
file.extend_from_slice(&zd);
file.extend_from_slice(&(zd.len() as u32 + 8).to_le_bytes());
file
}

#[test]
fn plain_file_without_key() {
let file = fake_fz(CONTENT, DESCR);
let u = unpack(&file, None).expect("plain file should unpack");
assert!(!u.was_encrypted);
assert_eq!(u.content, CONTENT);
assert_eq!(u.descr, DESCR);
}

#[test]
fn encrypted_file_with_right_key() {
let key = test_key();
let mut file = fake_fz(CONTENT, DESCR);
encrypt(&key, &mut file);

let u = unpack(&file, Some(&key)).expect("encrypted file should unpack");
assert!(u.was_encrypted);
assert_eq!(u.content, CONTENT);
assert_eq!(u.descr, DESCR);
}

#[test]
fn encrypted_file_without_key() {
let mut file = fake_fz(CONTENT, DESCR);
encrypt(&test_key(), &mut file);

let err = unpack(&file, None).unwrap_err();
assert!(err.contains("no key"), "unexpected message: {}", err);
}

#[test]
fn encrypted_file_with_wrong_key() {
let key = test_key();
let mut file = fake_fz(CONTENT, DESCR);
encrypt(&key, &mut file);

// Flipping two bits keeps the parity valid, so this key passes
// check_key but still decrypts to garbage.
let mut wrong = key;
wrong[10] ^= 0b11;

let err = unpack(&file, Some(&wrong)).unwrap_err();
assert!(err.contains("wrong key"), "unexpected message: {}", err);
}

#[test]
fn tiny_file_is_rejected() {
let err = unpack(&[0u8; 10], None).unwrap_err();
assert!(err.contains("too small"), "unexpected message: {}", err);
}
}
