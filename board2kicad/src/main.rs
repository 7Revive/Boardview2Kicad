mod formats;
mod model;
mod footprint;
mod kicad;

/// FZ decryption key (44 x 32-bit RC6 round keys), embedded in the program.
const EMBEDDED_KEY: formats::fz::decrypt::Key = [
0x25d8d248, 0xe1502405, 0x56b5d486, 0x69213fe0,
0xa22490ec, 0x01fdd9fa, 0x0681955f, 0x0fac202d,
0xdac9eeb4, 0xf6024aba, 0xcd8b4cc6, 0x9f307c8e,
0x4ab8fad7, 0x232f967d, 0x5e8666a3, 0xde966d4b,
0xc64bfb1c, 0xea7fb092, 0x1a751a7e, 0x37e8f0bc,
0x3359c8f3, 0x969ac22b, 0x610f5804, 0xd99d10e6,
0xc58d54d6, 0x1f9aea8b, 0x8e388c1a, 0xe4f7d2ed,
0x3e5da1f6, 0xedfe818a, 0x7252b016, 0xb503a170,
0xc4128fb6, 0x2c93ceeb, 0x53539a6e, 0xdacf7668,
0x3ab78e52, 0x8ee9d815, 0x7043f799, 0xc6a05dcf,
0x727f1da2, 0x0dfd983b, 0x78c53872, 0x00945692,
];

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use formats::fz::container::{unpack, Unpacked};
use formats::fz::decrypt::{check_key, load_key, Key};

const USAGE: &str = "usage: board2kicad <input_file> <output_dir> [--key <key_file>] [--dump]\n  \
supported input: .fz, .tvw";

/// How many warnings / skipped parts to list before summarising the rest.
const SHOW_MAX: usize = 10;

/// The command-line arguments, after they have been checked.
struct Args {
    /// True with --dump: write the raw FZ blocks instead of converting.
    dump: bool,
    board: PathBuf,
    out_dir: PathBuf,
    key_file: Option<PathBuf>,
}

fn main() -> ExitCode {
    // Collect the arguments, without the program name. args_os() avoids a
    // crash on file names that aren't valid UTF-8.
    let raw: Vec<OsString> = std::env::args_os().skip(1).collect();

    // Wrong shape: show how to use the program.
    let Some(args) = parse_args(&raw) else {
        eprintln!("{}", USAGE);
        return ExitCode::from(2);
    };

    // Convert (default) or dump; any error ends up here.
    let result = if args.dump {
        run_dump(&args)
    } else {
        run_convert(&args)
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {}", message);
            ExitCode::from(1)
        }
    }
}

/// Check that the arguments match
/// `<input_file> <output_dir> [--dump]`.
fn parse_args(raw: &[OsString]) -> Option<Args> {
    let mut positional: Vec<PathBuf> = Vec::new();
    let mut key_file: Option<PathBuf> = None;
    let mut dump = false;

    let mut iter = raw.iter();
    while let Some(arg) = iter.next() {
        let text = arg.to_str().unwrap_or("");
        if text == "--key" {
            // --key only once, and it needs a file name after it.
            if key_file.is_some() {
                return None;
            }
            key_file = Some(PathBuf::from(iter.next()?));
        } else if text == "--dump" {
            // --dump only once.
            if dump {
                return None;
            }
            dump = true;
        } else if text.starts_with("--") {
            return None; // unknown option
        } else {
            positional.push(PathBuf::from(arg));
        }
    }

    // Exactly two plain arguments: the input file and the output folder.
    if positional.len() != 2 {
        return None;
    }
    let out_dir = positional.pop()?;
    let board = positional.pop()?;

    Some(Args {
        dump,
         board,
         out_dir,
         key_file,
    })
}

/// `--dump`: write the two decompressed blocks as they are (FZ files only).
fn run_dump(args: &Args) -> Result<(), String> {
    // Read, decrypt if needed, decompress.
    let unpacked = load(args)?;

    // Write both blocks as raw bytes.
    create_dir(&args.out_dir)?;
    let content_path = args.out_dir.join("content.txt");
    let descr_path = args.out_dir.join("descr.txt");
    write_file(&content_path, &unpacked.content)?;
    write_file(&descr_path, &unpacked.descr)?;

    // Summary.
    let kind = if unpacked.was_encrypted {
        "encrypted file"
    } else {
        "plain zlib file"
    };
    println!(
        "{}: {}, content {} bytes, descr {} bytes",
        args.board.display(),
             kind,
             unpacked.content.len(),
             unpacked.descr.len()
    );
    println!("wrote {} and {}", content_path.display(), descr_path.display());
    Ok(())
}

/// Default: produce a KiCad footprint library, board and netlist.
fn run_convert(args: &Args) -> Result<(), String> {
    // Step 1: read the file, detect its format, and turn it into parts and pins.
    let key = choose_key(args.key_file.as_deref())?;
    let data = read_board(&args.board)?;
    let (board, format) = formats::load(&args.board, &data, &key)?;

    // Step 2: what we found, and any problems while reading it.
    println!("format: {}", format.name());
    println!(
        "{}: {} parts, {} pins, {} nets, {} warnings",
        args.board.display(),
             board.parts.len(),
             board.pin_count(),
             board.net_names().len(),
             board.warnings.len()
    );
    print_some("warning", &board.warnings);

    // Step 3: group parts with identical pad layouts into footprints.
    let lib = footprint::build(&board);
    println!(
        "{} footprints, {} parts skipped (no pins)",
             lib.footprints.len(),
             lib.skipped.len()
    );
    print_some("skipped", &lib.skipped);

    // Step 4: write the KiCad files.
    create_dir(&args.out_dir)?;

    let lib_dir = args.out_dir.join("Board.pretty");
    let count = kicad::fp::write_library(&lib, &lib_dir)?;
    println!("wrote {} ({} footprints)", lib_dir.display(), count);

    let pcb_path = args.out_dir.join("Board.kicad_pcb");
    kicad::pcb::write_board(&board, &lib, &pcb_path)?;
    println!("wrote {}", pcb_path.display());

    // The netlist records where it came from: just the input file's name.
    let source = args
    .board
    .file_name()
    .map(|n| n.to_string_lossy().into_owned())
    .unwrap_or_else(|| args.board.display().to_string());
    let net_path = args.out_dir.join("Board.net");
    kicad::net::write_netlist(&board, &lib, &source, &net_path)?;
    println!("wrote {}", net_path.display());

    // Step 5: tell the user how to use the result.
    println!(
        "done. In KiCad add {} as library nickname Board.",
        lib_dir.display()
    );
    Ok(())
}

/// Used by `--dump`: pick the key, read the file, refuse non-FZ, unpack the FZ container.
fn load(args: &Args) -> Result<Unpacked, String> {
    let key = choose_key(args.key_file.as_deref())?;
    let file = read_board(&args.board)?;
    // Only an explicit TVW detection is refused; anything else is tried as FZ
    // (encrypted FZ files may have any extension).
    if let Ok(formats::Format::Tvw) = formats::detect(&args.board, &file) {
        return Err("--dump only supports .fz files".to_string());
    }
    unpack(&file, Some(&key))
}

/// Read the whole input file, with an error message that names it.
fn read_board(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))
}

/// Decide which key to use.
/// - `--key` given: that file must exist, and is loaded (overrides the built-in key).
/// - not given: use the built-in EMBEDDED_KEY, after checking it for typos.
fn choose_key(key_file: Option<&Path>) -> Result<Key, String> {
    match key_file {
        Some(path) => {
            if !path.exists() {
                return Err(format!("key file {} does not exist", path.display()));
            }
            load_key(path)
        }
        None => {
            check_key(&EMBEDDED_KEY)?;
            Ok(EMBEDDED_KEY)
        }
    }
}

/// Print the first few items of a list, then how many more there are.
fn print_some(label: &str, items: &[String]) {
    for item in items.iter().take(SHOW_MAX) {
        println!("  {}: {}", label, item);
    }
    if items.len() > SHOW_MAX {
        println!("  ... and {} more", items.len() - SHOW_MAX);
    }
}

/// Create a folder (and any missing parent folders).
fn create_dir(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("cannot create folder {}: {}", dir.display(), e))
}

/// Write bytes to a file, with an error message that names the file.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|e| format!("cannot write {}: {}", path.display(), e))
}
