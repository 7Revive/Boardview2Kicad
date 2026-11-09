//! Decryption for ASUS .fz boardview files.
//!
//! The files are encrypted with the RC6 block cipher in 8-bit CFB
//! ("cipher feedback") mode. The decryption key is a list of 44 32-bit
//! words, which the user supplies in a text file.

use std::path::Path;

/// The RC6 key: 44 words of 32 bits each (20 rounds need 2*20 + 4 words).
pub type Key = [u32; 44];

/// Expected parity of each key word.
/// For word i, `(word.count_ones() % 2 == 0) as u8` must equal PARITY[i].
/// This lets us catch typos when someone copies the key by hand.
const PARITY: [u8; 44] = [
    0, 1, 1, 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 1, 1, 0, 1, 1, 0, 1, 0, 0, //
0, 1, 1, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 1, 1, 0, 1,
];

// ---------------------------------------------------------------------------
// PART 1: the key
// ---------------------------------------------------------------------------

/// Read a key file from disk and turn it into a Key.
pub fn load_key(path: &Path) -> Result<Key, String> {
    // Read the whole file as text.
    let text = std::fs::read_to_string(path)
    .map_err(|e| format!("could not read key file '{}': {}", path.display(), e))?;

    // Parse it, and add the file name to any error so the user knows
    // which file is wrong.
    parse_key(&text).map_err(|e| format!("key file '{}': {}", path.display(), e))
}

/// Turn the text of a key file into a Key.
pub fn parse_key(text: &str) -> Result<Key, String> {
    let mut words: Vec<u32> = Vec::with_capacity(44);

    for line in text.lines() {
        // Skip comment lines (first non-space character is '#').
        if line.trim_start().starts_with('#') {
            continue;
        }

        // Split the line on whitespace and commas, ignoring empty pieces.
        for token in line.split(|c: char| c.is_whitespace() || c == ',') {
            if token.is_empty() {
                continue;
            }

            // Drop an optional "0x" / "0X" prefix.
            let hex = token
            .strip_prefix("0x")
            .or_else(|| token.strip_prefix("0X"))
            .unwrap_or(token);

            // Make sure only hex digits are left (this also rejects things
            // like "+1", which Rust's parser would otherwise accept).
            if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!("'{}' is not a hexadecimal number", token));
            }

            // Convert the hex text into a number. This only fails if the
            // number is too big for 32 bits.
            let value = u32::from_str_radix(hex, 16).map_err(|_| {
                format!("'{}' is too large for a 32-bit key word (max 8 hex digits)", token)
            })?;

            words.push(value);
        }
    }

    // The key must have exactly 44 words.
    if words.len() != 44 {
        return Err(format!(
            "expected 44 key words but found {}",
            words.len()
        ));
    }

    // Move the list into a fixed-size array.
    let mut key: Key = [0; 44];
    key.copy_from_slice(&words);

    // Catch typos before using the key.
    check_key(&key)?;
    Ok(key)
}

/// Check every key word against the expected parity table.
pub fn check_key(key: &Key) -> Result<(), String> {
    for (i, &word) in key.iter().enumerate() {
        // 1 if the word has an even number of 1-bits, 0 if odd.
        let parity = (word.count_ones() % 2 == 0) as u8;
        if parity != PARITY[i] {
            return Err(format!(
                "word {} (0x{:08x}) fails the parity check, probably a typo",
                               i + 1,
                               word
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// PART 2: RC6 decryption in 8-bit CFB mode
// ---------------------------------------------------------------------------

/// Run the 16-byte register through RC6 encryption and return one byte
/// of the result. That byte is the "mask" used to hide or reveal one
/// byte of the file.
fn mask_byte(key: &Key, register: &[u8; 16]) -> u8 {
    // Read 4 bytes starting at `at` as a little-endian 32-bit number.
    let word = |at: usize| {
        u32::from_le_bytes([
            register[at],
            register[at + 1],
            register[at + 2],
            register[at + 3],
        ])
    };

    // Split the register into four 32-bit words.
    let mut a = word(0);
    let mut b = word(4);
    let mut c = word(8);
    let mut d = word(12);

    // Mix in the first two key words.
    b = b.wrapping_add(key[0]);
    d = d.wrapping_add(key[1]);

    // 20 rounds of scrambling.
    for i in 1..=20 {
        // Make two values from b and d that depend on all their bits.
        let t = b.wrapping_mul(b.wrapping_mul(2).wrapping_add(1)).rotate_left(5);
        let u = d.wrapping_mul(d.wrapping_mul(2).wrapping_add(1)).rotate_left(5);

        // Use them to scramble a and c, and mix in two more key words.
        a = (a ^ t).rotate_left(u).wrapping_add(key[2 * i]);
        c = (c ^ u).rotate_left(t).wrapping_add(key[2 * i + 1]);

        // Rotate the four words so each gets its turn in each position.
        let old_a = a;
        a = b;
        b = c;
        c = d;
        d = old_a;
    }

    // Final key mixing (only `a` is needed, because we only use its low byte).
    a = a.wrapping_add(key[42]);

    // Keep only the lowest 8 bits.
    a as u8
}

/// Decrypt `data` in place.
pub fn decrypt(key: &Key, data: &mut [u8]) {
    // The register holds the last 16 encrypted bytes; it starts as zeros.
    let mut register = [0u8; 16];

    for byte in data.iter_mut() {
        // Remember the encrypted byte before we overwrite it.
        let encrypted = *byte;

        // Reveal the original byte by XOR-ing it with the mask.
        *byte = encrypted ^ mask_byte(key, &register);

        // Slide the register one byte left and add the encrypted byte at the end.
        register.copy_within(1.., 0);
        register[15] = encrypted;
    }
}

/// Encrypt `data` in place (the reverse of `decrypt`; only used by tests).
#[cfg(test)]
pub fn encrypt(key: &Key, data: &mut [u8]) {
    let mut register = [0u8; 16];

    for byte in data.iter_mut() {
        // Hide the byte by XOR-ing it with the mask.
        *byte ^= mask_byte(key, &register);

        // Feed the NEW (encrypted) byte into the register, just as
        // decryption will.
        register.copy_within(1.., 0);
        register[15] = *byte;
    }
}

// ---------------------------------------------------------------------------
// PART 3: test helpers and tests
// ---------------------------------------------------------------------------

/// A made-up key that passes the parity check. It is NOT a real key.
#[cfg(test)]
pub fn test_key() -> Key {
    let mut key: Key = [0; 44];
    for i in 0..44 {
        let mut w = (i as u32 + 1).wrapping_mul(0x9E37_79B9) ^ 0x5BD1_E995;
        // Flipping one bit changes the parity, so this always fixes it.
        if (w.count_ones() % 2 == 0) as u8 != PARITY[i] {
            w ^= 0x8000_0000;
        }
        key[i] = w;
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_accepts_valid_text() {
        let key = test_key();

        // Write the key as text: a comment line, then 4 words per line.
        let mut text = String::from("# test key, not a real one\n");
        for (i, w) in key.iter().enumerate() {
            text.push_str(&format!("0x{:08x}", w));
            text.push(if i % 4 == 3 { '\n' } else { ' ' });
        }

        let parsed = parse_key(&text).expect("valid key text should parse");
        assert_eq!(parsed, key);
    }

    #[test]
    fn parse_key_rejects_wrong_count() {
        let err = parse_key("0x1 0x2").unwrap_err();
        assert!(err.contains("found 2"), "unexpected message: {}", err);
    }

    #[test]
    fn parse_key_rejects_non_hex() {
        let err = parse_key("0xZZ").unwrap_err();
        assert!(err.contains("0xZZ"), "unexpected message: {}", err);
    }

    #[test]
    fn check_key_rejects_flipped_bit() {
        let mut key = test_key();
        key[5] ^= 1; // one flipped bit breaks the parity of word 6
        let err = check_key(&key).unwrap_err();
        assert!(err.contains("word 6"), "unexpected message: {}", err);
    }

    #[test]
    fn encrypt_then_decrypt_roundtrip() {
        let key = test_key();
        let original: Vec<u8> = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();

        let mut data = original.clone();
        encrypt(&key, &mut data);
        assert_ne!(data, original, "encryption should change the data");

        decrypt(&key, &mut data);
        assert_eq!(data, original, "decryption should restore the data");
    }

    #[test]
        fn known_answer() {
            // These expected bytes were computed by an independent RC6
            // implementation (8-bit CFB, zero starting register, using the
            // 44 words of test_key() directly as the expanded key).
            // The roundtrip test only proves that decrypt undoes encrypt, which
            // would still pass if mask_byte were wrong in the same way for both.
            // Comparing against outside values proves mask_byte computes real RC6.
            let mut data = [0u8; 16];
            encrypt(&test_key(), &mut data);
            assert_eq!(
                data,
                [
                    0xd9, 0x78, 0x2a, 0xb2, 0xc5, 0x82, 0xa5, 0x6f, //
                    0xe6, 0x92, 0xe0, 0x2a, 0xcf, 0x46, 0xd5, 0xcd,
                ]
            );
        }
}
