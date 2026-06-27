//! Integration tests for MCP resource wiring on [`VescMcpService`].

use vesc_mcp_core::VescMcpService;
use vesc_mcp_core::resources::{
    POC_RUST_PACKER_URI, REFLOAT_MINIMAL_MANIFEST_URI, REFLOAT_VESC_TOOL_URI,
};

#[test]
fn service_registry_lists_default_static_resources() {
    let service = VescMcpService::new();
    let uris: Vec<_> = service
        .resource_registry()
        .list_static()
        .iter()
        .map(|meta| meta.uri.as_str())
        .collect();
    assert_eq!(uris.len(), 4);
    assert!(uris.contains(&REFLOAT_VESC_TOOL_URI));
    assert!(uris.contains(&POC_RUST_PACKER_URI));
    assert!(uris.contains(&REFLOAT_MINIMAL_MANIFEST_URI));
}

#[test]
fn service_registry_lists_manifest_template() {
    let service = VescMcpService::new();
    let templates = service.resource_registry().list_mcp_templates();
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].uri_template, "vescpkg://manifest/{path}");
}

#[test]
fn service_registry_reads_build_recipe_and_manifest() {
    let service = VescMcpService::new();
    let registry = service.resource_registry();

    let recipe = registry
        .read(REFLOAT_VESC_TOOL_URI)
        .unwrap_or_else(|err| panic!("read build recipe: {err}"));
    assert!(recipe.contains("--buildPkgFromDesc"));

    let manifest = registry
        .read(REFLOAT_MINIMAL_MANIFEST_URI)
        .unwrap_or_else(|err| panic!("read manifest: {err}"));
    assert!(manifest.contains("Refloat Minimal"));
}
