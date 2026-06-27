//! Integration tests for the `run_package_checks` MCP tool.

use serde_json::Value;
use vesc_mcp_core::test_support::{McpTestHarness, TempWorkspace};

#[test]
fn tool_run_checks_rejects_path_outside_env_roots() {
    let harness = McpTestHarness::new();
    let workspace = TempWorkspace::new();
    let response = harness.call_tool(
        "run_package_checks",
        serde_json::json!({ "root": workspace.root.to_string_lossy() }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], false, "response: {body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|err| err.contains("VESC_PACKAGE_ROOTS")),
        "response: {body}"
    );
}
