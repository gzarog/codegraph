//! Self-hosted CodeGraph telemetry endpoint.
//!
//!   codegraph-telemetry-server [--listen 127.0.0.1:8787] [--db telemetry.db]
//!                              [--retention-days 90] [--secure-cookies]
//!
//! Put it behind a TLS-terminating reverse proxy and point clients at it with
//! CODEGRAPH_TELEMETRY_ENDPOINT=https://your-host/v1/events. The dashboard at
//! /dashboard is enabled by setting CODEGRAPH_DASHBOARD_TOKEN.

use codegraph_telemetry::server::{run, Config};
use std::net::TcpListener;
use std::path::PathBuf;

fn main() {
    let mut listen = "127.0.0.1:8787".to_string();
    let mut db = PathBuf::from("telemetry.db");
    let mut retention_days = codegraph_telemetry::db::DEFAULT_RETENTION_DAYS;
    let mut secure_cookies = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--listen" => listen = args.next().unwrap_or(listen),
            "--db" => db = args.next().map(PathBuf::from).unwrap_or(db),
            "--retention-days" => {
                retention_days = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(retention_days)
                    .max(1)
            }
            "--secure-cookies" => secure_cookies = true,
            "-h" | "--help" => {
                println!("usage: codegraph-telemetry-server [--listen ADDR] [--db PATH] [--retention-days N] [--secure-cookies]\n\nCODEGRAPH_DASHBOARD_TOKEN enables the dashboard at /dashboard.");
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let dashboard_token = std::env::var("CODEGRAPH_DASHBOARD_TOKEN")
        .ok()
        .filter(|t| t.len() >= 12);
    if std::env::var("CODEGRAPH_DASHBOARD_TOKEN").is_ok() && dashboard_token.is_none() {
        eprintln!("CODEGRAPH_DASHBOARD_TOKEN must be at least 12 characters; dashboard disabled.");
    }
    let listener = match TcpListener::bind(&listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot listen on {listen}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "codegraph telemetry on http://{listen} (db {}, dashboard {})",
        db.display(),
        if dashboard_token.is_some() {
            "on"
        } else {
            "off"
        }
    );
    let cfg = Config {
        db_path: db,
        retention_days,
        dashboard_token,
        secure_cookies,
        rate_per_minute: 30,
    };
    if let Err(e) = run(listener, cfg) {
        eprintln!("telemetry server failed: {e}");
        std::process::exit(1);
    }
}
