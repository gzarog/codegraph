//! Print a file's parse tree: `cargo run --example tree -- <language> <file>`.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(lang), Some(file)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: tree <language> <file>");
        std::process::exit(2);
    };
    let lang: codegraph_core::Language = lang.parse().expect("language");
    let src = std::fs::read_to_string(file).expect("file");
    match codegraph_extract::debug_tree(lang, &src) {
        Some(t) => println!("{t}"),
        None => eprintln!("no grammar for {lang:?}"),
    }
}
