//! Catalog path validation against sibling repository checkouts.

use std::path::PathBuf;

use vesc_mcp_core::{
    RepoRoots, collect_catalog_path_refs, find_duplicate_catalog_ids, validate_catalog_paths,
};

fn catalog_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

#[test]
fn catalog_no_duplicate_ids() {
    let root = catalog_root();
    let duplicates = find_duplicate_catalog_ids(&root).expect("scan catalog");
    assert!(
        duplicates.is_empty(),
        "duplicate catalog ids: {duplicates:?}"
    );
}

#[test]
fn catalog_schema_files_parse() {
    let root = catalog_root();
    let refs = collect_catalog_path_refs(&root).expect("collect paths");
    assert!(!refs.is_empty(), "expected path refs in catalog yaml");
}

#[test]
#[ignore = "requires VESC_REFLOAT_ROOT, VESC_ROOT, VESC_POC_ROOT checkouts"]
fn catalog_paths_exist_when_env_set() {
    let roots = RepoRoots::from_env();
    validate_catalog_paths(&catalog_root(), &roots)
        .expect("all catalog paths should exist under configured repo roots");
}
