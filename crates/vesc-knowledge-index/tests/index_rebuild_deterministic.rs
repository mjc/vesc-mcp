//! Deterministic index rebuild and integrity tests.

use std::collections::HashSet;
use std::path::PathBuf;

use vesc_knowledge_index::{IndexBuilder, embedded_entries};

fn catalog_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

fn refloat_root() -> PathBuf {
    let vendor = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/refloat");
    if vendor.is_dir() {
        return vendor;
    }
    std::env::var("VESC_REFLOAT_ROOT").map_or_else(
        |_| PathBuf::from(std::env::var("HOME").expect("HOME")).join("projects/refloat"),
        PathBuf::from,
    )
}

#[test]
fn rebuild_deterministic_hash() {
    let rebuilt = IndexBuilder::build_embedded_index(&catalog_root(), &refloat_root())
        .expect("rebuild index from catalog");
    let committed: Vec<vesc_knowledge_index::IndexEntry> =
        serde_json::from_str(include_str!("../generated/knowledge_index.json"))
            .expect("parse committed generated index");

    assert_eq!(
        rebuilt, committed,
        "rebuilt index must match committed snapshot"
    );
}

#[test]
fn index_no_duplicate_ids() {
    let entries = embedded_entries();
    let mut seen = HashSet::new();
    for entry in entries {
        assert!(
            seen.insert(entry.id.clone()),
            "duplicate index id: {}",
            entry.id
        );
    }
}
