//! Print what extraction finds in a file: `cargo run --example extract -- <language> <file>`.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(lang), Some(file)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: extract <language> <file>");
        std::process::exit(2);
    };
    let lang: codegraph_core::Language = lang.parse().expect("language");
    let src = std::fs::read_to_string(file).expect("file");
    let r = codegraph_extract::extract_file(file, &src, lang);
    for n in &r.nodes {
        println!(
            "{:<10} {:<28} {:>4}-{:<4} {}",
            n.kind.as_str(),
            n.qualified_name,
            n.start_line,
            n.end_line,
            n.signature.as_deref().unwrap_or("")
        );
    }
    for u in &r.unresolved_references {
        let from = r
            .nodes
            .iter()
            .find(|n| n.id == u.from_node_id)
            .map_or("?", |n| n.name.as_str());
        println!(
            "  ref {:<12} {:<24} from {from} @{}",
            u.reference_kind.as_str(),
            u.reference_name,
            u.line
        );
    }
    for e in &r.errors {
        println!("  error {}", e.message);
    }
}
