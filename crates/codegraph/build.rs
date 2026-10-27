//! Build the browser viewer (crates/codegraph-viewer) for wasm32 and embed
//! it in the binary. Without the `wasm32-unknown-unknown` target (or with
//! `CODEGRAPH_SKIP_VIEWER=1`) an empty module is embedded and `codegraph ui`
//! explains how to get the viewer.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let dest = out.join("codegraph_viewer.wasm");
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let viewer = manifest_dir.join("../codegraph-viewer");
    let workspace = manifest_dir.join("../..");
    println!("cargo:rerun-if-changed={}", viewer.join("src").display());
    println!(
        "cargo:rerun-if-changed={}",
        viewer.join("Cargo.toml").display()
    );
    println!("cargo:rerun-if-env-changed=CODEGRAPH_SKIP_VIEWER");

    let skip = std::env::var("CODEGRAPH_SKIP_VIEWER").is_ok_and(|v| v == "1");
    if skip || !build_viewer(&workspace, &out, &dest) {
        std::fs::write(&dest, b"").expect("write placeholder viewer");
    }
}

/// `<target>/` from `<target>/<profile>/build/<pkg>/out`, so every profile
/// shares one viewer build.
fn target_root(out: &Path) -> PathBuf {
    out.ancestors()
        .nth(4)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| out.to_path_buf())
}

fn build_viewer(workspace: &Path, out: &Path, dest: &Path) -> bool {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let target_dir = target_root(out).join("viewer-wasm");
    let mut cmd = Command::new(cargo);
    cmd.args([
        "build",
        "--release",
        "--locked",
        "--target",
        "wasm32-unknown-unknown",
        "-p",
        "codegraph-viewer",
        "--manifest-path",
    ])
    .arg(workspace.join("Cargo.toml"))
    .env("CARGO_TARGET_DIR", &target_dir);
    // Don't hand the outer build's wrappers and flags (clippy, coverage,
    // host-only RUSTFLAGS) to the wasm build.
    for var in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CLIPPY_ARGS",
        "CARGO_BUILD_TARGET",
        "CARGO_PRIMARY_PACKAGE",
    ] {
        cmd.env_remove(var);
    }
    match cmd.status() {
        Ok(s) if s.success() => {
            let built = target_dir.join("wasm32-unknown-unknown/release/codegraph_viewer.wasm");
            if std::fs::copy(&built, dest).is_ok() {
                return true;
            }
            println!("cargo:warning=viewer built but {} is missing", built.display());
        }
        _ => println!(
            "cargo:warning=the browser viewer was not built (run `rustup target add wasm32-unknown-unknown`); `codegraph ui` will say so"
        ),
    }
    false
}
