//! Integration tests for catalog build-recipe MCP resources.

use std::path::PathBuf;

use vesc_mcp_core::resources::{
    REFLOAT_VESC_TOOL_URI, ResourceRegistry, read_build_recipe, register_build_recipe_resources,
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
fn resource_build_recipe_registers_one_static_resource() {
    let mut registry = ResourceRegistry::new();
    register_build_recipe_resources(&mut registry).expect("register build-recipe resources");
    assert_eq!(registry.list_static().len(), 1);
    assert!(
        registry
            .list_static()
            .iter()
            .any(|meta| meta.uri == REFLOAT_VESC_TOOL_URI)
    );
}
