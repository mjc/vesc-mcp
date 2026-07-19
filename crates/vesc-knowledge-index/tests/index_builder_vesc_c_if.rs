//! Index builder tests for catalog-backed `vesc_c_if` parsing.

use std::path::PathBuf;

use vesc_knowledge_index::{Category, IndexBuilder};

fn repo_catalog_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

#[test]
fn index_builder_parses_catalog_vesc_c_if_groups() {
    let entries = IndexBuilder::parse_vesc_c_if_groups(&repo_catalog_root())
        .expect("parse vesc_c_if catalog");

    let lbm_core: Vec<_> = entries
        .iter()
        .filter(|entry| entry.keywords.iter().any(|keyword| keyword == "lbm_core"))
        .collect();
    assert!(
        lbm_core.len() >= 30,
        "expected >=30 lbm_core entries, got {}",
        lbm_core.len()
    );

    let add_ext = entries
        .iter()
        .find(|entry| entry.name == "lbm_add_extension")
        .expect("lbm_add_extension entry");
    assert_eq!(add_ext.id, "vesc_c_if.lbm_add_extension");
    assert_eq!(add_ext.category, Category::FirmwareApi);
    assert_eq!(add_ext.source.repo, "vesc");
    assert_eq!(add_ext.source.path, "lispBM/c_libs/vesc_c_if.h");
    assert_eq!(add_ext.source.line, 324);
    assert!(add_ext.summary.contains("Primary extension registration"));

    for group in ["lbm_core", "lbm_symbols", "os", "comm", "nvm"] {
        assert!(
            entries
                .iter()
                .any(|entry| entry.keywords.contains(&group.to_string())),
            "missing group {group}"
        );
    }

    assert_eq!(
        entries.len(),
        lbm_core.len()
            + entries
                .iter()
                .filter(|entry| entry
                    .keywords
                    .iter()
                    .any(|keyword| keyword == "lbm_symbols"))
                .count()
            + entries
                .iter()
                .filter(|entry| entry.keywords.iter().any(|keyword| keyword == "os"))
                .count()
            + entries
                .iter()
                .filter(|entry| entry.keywords.iter().any(|keyword| keyword == "comm"))
                .count()
            + entries
                .iter()
                .filter(|entry| entry.keywords.iter().any(|keyword| keyword == "nvm"))
                .count()
    );
}

#[test]
fn index_builder_validates_header_when_vesc_root_set() {
    let Some(vesc_root) = std::env::var("VESC_ROOT").ok().map(PathBuf::from) else {
        eprintln!("skip: VESC_ROOT unset");
        return;
    };

    IndexBuilder::parse_vesc_c_if_groups_validated(&repo_catalog_root(), Some(&vesc_root))
        .expect("validated parse against upstream header");
}
