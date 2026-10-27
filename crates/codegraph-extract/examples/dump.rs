//! Print what extraction produces for a file: `cargo run --example dump -- <path> [language]`.
fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: dump <path> [language]");
    let content = std::fs::read_to_string(&path).expect("read file");
    let language = match args.next() {
        Some(l) => l.parse().expect("language"),
        None => codegraph_core::detect_language(&path, Some(&content)),
    };
    let r = codegraph_extract::extract_file(&path, &content, language);
    println!("{}", serde_json::to_string_pretty(&r).unwrap());
}
