//! Snapshot tests for the default MCP resource catalog URI list.

use vesc_mcp_core::VescMcpService;

#[test]
fn resources_list_snapshot() {
    let service = VescMcpService::new();
    let uris: Vec<&str> = service
        .resource_registry()
        .list_static()
        .iter()
        .map(|meta| meta.uri.as_str())
        .collect();

    assert!(
        uris.len() >= 8,
        "expected at least 8 registered resources, got {}",
        uris.len()
    );

    insta::assert_json_snapshot!(uris);
}
