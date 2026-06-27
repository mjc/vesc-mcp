//! Index builder tests for catalog priorities.json.

use std::path::PathBuf;

use vesc_knowledge_index::{Category, IndexBuilder};

fn repo_catalog_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

#[test]
fn index_builder_parses_priorities() {
    let entries = IndexBuilder::parse_priorities(&repo_catalog_root()).expect("parse priorities");
    assert!(entries.len() >= 10);

    let build_pkg = entries
        .iter()
        .find(|entry| entry.id == "priority.refloat-build-pkgdesc")
        .expect("refloat-build-pkgdesc priority");
    assert_eq!(build_pkg.category, Category::PackageBuild);
    assert!(
        build_pkg.summary.contains("buildPkgFromDesc"),
        "summary: {}",
        build_pkg.summary
    );
}
