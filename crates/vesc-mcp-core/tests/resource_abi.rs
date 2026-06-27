//! Integration tests for catalog ABI MCP resources.

use std::path::PathBuf;

use serde::Deserialize;
use vesc_mcp_core::resources::{
    MINIMAL_TEST_PACKAGE_ABI_URI, ResourceRegistry, read_abi_resource, register_abi_resources,
};

fn catalog_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

#[derive(Debug, Deserialize)]
struct MinimalTestPackageAbiResource {
    id: String,
    source_repo: String,
    sources: Vec<AbiSource>,
    requirements: Vec<AbiRequirement>,
}

#[derive(Debug, Deserialize)]
struct AbiSource {
    path: String,
}

#[derive(Debug, Deserialize)]
struct AbiRequirement {
    name: String,
    kind: String,
    #[allow(dead_code)]
    caller: String,
}

#[test]
fn resource_abi_minimal_json_valid() {
    let body = read_abi_resource(MINIMAL_TEST_PACKAGE_ABI_URI, &catalog_root())
        .unwrap_or_else(|err| panic!("read minimal test package ABI: {err}"));
    let parsed: MinimalTestPackageAbiResource =
        serde_json::from_str(&body).expect("resource returns valid JSON");

    assert_eq!(parsed.id, "minimal-test-package");
    assert_eq!(parsed.source_repo, "vesc-rust-poc");
    assert_eq!(parsed.requirements.len(), 12);
    assert!(
        parsed
            .requirements
            .iter()
            .any(|item| item.name == "lbm_add_extension" && item.kind == "function"),
        "missing lbm_add_extension requirement:\n{body}"
    );
    assert!(
        parsed
            .sources
            .iter()
            .any(|source| source.path.contains("abi_inventory.rs")),
        "missing abi_inventory source path:\n{body}"
    );
}

#[test]
fn resource_abi_registers_static_resource() {
    let mut registry = ResourceRegistry::new();
    register_abi_resources(&mut registry).expect("register abi resources");
    assert_eq!(registry.list_static().len(), 1);

    let meta = registry
        .lookup(MINIMAL_TEST_PACKAGE_ABI_URI)
        .expect("minimal test package ABI registered");
    assert_eq!(meta.mime_type, "application/json");
}
