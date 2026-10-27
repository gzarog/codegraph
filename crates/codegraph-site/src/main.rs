//! Build (or preview) the documentation website.
//!
//!   codegraph-site build [--site DIR] [--out DIR] [--base /codegraph]
//!   codegraph-site serve [--site DIR] [--port 4321]

use codegraph_http::{Request, Response};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

fn serve_file(root: &Path, req: &Request) -> Response {
    let rel = req.path.trim_start_matches('/');
    if Path::new(rel)
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
        && !rel.is_empty()
    {
        return Response::text(404, "not found\n");
    }
    let mut path = root.join(rel);
    if path.is_dir() {
        path = path.join("index.html");
    }
    match std::fs::read(&path) {
        Ok(bytes) => Response::new(200, content_type(&path), bytes),
        Err(_) => Response::new(
            404,
            "text/html; charset=utf-8",
            std::fs::read(root.join("404.html")).unwrap_or_default(),
        ),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "build".into());
    let mut site = codegraph_site::default_site_dir();
    let mut out: Option<PathBuf> = None;
    let mut base = String::from("/codegraph");
    let mut port: u16 = 4321;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--site" => site = args.next().map(PathBuf::from).unwrap_or(site),
            "--out" => out = args.next().map(PathBuf::from),
            "--base" => base = args.next().unwrap_or(base),
            "--port" => port = args.next().and_then(|p| p.parse().ok()).unwrap_or(port),
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    match cmd.as_str() {
        "build" => {
            let out = out.unwrap_or_else(|| site.join("dist"));
            match codegraph_site::build(&site, &out, &base) {
                Ok(r) => println!("Built {} pages into {}", r.pages, r.out.display()),
                Err(e) => {
                    eprintln!("site build failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "serve" => {
            let out = out.unwrap_or_else(|| site.join("dist"));
            // Previews are served at the root.
            if let Err(e) = codegraph_site::build(&site, &out, "") {
                eprintln!("site build failed: {e}");
                std::process::exit(1);
            }
            let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("cannot listen on port {port}: {e}");
                    std::process::exit(1);
                }
            };
            println!("Docs preview at http://127.0.0.1:{port}/ (Ctrl-C to stop)");
            let root = Arc::new(out);
            codegraph_http::serve(
                listener,
                0,
                Arc::new(move |req: Request| serve_file(&root, &req)),
            );
        }
        other => {
            eprintln!("unknown command: {other} (use build or serve)");
            std::process::exit(2);
        }
    }
}
