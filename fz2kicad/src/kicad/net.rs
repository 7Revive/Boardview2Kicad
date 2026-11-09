use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::footprint::{Library, Placement};
use crate::kicad::sexpr::{list, string, SExpr};
use crate::model::{Board, Part};

/// One pad connected to a net.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Node<'a> {
    refdes: &'a str,
    pin: &'a str,
    name: &'a str,
}

fn placed_parts<'a>(board: &'a Board, lib: &'a Library) -> Vec<(&'a Placement, &'a Part )> {
let parts: HashMap<&str, &Part> = board.parts.iter().map(|p| (p.refdes.as_str(), p)).collect();
let mut seen: HashSet<&str> = HashSet::new();
lib.placements
.iter()
.filter_map(|pl| parts.get(pl.refdes.as_str()).map(|&part| (pl, part)))
.filter(|(pl, _)| seen.insert(pl.refdes.as_str()))
.collect()
}

fn collect_nodes<'a>(placed: &[(&'a Placement, &'a Part)]) -> HashMap<&'a str, Vec<Node<'a>>> {
    let mut nets: HashMap<&str, Vec<Node>> = HashMap::new();
    for (_, part) in placed {
        for pin in &part.pins {
            if pin.net.is_empty() {
                continue;
            }
            nets.entry(pin.net.as_str()).or_default().push(Node {
                refdes: &part.refdes,
                pin: &pin.number,
                name: &pin.name,
            });
        }
    }
    for nodes in nets.values_mut() {
        nodes.sort_by(|a, b| a.refdes.cmp(b.refdes).then(a.pin.cmp(b.pin)));
    }
    nets
}

/// One (comp ...) entry.
fn component(pl: &Placement, part: &Part) -> SExpr {
    let id = pl.footprint_id.to_string();
    let value = part.description.as_deref().unwrap_or(&id);

    let mut items = vec![
        list("ref", vec![string(&part.refdes)]),
                                                                list("value", vec![string(value)]),
                                                                list("footprint", vec![string(&format!("fz_board:{}", id))]),
    ];
    if let Some(pn) = &part.part_number {
        items.push(list(
            "fields",
            vec![list(
                "field",
                vec![list("name", vec![string("Part Number")]), string(pn)],
            )],
        ));
    }
    list("comp", items)
}

/// One (net ...) entry with all its nodes.
fn net(code: usize, name: &str, nodes: &[Node]) -> SExpr {
    let mut items = vec![
        list("code", vec![string(&code.to_string())]),
                                                                list("name", vec![string(name)]),
    ];
    for node in nodes {
        let mut node_items = vec![
            list("ref", vec![string(node.refdes)]),
                                                                list("pin", vec![string(node.pin)]),
        ];
        if !node.name.is_empty() {
            node_items.push(list("pinfunction", vec![string(node.name)]));
        }
        items.push(list("node", node_items));
    }
    list("net", items)
}

/// Build the whole netlist as one S-expression.
pub fn netlist_sexpr(board: &Board, lib: &Library, source: &str) -> SExpr {
    let placed = placed_parts(board, lib);
    let nodes = collect_nodes(&placed);

    // Header: format version and where the data came from.
    let version = list("version", vec![string("E")]);
    let design = list(
        "design",
        vec![
            list("source", vec![string(source)]),
                      list("tool", vec![string("fz2kicad")]),
        ],
    );

    // One component per placed part.
    let components = list(
        "components",
        placed.iter().map(|(pl, part)| component(pl, part)).collect(),
    );

    // One net per board net, numbered like in the .kicad_pcb file.
    let nets = list(
        "nets",
        board
        .net_names()
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let empty = Vec::new();
            let n = nodes.get(name.as_str()).unwrap_or(&empty);
            net(i + 1, name, n)
        })
        .collect(),
    );

    list("export", vec![version, design, components, nets])
}

/// Write the netlist to a .net file.
pub fn write_netlist(
    board: &Board,
    lib: &Library,
    source: &str,
    path: &Path,
) -> Result<(), String> {
    let text = format!("{}\n", netlist_sexpr(board, lib, source).to_pretty());
    fs::write(path, text).map_err(|e| format!("cannot write {}: {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::footprint::build;
    use crate::model::Pin;

    /// Make a part from (number, name, net, x, y) tuples.
    fn part(refdes: &str, part_number: Option<&str>, pins: &[(&str, &str, &str, f64, f64)]) -> Part {
        Part {
            refdes: refdes.to_string(),
            insertion_code: "1".to_string(),
            sym_name: String::new(),
            bottom: false,
            rotation: None,
            part_number: part_number.map(String::from),
            description: None,
            pins: pins
            .iter()
            .map(|&(number, name, net, x, y)| Pin {
            number: number.to_string(),
            name: name.to_string(),
            net: net.to_string(),
            x,
            y,
            radius: 5.0,
                })
            .collect(),
        }
    }

    /// R1 and C1 share GND; R1 pin 2 has no net; C1 pin 1 is named VIN.
    fn test_board() -> Board {
        Board {
            parts: vec![
                part("R1", None, &[("1", "", "GND", 0.0, 0.0), ("2", "", "", 20.0, 0.0)]),
                part("C1", Some("P1"), &[("1", "VIN", "VCC", 100.0, 0.0), ("2", "", "GND", 120.0, 0.0)]),
                ],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn basic_structure() {
        let board = test_board();
        let lib = build(&board);
        let text = netlist_sexpr(&board, &lib, "test.fz").to_pretty();
        assert!(text.contains("(version \"E\")"));
        assert!(text.contains("(footprint \"fz_board:1\")"));
        assert!(text.contains("(code \"1\")"));
    }

    #[test]
    fn shared_net_lists_both_parts_sorted() {
        let board = test_board();
        let lib = build(&board);
        let placed = placed_parts(&board, &lib);
        let nodes = collect_nodes(&placed);
        let gnd: Vec<(&str, &str)> = nodes["GND"].iter().map(|n| (n.refdes, n.pin)).collect();
        assert_eq!(gnd, vec![("C1", "2"), ("R1", "1")]);
    }

    #[test]
    fn pin_without_net_is_not_listed() {
        let board = test_board();
        let lib = build(&board);
        let placed = placed_parts(&board, &lib);
        let nodes = collect_nodes(&placed);
        let found = nodes
        .values()
        .flatten()
        .any(|n| n.refdes == "R1" && n.pin == "2");
        assert!(!found, "R1 pin 2 has no net but was listed");
    }

    #[test]
    fn part_number_field() {
        let board = test_board();
        let lib = build(&board);
        let sexpr = netlist_sexpr(&board, &lib, "test.fz");
        let expected = list(
            "field",
            vec![list("name", vec![string("Part Number")]), string("P1")],
        );
        // Compare the printed text of the field as it appears in the full output.
        // The printer puts nested lists on separate lines, so build the expected
        // text with the same printer and the indentation it has inside the file.
        let text = sexpr.to_pretty();
        let field_text: String = expected
        .to_pretty()
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
        let flat: String = text.lines().map(str::trim).collect::<Vec<_>>().join("\n");
        assert!(flat.contains(&field_text), "missing:\n{}\nin:\n{}", field_text, text);
    }

    #[test]
    fn pin_name_is_pinfunction() {
        let board = test_board();
        let lib = build(&board);
        let text = netlist_sexpr(&board, &lib, "test.fz").to_pretty();
        assert!(text.contains("(pinfunction \"VIN\")"));
    }

    /// Find the (net ...) entry with the given name anywhere in the tree.
    fn find_net<'a>(e: &'a SExpr, name: &str) -> Option<&'a SExpr> {
        let SExpr::List(items) = e else { return None };
        let is_net = matches!(items.first(), Some(SExpr::Atom(a)) if a == "net");
        let has_name = items.iter().any(|c| {
            *c == list("name", vec![string(name)])
        });
        if is_net && has_name {
            return Some(e);
        }
        items.iter().find_map(|c| find_net(c, name))
    }

    /// Run with: cargo test kicad::net::tests::real_file -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_file() {
        let (Ok(content), Ok(descr)) = (
            std::fs::read("out/content.txt"),
                                        std::fs::read("out/descr.txt"),
        ) else {
            println!("out/content.txt or out/descr.txt not found; run `fz2kicad dump` first");
            return;
        };

        let board = crate::fz::parse::parse(&content, &descr);
        let lib = build(&board);
        let path = Path::new("out/fz_board.net");
        write_netlist(&board, &lib, "board.fz", path).expect("writing the netlist failed");

        println!("wrote {}", path.display());
        println!("components: {}", placed_parts(&board, &lib).len());
        println!("nets:       {}", board.net_names().len());

        let sexpr = netlist_sexpr(&board, &lib, "board.fz");
        match find_net(&sexpr, "VGA_XTALIN_R") {
        Some(n) => println!("{}", n),
        None => println!("net VGA_XTALIN_R not found"),
        }
    }
}
