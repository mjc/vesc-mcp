//! Index builder tests for catalog-backed minimal native-lib ABI inventory parsing.

use std::path::PathBuf;

use vesc_knowledge_index::{Category, IndexBuilder};

fn repo_catalog_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

#[test]
fn index_builder_parses_abi_inventory() {
    let entries =
        IndexBuilder::parse_abi_inventory(&repo_catalog_root()).expect("parse ABI catalog");

    assert_eq!(
        entries.len(),
        12,
        "expected 12 minimal-test-package ABI symbols"
    );

    for entry in &entries {
        assert_eq!(entry.category, Category::PocAbi);
        assert!(
            entry.id.starts_with("poc_abi."),
            "unexpected id prefix: {}",
            entry.id
        );
    }

    let lbm_add_ext = entries
        .iter()
        .find(|entry| entry.name == "lbm_add_extension")
        .expect("lbm_add_extension entry");
    assert_eq!(lbm_add_ext.id, "poc_abi.lbm_add_extension");
    assert_eq!(lbm_add_ext.source.repo, "vesc-mcp");
    assert!(
        lbm_add_ext.source.path.contains("vesc-pkg-lib-abi.md"),
        "expected vesc-pkg-lib-abi.md source path, got {}",
        lbm_add_ext.source.path
    );
    assert!(lbm_add_ext.summary.contains("LispBM extensions"));
    assert!(lbm_add_ext.keywords.iter().any(|kw| kw == "function"));
    assert!(
        lbm_add_ext
            .keywords
            .iter()
            .any(|kw| kw == "minimal-test-package")
    );

    let vesc_if_handler = entries
        .iter()
        .find(|entry| entry.name == "VESC_IF.set_app_data_handler")
        .expect("VESC_IF.set_app_data_handler entry");
    assert_eq!(vesc_if_handler.id, "poc_abi.VESC_IF.set_app_data_handler");
    assert!(vesc_if_handler.keywords.iter().any(|kw| kw == "function"));
}

#[test]
fn index_abi_entry_has_in_repo_source() {
    let entries =
        IndexBuilder::parse_abi_inventory(&repo_catalog_root()).expect("parse ABI catalog");

    for entry in &entries {
        assert_eq!(entry.source.repo, "vesc-mcp");
        assert!(
            entry.source.path.contains("vesc-pkg-lib-abi.md"),
            "{} missing vesc-pkg-lib-abi.md source path",
            entry.id
        );
        assert_eq!(entry.source.line, 94);
    }
}
