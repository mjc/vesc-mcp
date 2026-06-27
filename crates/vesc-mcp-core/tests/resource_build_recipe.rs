//! Integration tests for catalog build-recipe MCP resources.

use std::path::PathBuf;

use vesc_mcp_core::resources::{
    POC_RUST_PACKER_URI, REFLOAT_VESC_TOOL_URI, ResourceRegistry, read_build_recipe,
    register_build_recipe_resources,
};

fn catalog_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

#[test]
fn resource_footer_contains_source_path() {
    let body = read_build_recipe(REFLOAT_VESC_TOOL_URI, &catalog_root())
        .unwrap_or_else(|err| panic!("read refloat recipe: {err}"));
    assert!(
        body.contains("Source: refloat/Makefile"),
        "missing source path in footer:\n{body}"
    );
    assert!(
        body.contains("\n---\n"),
        "missing footer separator:\n{body}"
    );
}

#[test]
fn resource_build_recipe_refloat_contains_vesc_tool() {
    let body = read_build_recipe(REFLOAT_VESC_TOOL_URI, &catalog_root())
        .unwrap_or_else(|err| panic!("read refloat recipe: {err}"));
    assert!(
        body.contains("--buildPkgFromDesc"),
        "missing vesc_tool modern build command:\n{body}"
    );
    assert!(
        body.contains("VESC_TOOL") || body.contains("vesc_tool"),
        "missing vesc_tool variable reference:\n{body}"
    );
    assert!(
        body.contains("Source: refloat/"),
        "missing attribution footer:\n{body}"
    );
}

#[test]
fn resource_build_recipe_poc_contains_package_target() {
    let body = read_build_recipe(POC_RUST_PACKER_URI, &catalog_root())
        .unwrap_or_else(|err| panic!("read poc recipe: {err}"));
    assert!(
        body.contains("package"),
        "missing make package target:\n{body}"
    );
    assert!(
        body.contains("vesc-pkg-build") || body.contains("vesc-rust-poc"),
        "missing POC packer info:\n{body}"
    );
    assert!(
        body.contains("Source:"),
        "missing attribution footer:\n{body}"
    );
    assert!(
        body.contains("Source: vesc-rust-poc/docs/package-flow.md#L19"),
        "missing POC doc line anchor in footer:\n{body}"
    );
}

#[test]
fn resource_build_recipe_registers_two_static_resources() {
    let mut registry = ResourceRegistry::new();
    register_build_recipe_resources(&mut registry).expect("register build-recipe resources");
    assert_eq!(registry.list_static().len(), 2);

    let uris: Vec<_> = registry
        .list_static()
        .iter()
        .map(|meta| meta.uri.as_str())
        .collect();
    assert!(uris.contains(&REFLOAT_VESC_TOOL_URI));
    assert!(uris.contains(&POC_RUST_PACKER_URI));
}
