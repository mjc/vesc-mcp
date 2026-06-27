//! Integration test for the `list_vesc_packages` MCP tool.

use serde_json::Value;
use vesc_mcp_core::test_support::{McpTestHarness, fixture_path};

#[test]
fn tool_list_finds_pkgdesc_in_fixture() {
    let harness = McpTestHarness::new();
    let root = fixture_path("refloat-minimal");
    let response = harness.call_tool(
        "list_vesc_packages",
        serde_json::json!({ "roots": [root.to_string_lossy()] }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], true, "response: {body}");

    let packages = body["packages"].as_array().expect("packages array");
    assert!(
        !packages.is_empty(),
        "expected at least one package under refloat-minimal"
    );

    let entry = &packages[0];
    assert_eq!(entry["dialect"], "vesc_tool");
    assert!(
        entry["pkgdesc_path"]
            .as_str()
            .is_some_and(|path| path.ends_with("pkgdesc.qml")),
        "entry: {entry}"
    );
    assert!(
        entry["root"]
            .as_str()
            .is_some_and(|path| path.contains("refloat-minimal")),
        "entry: {entry}"
    );
}
