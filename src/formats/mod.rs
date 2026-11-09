//! Readers for boardview file formats; each produces a crate::model::Board.

pub mod fz;
pub mod tvw;
pub mod gencad;

use std::path::Path;

use crate::model::Board;

/// A supported boardview file format.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Format {
    Fz,
    Tvw,
    Gencad,
}

impl Format {
    /// Short display name, e.g. "FZ".
    pub fn name(&self) -> &'static str {
        match self {
            Format::Fz => "FZ",
            Format::Tvw => "TVW",
            Format::Gencad => "GenCAD",
        }
    }
}

/// Works out the file format, preferring the file's content over its extension.
pub fn detect(path: &Path, data: &[u8]) -> Result<Format, String> {
    // 1. Teboview header text.
    if tvw::is_tvw(data) {
        return Ok(Format::Tvw);
    }

    // 2. GenCAD text: starts with $HEADER and mentions GENCAD.
    if gencad::is_gencad(data) {
        return Ok(Format::Gencad);
    }

    // 3. Unencrypted FZ: 4-byte size, then a zlib header (78 9C or 78 DA).
    if data.len() > 6 && data[4] == 0x78 && (data[5] == 0x9C || data[5] == 0xDA) {
        return Ok(Format::Fz);
    }

    // 4. File extension, case-insensitive.
    let ext = path
    .extension()
    .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match ext.as_deref() {
        Some("fz") => return Ok(Format::Fz),
        Some("tvw") => return Ok(Format::Tvw),
        Some("cad") | Some("gcd") => return Ok(Format::Gencad),
        _ => {}
    }

    // 5. Give up.
    let name = path
    .file_name()
    .map(|n| n.to_string_lossy().into_owned())
    .unwrap_or_else(|| path.display().to_string());
    Err(format!(
        "unknown boardview format: {} (supported: .fz, .tvw, .cad (GenCAD))",
                name
    ))
}

/// Detects the format and parses the file into a Board.
pub fn load(path: &Path, data: &[u8], fz_key: &fz::decrypt::Key) -> Result<(Board, Format), String> {
    let format = detect(path, data)?;
    let board = match format {
        Format::Fz => {
            let unpacked = fz::container::unpack(data, Some(fz_key))?;
            fz::parse::parse(&unpacked.content, &unpacked.descr)
        }
        Format::Tvw => tvw::parse(data)?,
        Format::Gencad => gencad::parse(data)?,
    };
    Ok((board, format))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_tvw_by_content() {
        let text = b"O95w-28ps49m 02v9o.";
        let mut data = vec![text.len() as u8];
        data.extend_from_slice(text);
        data.extend_from_slice(&[0u8; 16]);
        assert_eq!(detect(Path::new("x.bin"), &data), Ok(Format::Tvw));
    }

    #[test]
    fn detects_unencrypted_fz_by_content() {
        let mut data = vec![0u8; 4];
        data.extend_from_slice(&[0x78, 0x9C]);
        data.extend_from_slice(&[0u8; 10]);
        assert_eq!(detect(Path::new("x.bin"), &data), Ok(Format::Fz));
    }

    #[test]
    fn detects_gencad_by_content() {
        let data = b"$HEADER\nGENCAD 1.4\nUNITS INCH\n$ENDHEADER\n$BOARD\n$ENDBOARD\n";
        assert_eq!(detect(Path::new("x.bin"), data), Ok(Format::Gencad));
    }

    #[test]
    fn falls_back_to_extension() {
        let data = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(detect(Path::new("board.FZ"), &data), Ok(Format::Fz));
        assert_eq!(detect(Path::new("board.tvw"), &data), Ok(Format::Tvw));
        assert_eq!(detect(Path::new("board.cad"), &data), Ok(Format::Gencad));
        assert_eq!(detect(Path::new("board.GCD"), &data), Ok(Format::Gencad));

        let err = detect(Path::new("board.xyz"), &data).unwrap_err();
        assert!(err.contains("unknown boardview format"), "{}", err);
        assert!(err.contains("board.xyz"), "{}", err);
        assert!(err.contains(".cad"), "{}", err);
    }

    #[test]
    fn format_names() {
        assert_eq!(Format::Fz.name(), "FZ");
        assert_eq!(Format::Tvw.name(), "TVW");
        assert_eq!(Format::Gencad.name(), "GenCAD");
    }
}
