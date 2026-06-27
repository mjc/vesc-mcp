//! Regenerate `generated/knowledge_index.json` from catalog YAML.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use vesc_knowledge_index::IndexBuilder;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let catalog_root = manifest_dir.join("../../catalog");
    let refloat_root = resolve_refloat_root(&manifest_dir);
    let out_path = manifest_dir.join("generated/knowledge_index.json");

    let entries = IndexBuilder::build_embedded_index(&catalog_root, &refloat_root)
        .expect("build knowledge index from catalog");
    let json = serde_json::to_string_pretty(&entries).expect("serialize knowledge index");
    fs::write(&out_path, json).expect("write generated/knowledge_index.json");
    eprintln!("wrote {}", out_path.display());
}

fn resolve_refloat_root(manifest_dir: &Path) -> PathBuf {
    if let Ok(path) = env::var("VESC_REFLOAT_ROOT") {
        return PathBuf::from(path);
    }

    let workspace = manifest_dir.join("../..");
    let vendor = workspace.join("vendor/refloat");
    if vendor.is_dir() {
        return vendor;
    }

    PathBuf::from(env::var("HOME").unwrap_or_else(|_| "/".into())).join("projects/refloat")
}
