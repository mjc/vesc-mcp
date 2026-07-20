//! Integration tests for MCP resource wiring on [`VescMcpService`].

use vesc_mcp_core::VescMcpService;
use vesc_mcp_core::resources::{
    LISP_IMPORTS_URI, MINIMAL_TEST_PACKAGE_ABI_URI, PKGDESC_DIALECTS_URI,
    REALTIME_DATA_COMMAND_URI, REFLOAT_MINIMAL_MANIFEST_URI, REFLOAT_VESC_TOOL_URI, VESC_C_IF_URI,
    VESC_PKG_LIB_ABI_URI, VESCPACKAGE_REFERENCE_URI,
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
    assert_eq!(uris.len(), 18);
    assert!(uris.contains(&REFLOAT_VESC_TOOL_URI));
    assert!(uris.contains(&PKGDESC_DIALECTS_URI));
    assert!(uris.contains(&VESC_C_IF_URI));
    assert!(uris.contains(&VESC_PKG_LIB_ABI_URI));
    assert!(uris.contains(&LISP_IMPORTS_URI));
    assert!(uris.contains(&VESCPACKAGE_REFERENCE_URI));
    assert!(uris.contains(&MINIMAL_TEST_PACKAGE_ABI_URI));
    assert!(uris.contains(&REALTIME_DATA_COMMAND_URI));
    assert!(uris.contains(&REFLOAT_MINIMAL_MANIFEST_URI));
}

#[test]
fn service_registry_lists_resource_templates() {
    let service = VescMcpService::new();
    let templates = service.resource_registry().list_mcp_templates();
    assert_eq!(templates.len(), 6);
    let template_uris: Vec<_> = templates.iter().map(|t| t.uri_template.as_str()).collect();
    for expected in [
        "vescpkg://manifest/{path}",
        "vesc://catalog/commands/refloat/{command}",
        "vesc://knowledge/chunk/{id}",
        "vesc://knowledge/document/{id}",
        "vesc://knowledge/snapshot/{snapshot}/chunk/{id}",
        "vesc://knowledge/snapshot/{snapshot}/document/{id}",
    ] {
        assert!(template_uris.contains(&expected), "missing {expected}");
    }
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

    let dialects = registry
        .read(PKGDESC_DIALECTS_URI)
        .unwrap_or_else(|err| panic!("read doc topic: {err}"));
    assert!(dialects.contains("pkgName") && dialects.contains("packageName"));

    let abi = registry
        .read(MINIMAL_TEST_PACKAGE_ABI_URI)
        .unwrap_or_else(|err| panic!("read abi resource: {err}"));
    assert!(abi.contains("lbm_add_extension"));

    let realtime = registry
        .read(REALTIME_DATA_COMMAND_URI)
        .unwrap_or_else(|err| panic!("read refloat command: {err}"));
    assert!(!realtime.is_empty());
}
