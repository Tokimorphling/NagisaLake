//! Guards the `embed-web` feature so a missing frontend build fails with an
//! actionable message instead of a `rust-embed` macro error, and re-runs the
//! build when the compiled frontend changes.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var_os("CARGO_FEATURE_EMBED_WEB").is_none() {
        return;
    }

    // apps/nagisalake-hub -> repository root -> web/dist
    let dist = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../web/dist")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/dist"));

    if !dist.join("index.html").is_file() {
        println!(
            "cargo:warning=embed-web is enabled but {} is missing",
            dist.display()
        );
        panic!(
            "feature `embed-web` requires a compiled frontend at web/dist.\n\
             Build it first:\n\
             \n    cd web && pnpm install && pnpm build\n\n\
             Then rebuild the Hub with --features embed-web."
        );
    }

    // Rebuild when any emitted asset changes. Watching the directory alone does
    // not catch edits to files inside it on every platform.
    println!("cargo:rerun-if-changed={}", dist.display());
    if let Ok(entries) = std::fs::read_dir(dist.join("assets")) {
        for entry in entries.flatten() {
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }
    println!(
        "cargo:rerun-if-changed={}",
        dist.join("index.html").display()
    );
}
