//! One-time exporter for the shipped `packs/default` pack.
//!
//! Generates the same 76 files the game uses at runtime from the built-in
//! procedural art and synthesized sounds, then writes them to the requested
//! folder (default: `packs/default`). Running it twice produces byte-identical
//! output, so a committed folder plus this binary is the drift check between
//! the shipped bytes and the procedural sources.
//!
//! Usage: `cargo run --example export_default_pack -- packs/default`

use std::path::PathBuf;

use opencraft::pack::defaults::{build_default_pack, export_default_pack};

fn main() {
    let dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("packs/default"));

    let file_count = build_default_pack().len();
    match export_default_pack(&dir) {
        Ok(()) => println!("exported {file_count} files to {}", dir.display()),
        Err(err) => {
            eprintln!(
                "failed to export the default pack to {}: {err}",
                dir.display()
            );
            std::process::exit(1);
        }
    }
}
