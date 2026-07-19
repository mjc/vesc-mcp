//! Synthetic catalog validation tests (no sibling repo checkouts required).

use std::fs;
use std::path::PathBuf;

use vesc_mcp_core::catalog::{CatalogValidationError, RepoRoots, validate_catalog_paths};

fn write_minimal_catalog(root: &std::path::Path) {
    fs::create_dir_all(root.join("refloat")).expect("refloat dir");
    fs::write(
        root.join("refloat/commands.yaml"),
        "id: demo-commands\nsource_repo: refloat\nversion: 1\npublic_commands:\n  - name: DEMO\n    path: doc/commands/DEMO.md\n    summary: demo\n",
    )
    .expect("commands yaml");
}

#[test]
fn validate_missing_catalog_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let missing = temp.path().join("nope");
    let roots = RepoRoots {
        refloat: temp.path().join("refloat"),
        vesc: temp.path().join("vesc"),
        poc: temp.path().join("poc"),
        vesc_tool: temp.path().join("vesc_tool"),
        vesc_mcp: temp.path().join("vesc_mcp"),
    };
    let err = validate_catalog_paths(&missing, &roots).expect_err("missing dir");
    assert!(matches!(err, CatalogValidationError::MissingCatalogDir(_)));
}

#[test]
fn validate_missing_repo_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_minimal_catalog(temp.path());
    let roots = RepoRoots {
        refloat: PathBuf::from("/nonexistent/refloat/root"),
        vesc: temp.path().join("vesc"),
        poc: temp.path().join("poc"),
        vesc_tool: temp.path().join("vesc_tool"),
        vesc_mcp: temp.path().join("vesc_mcp"),
    };
    fs::create_dir_all(&roots.vesc).expect("vesc");
    fs::create_dir_all(&roots.poc).expect("poc");
    fs::create_dir_all(&roots.vesc_tool).expect("vesc_tool");
    fs::create_dir_all(&roots.vesc_mcp).expect("vesc_mcp");
    let err = validate_catalog_paths(temp.path(), &roots).expect_err("missing refloat root");
    assert!(matches!(
        err,
        CatalogValidationError::MissingRepoRoot { .. }
    ));
}

#[test]
fn validate_missing_catalog_path_reference() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_minimal_catalog(temp.path());
    let refloat = temp.path().join("refloat_repo");
    fs::create_dir_all(&refloat).expect("refloat repo");
    let roots = RepoRoots {
        refloat,
        vesc: temp.path().join("vesc"),
        poc: temp.path().join("poc"),
        vesc_tool: temp.path().join("vesc_tool"),
        vesc_mcp: temp.path().join("vesc_mcp"),
    };
    fs::create_dir_all(&roots.vesc).expect("vesc");
    fs::create_dir_all(&roots.poc).expect("poc");
    fs::create_dir_all(&roots.vesc_tool).expect("vesc_tool");
    fs::create_dir_all(&roots.vesc_mcp).expect("vesc_mcp");
    // catalog cites doc/commands/DEMO.md but file does not exist under refloat root
    let err = validate_catalog_paths(temp.path(), &roots).expect_err("missing path");
    assert!(matches!(err, CatalogValidationError::MissingPath { .. }));
}
