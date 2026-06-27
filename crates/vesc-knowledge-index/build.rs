//! Emit `OUT_DIR/index.json` from the committed generated index snapshot.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let catalog_root = manifest_dir.join("../../catalog");
    let generated = manifest_dir.join("generated/knowledge_index.json");

    println!("cargo:rerun-if-changed={}", catalog_root.display());
    println!("cargo:rerun-if-changed={}", generated.display());

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let json = include_str!("generated/knowledge_index.json");
    fs::write(out_dir.join("index.json"), json).expect("write embedded index.json");
}
